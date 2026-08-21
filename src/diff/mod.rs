//! Checksummed, resource-bounded keyed dataset diffing.

mod partition;
mod sink;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use arrow::array::Array;
use arrow::datatypes::Schema;
use arrow::record_batch::{RecordBatch, RecordBatchReader};
use serde::Serialize;
use tempfile::TempDir;

use crate::{
    CancellationToken, ChangedRow, DiffReport, MemoryReservation, ProofFrameError, ResourceAccount,
    ResourceLimits, TempReservation,
};
use partition::{
    PARTITIONS, PartitionLimits, PartitionReader, PartitionWriter, RowEntry, WRITER_BUFFER_BYTES,
};
use sink::{AtomicSink, DiffEvent};

const MAX_PARTITION_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const LEDGER_CHUNK_BYTES: u64 = 1024 * 1024;

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_partition_bytes(input: &[u8]) -> Result<(), ProofFrameError> {
    partition::fuzz_bytes(input)
}

#[derive(Debug, Clone)]
pub enum DiffOutput {
    JsonLines(PathBuf),
    ArrowIpc(PathBuf),
}

/// Policy for exact-state overflow during a keyed diff.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum SpillPolicy {
    /// Use the in-memory fast path when exact size hints are safely below the budget, otherwise
    /// fall back to checksummed temporary partitions.
    #[default]
    Auto,
    /// Never create data partitions; fail closed if the in-memory exact state exceeds its budget.
    Never,
}

impl SpillPolicy {
    pub fn from_name(value: &str) -> Result<Self, ProofFrameError> {
        match value {
            "auto" => Ok(Self::Auto),
            "never" => Ok(Self::Never),
            other => Err(ProofFrameError::InvalidContract(format!(
                "Unsupported spill policy `{other}`; expected `auto` or `never`"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiffOptions {
    pub resources: ResourceLimits,
    pub max_samples: usize,
    pub output: Option<DiffOutput>,
    pub cancellation: CancellationToken,
    /// Exact row counts supplied without consuming either stream.
    pub row_count_hints: Option<(u64, u64)>,
    /// Combined logical input bytes supplied by a trusted container adapter.
    pub input_bytes_hint: Option<u64>,
    pub spill: SpillPolicy,
}

impl Default for DiffOptions {
    fn default() -> Self {
        let resources = ResourceLimits::default();
        Self {
            max_samples: resources.max_samples,
            resources,
            output: None,
            cancellation: CancellationToken::new(),
            row_count_hints: None,
            input_bytes_hint: None,
            spill: SpillPolicy::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DiffMetrics {
    pub partitions: usize,
    pub temp_bytes: u64,
    pub peak_memory_bytes: u64,
    pub peak_temp_bytes: u64,
    pub output_records: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ColumnSchema {
    name: String,
    data_type: String,
    nullable: bool,
}

struct PartitionedRows {
    signature: Vec<ColumnSchema>,
    schema_digest: [u8; 32],
    rows: usize,
    paths: Vec<PathBuf>,
}

struct InMemoryRows {
    signature: Vec<ColumnSchema>,
    rows: usize,
    entries: BTreeMap<Vec<u8>, RowEntry>,
    _memory: MemoryLedger,
}

pub fn diff_readers_with_options<B, A>(
    before: B,
    after: A,
    keys: &[String],
    options: &DiffOptions,
) -> Result<DiffReport, ProofFrameError>
where
    B: RecordBatchReader,
    A: RecordBatchReader,
{
    if keys.is_empty() {
        return Err(ProofFrameError::NoKeyColumns);
    }
    options.cancellation.check()?;
    if should_use_in_memory(&before, &after, options) {
        return diff_readers_in_memory(before, after, keys, options);
    }
    let directory = TempDir::new()?;
    let account = ResourceAccount::root(options.resources);
    let mut temp = TempLedger::new(account.clone());
    let before = partition_rows(
        before,
        keys,
        directory.path(),
        "before",
        &account,
        &mut temp,
        &options.cancellation,
    )?;
    let after = partition_rows(
        after,
        keys,
        directory.path(),
        "after",
        &account,
        &mut temp,
        &options.cancellation,
    )?;
    if before.signature != after.signature {
        return Err(ProofFrameError::SchemaMismatch(
            "normalize columns before row-level diff".to_string(),
        ));
    }

    let sample_limit = options.max_samples.min(options.resources.max_samples);
    let column_names = before
        .signature
        .iter()
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();
    let mut accumulator = Accumulator::new(
        keys.to_vec(),
        before.rows,
        after.rows,
        sample_limit,
        options.output.as_ref(),
        &account,
    )?;
    let limits = PartitionLimits {
        max_record_bytes: MAX_PARTITION_RECORD_BYTES.min(options.resources.max_memory_bytes),
        max_columns: u32::try_from(column_names.len())
            .map_err(|_| ProofFrameError::CorruptData("Diff schema exceeds u32 columns".into()))?,
    };
    for (before_path, after_path) in before.paths.iter().zip(&after.paths) {
        options.cancellation.check()?;
        process_partition(
            before_path,
            after_path,
            before.schema_digest,
            limits,
            &column_names,
            &account,
            &mut temp,
            &mut accumulator,
            &options.cancellation,
        )?;
    }
    let metrics = DiffMetrics {
        partitions: PARTITIONS,
        temp_bytes: temp.used,
        peak_memory_bytes: account.peak_memory_used(),
        peak_temp_bytes: account.peak_temp_used(),
        output_records: accumulator.output_records,
    };
    accumulator.finish(metrics, &mut temp)
}

fn should_use_in_memory<B, A>(before: &B, after: &A, options: &DiffOptions) -> bool
where
    B: RecordBatchReader,
    A: RecordBatchReader,
{
    if options.spill == SpillPolicy::Never {
        return true;
    }
    let (Some((before_rows, after_rows)), Some(input_bytes)) =
        (options.row_count_hints, options.input_bytes_hint)
    else {
        return false;
    };
    let rows = before_rows.saturating_add(after_rows);
    let columns = before
        .schema()
        .fields()
        .len()
        .max(after.schema().fields().len()) as u64;
    let per_row = 256_u64.saturating_add(columns.saturating_mul(32));
    let estimate = input_bytes
        .saturating_mul(2)
        .saturating_add(rows.saturating_mul(per_row));
    estimate <= options.resources.max_memory_bytes.saturating_mul(3) / 4
}

fn diff_readers_in_memory<B, A>(
    before: B,
    after: A,
    keys: &[String],
    options: &DiffOptions,
) -> Result<DiffReport, ProofFrameError>
where
    B: RecordBatchReader,
    A: RecordBatchReader,
{
    let account = ResourceAccount::root(options.resources);
    let before = collect_rows_in_memory(before, keys, &account, &options.cancellation)?;
    let after = collect_rows_in_memory(after, keys, &account, &options.cancellation)?;
    if before.signature != after.signature {
        return Err(ProofFrameError::SchemaMismatch(
            "normalize columns before row-level diff".to_string(),
        ));
    }
    let sample_limit = options.max_samples.min(options.resources.max_samples);
    let column_names = before
        .signature
        .iter()
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();
    let mut temp = TempLedger::new(account.clone());
    let mut accumulator = Accumulator::new(
        keys.to_vec(),
        before.rows,
        after.rows,
        sample_limit,
        options.output.as_ref(),
        &account,
    )?;
    for (key, after_entry) in &after.entries {
        options.cancellation.check()?;
        if let Some(before_entry) = before.entries.get(key) {
            if before_entry.hash != after_entry.hash {
                let columns = if accumulator.needs_changed_detail() {
                    before_entry
                        .values
                        .iter()
                        .zip(&after_entry.values)
                        .zip(&column_names)
                        .filter(|((before, after), _)| before != after)
                        .map(|(_, name)| name.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                accumulator.changed(after_entry.display_key.clone(), columns, &mut temp)?;
            }
        } else {
            accumulator.added(after_entry.display_key.clone(), &mut temp)?;
        }
    }
    for (key, before_entry) in &before.entries {
        options.cancellation.check()?;
        if !after.entries.contains_key(key) {
            accumulator.removed(before_entry.display_key.clone(), &mut temp)?;
        }
    }
    let metrics = DiffMetrics {
        partitions: 0,
        temp_bytes: temp.used,
        peak_memory_bytes: account.peak_memory_used(),
        peak_temp_bytes: account.peak_temp_used(),
        output_records: accumulator.output_records,
    };
    accumulator.finish(metrics, &mut temp)
}

fn collect_rows_in_memory<R: RecordBatchReader>(
    mut reader: R,
    keys: &[String],
    account: &ResourceAccount,
    cancellation: &CancellationToken,
) -> Result<InMemoryRows, ProofFrameError> {
    let schema = reader.schema();
    let key_indexes = keys
        .iter()
        .map(|key| {
            schema
                .index_of(key)
                .map_err(|_| ProofFrameError::MissingColumn(key.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signature = schema_signature(schema.as_ref());
    let mut entries = BTreeMap::new();
    let mut memory = MemoryLedger::new(account.child(options_memory_cap(account), 0));
    let mut rows = 0_usize;
    for maybe_batch in &mut reader {
        cancellation.check()?;
        let batch = maybe_batch?;
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ProofFrameError::SchemaMismatch(
                "record batch schema changed during in-memory diff".to_string(),
            ));
        }
        for row in 0..batch.num_rows() {
            let (key, display_key) = diff_key(&batch, &key_indexes, row)?;
            let values = batch
                .columns()
                .iter()
                .map(|array| {
                    if array.is_null(row) {
                        Ok(None)
                    } else {
                        crate::canonical_value_bytes(array.as_ref(), row).map(Some)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let entry = RowEntry {
                display_key,
                hash: row_hash(&values),
                values,
            };
            memory.reserve_additional(estimated_entry_bytes(&key, &entry))?;
            let duplicate_key = entry.display_key.clone();
            if entries.insert(key, entry).is_some() {
                return Err(ProofFrameError::DuplicateKey(duplicate_key));
            }
            rows = rows
                .checked_add(1)
                .ok_or_else(|| ProofFrameError::CorruptData("Diff row count overflowed".into()))?;
        }
    }
    Ok(InMemoryRows {
        signature,
        rows,
        entries,
        _memory: memory,
    })
}

fn partition_rows<R: RecordBatchReader>(
    reader: R,
    keys: &[String],
    directory: &Path,
    prefix: &str,
    account: &ResourceAccount,
    temp: &mut TempLedger,
    cancellation: &CancellationToken,
) -> Result<PartitionedRows, ProofFrameError> {
    let schema = reader.schema();
    let key_indexes = keys
        .iter()
        .map(|key| {
            schema
                .index_of(key)
                .map_err(|_| ProofFrameError::MissingColumn(key.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signature = schema_signature(schema.as_ref());
    let schema_digest = signature_digest(&signature);
    let paths = (0..PARTITIONS)
        .map(|partition| directory.join(format!("{prefix}-{partition}.pfpart")))
        .collect::<Vec<_>>();
    let header_bytes = (PARTITIONS as u64)
        .checked_mul(92)
        .ok_or_else(|| ProofFrameError::CorruptData("Diff header budget overflowed".into()))?;
    temp.reserve_additional(header_bytes)?;
    let _writer_memory =
        account.try_reserve_memory((PARTITIONS * (WRITER_BUFFER_BYTES + 1024)) as u64)?;
    let mut writers = paths
        .iter()
        .map(|path| PartitionWriter::create(path, schema_digest))
        .collect::<Result<Vec<_>, _>>()?;
    let mut row_count = 0_usize;
    for batch in reader {
        cancellation.check()?;
        let batch = batch?;
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ProofFrameError::SchemaMismatch(
                "record batch schema changed during diff partitioning".to_string(),
            ));
        }
        for row in 0..batch.num_rows() {
            let (key, display_key) = diff_key(&batch, &key_indexes, row)?;
            let values = batch
                .columns()
                .iter()
                .map(|array| {
                    if array.is_null(row) {
                        Ok(None)
                    } else {
                        crate::canonical_value_bytes(array.as_ref(), row).map(Some)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let hash = row_hash(&values);
            let partition = row_partition(&key);
            writers[partition].write_record(
                &key,
                &RowEntry {
                    display_key,
                    values,
                    hash,
                },
                MAX_PARTITION_RECORD_BYTES.min(account.limits().max_memory_bytes),
                |bytes| account.try_reserve_memory(bytes),
                |bytes| temp.reserve_additional(bytes),
            )?;
            row_count = row_count
                .checked_add(1)
                .ok_or_else(|| ProofFrameError::CorruptData("Diff row count overflowed".into()))?;
        }
    }
    for writer in writers {
        writer.finish()?;
    }
    Ok(PartitionedRows {
        signature,
        schema_digest,
        rows: row_count,
        paths,
    })
}

#[allow(clippy::too_many_arguments)]
fn process_partition(
    before_path: &Path,
    after_path: &Path,
    schema_digest: [u8; 32],
    limits: PartitionLimits,
    column_names: &[String],
    account: &ResourceAccount,
    temp: &mut TempLedger,
    accumulator: &mut Accumulator,
    cancellation: &CancellationToken,
) -> Result<(), ProofFrameError> {
    let partition_account = account.child(options_memory_cap(account), 0);
    let mut memory = MemoryLedger::new(partition_account);
    let mut before_rows = BTreeMap::new();
    let mut reader = PartitionReader::open(before_path, schema_digest, limits)?;
    while let Some((key, entry)) = reader.next()? {
        memory.reserve_additional(estimated_entry_bytes(&key, &entry))?;
        let display_key = entry.display_key.clone();
        if before_rows.insert(key, entry).is_some() {
            return Err(ProofFrameError::DuplicateKey(display_key));
        }
    }

    let mut seen_after = BTreeSet::new();
    let mut reader = PartitionReader::open(after_path, schema_digest, limits)?;
    while let Some((key, entry)) = reader.next()? {
        memory.reserve_additional(key.len() as u64 + 64)?;
        if !seen_after.insert(key.clone()) {
            return Err(ProofFrameError::DuplicateKey(entry.display_key));
        }
        if let Some(before_entry) = before_rows.get(&key) {
            if before_entry.hash != entry.hash {
                let needs_columns = accumulator.needs_changed_detail();
                let columns = if needs_columns {
                    before_entry
                        .values
                        .iter()
                        .zip(&entry.values)
                        .zip(column_names)
                        .filter(|((before, after), _)| before != after)
                        .map(|(_, name)| name.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                accumulator.changed(entry.display_key, columns, temp)?;
            }
        } else {
            accumulator.added(entry.display_key, temp)?;
        }
    }
    for (key, entry) in before_rows {
        cancellation.check()?;
        if !seen_after.contains(&key) {
            accumulator.removed(entry.display_key, temp)?;
        }
    }
    Ok(())
}

struct Accumulator {
    keys: Vec<String>,
    before_rows: usize,
    after_rows: usize,
    sample_limit: usize,
    added_count: usize,
    removed_count: usize,
    changed_count: usize,
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<ChangedRow>,
    sink: Option<AtomicSink>,
    output_limit: u64,
    output_records: u64,
    sample_account: ResourceAccount,
    added_memory: Vec<MemoryReservation>,
    removed_memory: Vec<MemoryReservation>,
    changed_memory: Vec<MemoryReservation>,
}

impl Accumulator {
    fn new(
        keys: Vec<String>,
        before_rows: usize,
        after_rows: usize,
        sample_limit: usize,
        output: Option<&DiffOutput>,
        account: &ResourceAccount,
    ) -> Result<Self, ProofFrameError> {
        Ok(Self {
            keys,
            before_rows,
            after_rows,
            sample_limit,
            added_count: 0,
            removed_count: 0,
            changed_count: 0,
            added: Vec::with_capacity(sample_limit.min(1024)),
            removed: Vec::with_capacity(sample_limit.min(1024)),
            changed: Vec::with_capacity(sample_limit.min(1024)),
            sink: output
                .map(|output| AtomicSink::new(output, account))
                .transpose()?,
            output_limit: account.limits().max_output_records,
            output_records: 0,
            sample_account: account.clone(),
            added_memory: Vec::with_capacity(sample_limit.min(1024)),
            removed_memory: Vec::with_capacity(sample_limit.min(1024)),
            changed_memory: Vec::with_capacity(sample_limit.min(1024)),
        })
    }

    fn needs_changed_detail(&self) -> bool {
        self.sink.is_some() || self.sample_limit != 0
    }

    fn added(&mut self, key: String, temp: &mut TempLedger) -> Result<(), ProofFrameError> {
        self.added_count += 1;
        self.emit("added", &key, None, temp)?;
        if should_retain_string(&self.added, &key, self.sample_limit) {
            let reservation = self
                .sample_account
                .try_reserve_memory(estimated_string_sample_bytes(&key))?;
            retain_string(
                &mut self.added,
                &mut self.added_memory,
                key,
                reservation,
                self.sample_limit,
            );
        }
        Ok(())
    }

    fn removed(&mut self, key: String, temp: &mut TempLedger) -> Result<(), ProofFrameError> {
        self.removed_count += 1;
        self.emit("removed", &key, None, temp)?;
        if should_retain_string(&self.removed, &key, self.sample_limit) {
            let reservation = self
                .sample_account
                .try_reserve_memory(estimated_string_sample_bytes(&key))?;
            retain_string(
                &mut self.removed,
                &mut self.removed_memory,
                key,
                reservation,
                self.sample_limit,
            );
        }
        Ok(())
    }

    fn changed(
        &mut self,
        key: String,
        columns: Vec<String>,
        temp: &mut TempLedger,
    ) -> Result<(), ProofFrameError> {
        self.changed_count += 1;
        self.emit("changed", &key, Some(&columns), temp)?;
        if should_retain_changed(&self.changed, &key, self.sample_limit) {
            let value = ChangedRow { key, columns };
            let reservation = self
                .sample_account
                .try_reserve_memory(estimated_changed_sample_bytes(&value))?;
            retain_changed(
                &mut self.changed,
                &mut self.changed_memory,
                value,
                reservation,
                self.sample_limit,
            );
        }
        Ok(())
    }

    fn emit(
        &mut self,
        kind: &'static str,
        key: &str,
        columns: Option<&[String]>,
        temp: &mut TempLedger,
    ) -> Result<(), ProofFrameError> {
        let Some(sink) = self.sink.as_mut() else {
            return Ok(());
        };
        if self.output_records >= self.output_limit {
            return Err(ProofFrameError::ResourceLimit {
                resource: "diff output records",
                requested: 1,
                used: self.output_records,
                limit: self.output_limit,
            });
        }
        sink.write(&DiffEvent { kind, key, columns }, |bytes| {
            temp.reserve_additional(bytes)
        })?;
        self.output_records += 1;
        Ok(())
    }

    fn finish(
        mut self,
        metrics: DiffMetrics,
        _temp: &mut TempLedger,
    ) -> Result<DiffReport, ProofFrameError> {
        if let Some(sink) = self.sink.take() {
            sink.finish()?;
        }
        let retained = self.added.len() + self.removed.len() + self.changed.len();
        let total = self.added_count + self.removed_count + self.changed_count;
        Ok(DiffReport {
            keys: self.keys,
            before_rows: self.before_rows,
            after_rows: self.after_rows,
            added_count: self.added_count,
            removed_count: self.removed_count,
            changed_count: self.changed_count,
            added_keys: self.added,
            removed_keys: self.removed,
            changed: self.changed,
            truncated: total > retained,
            metrics,
        })
    }
}

fn retain_string(
    samples: &mut Vec<String>,
    reservations: &mut Vec<MemoryReservation>,
    value: String,
    reservation: MemoryReservation,
    limit: usize,
) {
    let position = samples
        .binary_search(&value)
        .unwrap_or_else(|position| position);
    samples.insert(position, value);
    reservations.insert(position, reservation);
    if samples.len() > limit {
        samples.pop();
        reservations.pop();
    }
}

fn retain_changed(
    samples: &mut Vec<ChangedRow>,
    reservations: &mut Vec<MemoryReservation>,
    value: ChangedRow,
    reservation: MemoryReservation,
    limit: usize,
) {
    let position = samples
        .binary_search_by(|item| item.key.cmp(&value.key))
        .unwrap_or_else(|position| position);
    samples.insert(position, value);
    reservations.insert(position, reservation);
    if samples.len() > limit {
        samples.pop();
        reservations.pop();
    }
}

fn should_retain_string(samples: &[String], value: &str, limit: usize) -> bool {
    limit != 0
        && (samples.len() < limit || samples.last().is_some_and(|last| value < last.as_str()))
}

fn should_retain_changed(samples: &[ChangedRow], key: &str, limit: usize) -> bool {
    limit != 0
        && (samples.len() < limit || samples.last().is_some_and(|last| key < last.key.as_str()))
}

fn estimated_string_sample_bytes(value: &String) -> u64 {
    (std::mem::size_of::<String>() + std::mem::size_of::<MemoryReservation>() + value.capacity())
        as u64
}

fn estimated_changed_sample_bytes(value: &ChangedRow) -> u64 {
    let columns = value
        .columns
        .iter()
        .map(|column| std::mem::size_of::<String>() + column.capacity())
        .sum::<usize>();
    (std::mem::size_of::<ChangedRow>()
        + std::mem::size_of::<MemoryReservation>()
        + value.key.capacity()
        + columns) as u64
}

fn schema_signature(schema: &Schema) -> Vec<ColumnSchema> {
    schema
        .fields()
        .iter()
        .map(|field| ColumnSchema {
            name: field.name().clone(),
            data_type: field.data_type().to_string(),
            nullable: field.is_nullable(),
        })
        .collect()
}

fn signature_digest(signature: &[ColumnSchema]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proofframe:diff-schema:v2\0");
    for field in signature {
        hash_bytes(&mut hasher, field.name.as_bytes());
        hash_bytes(&mut hasher, field.data_type.as_bytes());
        hasher.update(&[u8::from(field.nullable)]);
    }
    *hasher.finalize().as_bytes()
}

fn diff_key(
    batch: &RecordBatch,
    key_indexes: &[usize],
    row: usize,
) -> Result<(Vec<u8>, String), ProofFrameError> {
    let mut canonical = Vec::new();
    let mut display = Vec::with_capacity(key_indexes.len());
    for index in key_indexes {
        let array = batch.column(*index);
        let value = crate::canonical_value_bytes(array.as_ref(), row)?;
        canonical.extend_from_slice(&(*index as u64).to_le_bytes());
        canonical.extend_from_slice(&(value.len() as u64).to_le_bytes());
        canonical.extend_from_slice(&value);
        if array.is_null(row) {
            display.push("<null>".to_string());
        } else {
            display.push(crate::value_for_rules(array.as_ref(), row)?);
        }
    }
    Ok((canonical, display.join("\u{1f}")))
}

fn row_hash(values: &[Option<Vec<u8>>]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proofframe:diff-row:v2\0");
    for value in values {
        match value {
            None => {
                hasher.update(&[0]);
            }
            Some(bytes) => {
                hasher.update(&[1]);
                hash_bytes(&mut hasher, bytes);
            }
        }
    }
    *hasher.finalize().as_bytes()
}

fn hash_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn row_partition(key: &[u8]) -> usize {
    let digest = blake3::hash(key);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    (u64::from_le_bytes(bytes) as usize) % PARTITIONS
}

fn estimated_entry_bytes(key: &[u8], entry: &RowEntry) -> u64 {
    let values = entry
        .values
        .iter()
        .filter_map(Option::as_ref)
        .map(|value| value.len() as u64)
        .sum::<u64>();
    key.len() as u64
        + entry.display_key.len() as u64
        + values
        + (entry.values.len() * std::mem::size_of::<Option<Vec<u8>>>()) as u64
        + 192
}

fn options_memory_cap(account: &ResourceAccount) -> u64 {
    account.limits().max_memory_bytes
}

fn ledger_charge(needed: u64, remaining: u64) -> u64 {
    if remaining < LEDGER_CHUNK_BYTES {
        return remaining;
    }
    needed.clamp(LEDGER_CHUNK_BYTES, remaining)
}

struct TempLedger {
    account: ResourceAccount,
    reservations: Vec<TempReservation>,
    reserved: u64,
    used: u64,
}

impl TempLedger {
    fn new(account: ResourceAccount) -> Self {
        Self {
            account,
            reservations: Vec::new(),
            reserved: 0,
            used: 0,
        }
    }

    fn reserve_additional(&mut self, bytes: u64) -> Result<(), ProofFrameError> {
        self.used = self
            .used
            .checked_add(bytes)
            .ok_or(ProofFrameError::ResourceLimit {
                resource: "temporary storage",
                requested: bytes,
                used: self.used,
                limit: self.account.limits().max_temp_bytes,
            })?;
        while self.reserved < self.used {
            let needed = self.used - self.reserved;
            let remaining = self
                .account
                .limits()
                .max_temp_bytes
                .saturating_sub(self.reserved);
            let charge = ledger_charge(needed, remaining);
            if charge == 0 {
                return Err(ProofFrameError::ResourceLimit {
                    resource: "temporary storage",
                    requested: bytes,
                    used: self.reserved,
                    limit: self.account.limits().max_temp_bytes,
                });
            }
            self.reservations
                .push(self.account.try_reserve_temp(charge)?);
            self.reserved += charge;
        }
        Ok(())
    }
}

struct MemoryLedger {
    account: ResourceAccount,
    reservations: Vec<MemoryReservation>,
    reserved: u64,
    used: u64,
}

impl MemoryLedger {
    fn new(account: ResourceAccount) -> Self {
        Self {
            account,
            reservations: Vec::new(),
            reserved: 0,
            used: 0,
        }
    }

    fn reserve_additional(&mut self, bytes: u64) -> Result<(), ProofFrameError> {
        self.used = self
            .used
            .checked_add(bytes)
            .ok_or(ProofFrameError::ResourceLimit {
                resource: "diff partition memory",
                requested: bytes,
                used: self.used,
                limit: self.account.limits().max_memory_bytes,
            })?;
        while self.reserved < self.used {
            let needed = self.used - self.reserved;
            let remaining = self
                .account
                .limits()
                .max_memory_bytes
                .saturating_sub(self.reserved);
            let charge = ledger_charge(needed, remaining);
            if charge == 0 {
                return Err(ProofFrameError::ResourceLimit {
                    resource: "diff partition memory",
                    requested: bytes,
                    used: self.reserved,
                    limit: self.account.limits().max_memory_bytes,
                });
            }
            self.reservations
                .push(self.account.try_reserve_memory(charge)?);
            self.reserved += charge;
        }
        Ok(())
    }
}

#[cfg(test)]
mod ledger_tests {
    use super::{LEDGER_CHUNK_BYTES, ledger_charge};

    #[test]
    fn ledger_charge_respects_small_remaining_budgets() {
        assert_eq!(
            ledger_charge(1, LEDGER_CHUNK_BYTES - 1),
            LEDGER_CHUNK_BYTES - 1
        );
    }

    #[test]
    fn ledger_charge_uses_chunks_without_exceeding_the_budget() {
        assert_eq!(ledger_charge(1, LEDGER_CHUNK_BYTES * 2), LEDGER_CHUNK_BYTES);
        assert_eq!(
            ledger_charge(LEDGER_CHUNK_BYTES + 7, LEDGER_CHUNK_BYTES * 2),
            LEDGER_CHUNK_BYTES + 7
        );
        assert_eq!(
            ledger_charge(LEDGER_CHUNK_BYTES * 3, LEDGER_CHUNK_BYTES * 2),
            LEDGER_CHUNK_BYTES * 2
        );
    }
}
