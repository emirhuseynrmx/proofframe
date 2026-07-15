#![forbid(unsafe_code)]
//! Native ProofFrame engine for Arrow-backed data contracts.
//!
//! The published crate currently provides the PyO3 extension module used by the Python package.
//! Its core invariants are stable enough to publish as an alpha: `pf-fp-v1` canonical dataset
//! fingerprints, disk-backed exact keyed diffs, privacy-preserving PII findings, leakage checks,
//! and signed proof receipts. A smaller public Rust API is planned before the 0.4 stable line.

mod pii;
mod receipt;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use ahash::RandomState;
use arrow::array::{
    Array, BinaryArray, BooleanArray, Date32Array, Date64Array, Decimal128Array, Float32Array,
    Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, LargeBinaryArray,
    LargeStringArray, RecordBatch, StringArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};
use arrow::ffi_stream::ArrowArrayStreamReader;
use arrow::pyarrow::PyArrowType;
use arrow::record_batch::RecordBatchReader;
use arrow::util::display::array_value_to_string;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use regex::Regex;
use roaring::RoaringTreemap;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const DEFAULT_MAX_FINDINGS: usize = 100;
const DIFF_PARTITIONS: usize = 64;

type RowValues = Vec<Option<Vec<u8>>>;
struct RowEntry {
    values: RowValues,
    hash: String,
}

#[derive(Debug, Serialize)]
struct ColumnProfile {
    name: String,
    data_type: String,
    null_count: u64,
    non_null_count: u64,
    distinct_count: usize,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Default)]
struct ColumnState {
    null_count: u64,
    non_null_count: u64,
    distinct: HashSet<Vec<u8>>,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Profile {
    rows: u64,
    columns: Vec<ColumnProfile>,
    fingerprint: String,
}

#[derive(Debug, Deserialize, Default)]
struct Contract {
    #[serde(default)]
    columns: HashMap<String, ColumnContract>,
    #[serde(default = "default_max_findings")]
    max_findings: usize,
}

fn default_max_findings() -> usize {
    DEFAULT_MAX_FINDINGS
}

#[derive(Debug, Deserialize, Default)]
struct ColumnContract {
    #[serde(default)]
    required: bool,
    #[serde(default)]
    not_null: bool,
    #[serde(default)]
    unique: bool,
    min: Option<f64>,
    max: Option<f64>,
    pattern: Option<String>,
    allowed: Option<HashSet<String>>,
}

#[derive(Debug, Serialize)]
struct Finding {
    rule: &'static str,
    column: String,
    row: Option<u64>,
    message: String,
}

#[derive(Debug, Serialize)]
struct ValidationReport {
    valid: bool,
    findings: Vec<Finding>,
    profile: Profile,
}

#[derive(Debug, Serialize)]
struct FastValidationReport {
    valid: bool,
    findings: Vec<Finding>,
    rows: u64,
    mode: &'static str,
}

enum UniqueState {
    Int64(RoaringTreemap),
    UInt64(RoaringTreemap),
    Float64(HashSet<u64, RandomState>),
    Utf8(HashSet<String, RandomState>),
    Generic(HashSet<Vec<u8>, RandomState>),
}

impl UniqueState {
    fn for_array(array: &dyn Array) -> Self {
        if array.as_any().is::<Int64Array>() {
            Self::Int64(RoaringTreemap::new())
        } else if array.as_any().is::<UInt64Array>() {
            Self::UInt64(RoaringTreemap::new())
        } else if array.as_any().is::<Float64Array>() {
            Self::Float64(HashSet::with_hasher(RandomState::new()))
        } else if array.as_any().is::<StringArray>() {
            Self::Utf8(HashSet::with_hasher(RandomState::new()))
        } else {
            Self::Generic(HashSet::with_hasher(RandomState::new()))
        }
    }
}

#[derive(Debug, Serialize)]
struct ChangedRow {
    key: String,
    columns: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DiffReport {
    keys: Vec<String>,
    before_rows: usize,
    after_rows: usize,
    added_count: usize,
    removed_count: usize,
    changed_count: usize,
    added_keys: Vec<String>,
    removed_keys: Vec<String>,
    changed: Vec<ChangedRow>,
}

#[derive(Debug, Serialize)]
struct PiiFinding {
    kind: &'static str,
    confidence: &'static str,
    column: String,
    row: u64,
    value_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct PiiReport {
    detected: bool,
    scanned_rows: u64,
    finding_count: usize,
    counts_by_kind: BTreeMap<&'static str, usize>,
    truncated: bool,
    findings: Vec<PiiFinding>,
}

#[derive(Debug, Serialize)]
struct LeakageReport {
    detected: bool,
    mode: &'static str,
    keys: Vec<String>,
    train_rows: usize,
    test_rows: usize,
    overlap_count: usize,
    train_overlap_rate: f64,
    test_overlap_rate: f64,
    sample_fingerprints: Vec<String>,
    truncated: bool,
}

fn py_err(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

fn update_len_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn update_schema_hash(hasher: &mut blake3::Hasher, name: &str, data_type: &str, nullable: bool) {
    hasher.update(b"pf-schema-field-v1\0");
    update_len_prefixed(hasher, name.as_bytes());
    update_len_prefixed(hasher, data_type.as_bytes());
    hasher.update(&[u8::from(nullable)]);
}

fn canonical_value_bytes(array: &dyn Array, row: usize) -> Result<Vec<u8>, String> {
    if array.is_null(row) {
        return Ok(vec![0]);
    }

    macro_rules! primitive_bytes {
        ($array_ty:ty, $tag:literal) => {
            if let Some(values) = array.as_any().downcast_ref::<$array_ty>() {
                let mut encoded = Vec::with_capacity(1 + 16);
                encoded.push($tag);
                encoded.extend_from_slice(&values.value(row).to_le_bytes());
                return Ok(encoded);
            }
        };
    }

    primitive_bytes!(Int8Array, 1);
    primitive_bytes!(Int16Array, 2);
    primitive_bytes!(Int32Array, 3);
    primitive_bytes!(Int64Array, 4);
    primitive_bytes!(UInt8Array, 5);
    primitive_bytes!(UInt16Array, 6);
    primitive_bytes!(UInt32Array, 7);
    primitive_bytes!(UInt64Array, 8);
    primitive_bytes!(Float32Array, 9);
    primitive_bytes!(Float64Array, 10);
    primitive_bytes!(Date32Array, 11);
    primitive_bytes!(Date64Array, 12);
    primitive_bytes!(TimestampSecondArray, 13);
    primitive_bytes!(TimestampMillisecondArray, 14);
    primitive_bytes!(TimestampMicrosecondArray, 15);
    primitive_bytes!(TimestampNanosecondArray, 16);
    primitive_bytes!(Decimal128Array, 17);

    if let Some(values) = array.as_any().downcast_ref::<BooleanArray>() {
        return Ok(vec![18, u8::from(values.value(row))]);
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        let value = values.value(row).as_bytes();
        let mut encoded = Vec::with_capacity(9 + value.len());
        encoded.push(19);
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value);
        return Ok(encoded);
    }
    if let Some(values) = array.as_any().downcast_ref::<LargeStringArray>() {
        let value = values.value(row).as_bytes();
        let mut encoded = Vec::with_capacity(9 + value.len());
        encoded.push(20);
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value);
        return Ok(encoded);
    }
    if let Some(values) = array.as_any().downcast_ref::<BinaryArray>() {
        let value = values.value(row);
        let mut encoded = Vec::with_capacity(9 + value.len());
        encoded.push(21);
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value);
        return Ok(encoded);
    }
    if let Some(values) = array.as_any().downcast_ref::<LargeBinaryArray>() {
        let value = values.value(row);
        let mut encoded = Vec::with_capacity(9 + value.len());
        encoded.push(22);
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value);
        return Ok(encoded);
    }

    Err(format!(
        "Unsupported Arrow type `{}` for canonical fingerprinting",
        array.data_type()
    ))
}

fn value_for_rules(array: &dyn Array, row: usize) -> Result<String, String> {
    array_value_to_string(array, row).map_err(|error| error.to_string())
}

fn update_hash(
    hasher: &mut blake3::Hasher,
    column: usize,
    array: &dyn Array,
    row: usize,
) -> Result<(), String> {
    hasher.update(b"pf-cell-v1\0");
    hasher.update(&(column as u64).to_le_bytes());
    let encoded = canonical_value_bytes(array, row)?;
    update_len_prefixed(hasher, &encoded);
    Ok(())
}

fn row_values_hash(values: &RowValues) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pf-diff-row-v1\0");
    for (column, value) in values.iter().enumerate() {
        hasher.update(&(column as u64).to_le_bytes());
        match value {
            Some(bytes) => update_len_prefixed(&mut hasher, bytes),
            None => {
                hasher.update(&u64::MAX.to_le_bytes());
            }
        };
    }
    hasher.finalize().to_hex().to_string()
}

fn inspect_batches<R>(
    reader: R,
    contract: Option<&Contract>,
) -> Result<(Profile, Vec<Finding>), String>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let mut states = schema
        .fields()
        .iter()
        .map(|_| ColumnState::default())
        .collect::<Vec<_>>();
    let mut findings = Vec::new();
    let mut seen_unique: HashMap<String, HashSet<Vec<u8>>> = HashMap::new();
    let mut patterns: HashMap<String, Regex> = HashMap::new();
    let max_findings = contract.map_or(DEFAULT_MAX_FINDINGS, |value| value.max_findings);

    if let Some(contract) = contract {
        for (name, rule) in &contract.columns {
            if rule.required && schema.index_of(name).is_err() {
                findings.push(Finding {
                    rule: "required",
                    column: name.clone(),
                    row: None,
                    message: format!("Required column `{name}` is missing"),
                });
            }
            if rule.unique {
                seen_unique.insert(name.clone(), HashSet::new());
            }
            if let Some(pattern) = &rule.pattern {
                patterns.insert(
                    name.clone(),
                    Regex::new(pattern).map_err(|error| error.to_string())?,
                );
            }
        }
    }

    let mut rows = 0_u64;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pf-fp-v1\0");
    for field in schema.fields() {
        let data_type = field.data_type().to_string();
        update_schema_hash(&mut hasher, field.name(), &data_type, field.is_nullable());
    }
    hasher.update(b"pf-fp-body-v1\0");
    for maybe_batch in reader {
        let batch = maybe_batch.map_err(|error| error.to_string())?;
        for row in 0..batch.num_rows() {
            let global_row = rows + row as u64;
            for (column_index, array) in batch.columns().iter().enumerate() {
                let field = schema.field(column_index);
                let state = &mut states[column_index];
                if array.is_null(row) {
                    state.null_count += 1;
                    update_hash(&mut hasher, column_index, array.as_ref(), row)?;
                    if let Some(rule) = contract.and_then(|value| value.columns.get(field.name())) {
                        if rule.not_null && findings.len() < max_findings {
                            findings.push(Finding {
                                rule: "not_null",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: "Null value is not allowed".to_string(),
                            });
                        }
                    }
                    continue;
                }

                state.non_null_count += 1;
                update_hash(&mut hasher, column_index, array.as_ref(), row)?;
                let value_key = canonical_value_bytes(array.as_ref(), row)?;
                state.distinct.insert(value_key.clone());
                if let Some(number) = numeric_value(array.as_ref(), row)? {
                    state.min = Some(state.min.map_or(number, |current| current.min(number)));
                    state.max = Some(state.max.map_or(number, |current| current.max(number)));
                }

                if let Some(rule) = contract.and_then(|value| value.columns.get(field.name())) {
                    let value = value_for_rules(array.as_ref(), row)?;
                    if rule.unique
                        && !seen_unique
                            .get_mut(field.name())
                            .expect("unique set initialized")
                            .insert(value_key)
                        && findings.len() < max_findings
                    {
                        findings.push(Finding {
                            rule: "unique",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: format!("Duplicate value `{value}`"),
                        });
                    }
                    if let Some(min) = rule.min {
                        if numeric_value(array.as_ref(), row)?.is_some_and(|number| number < min)
                            && findings.len() < max_findings
                        {
                            findings.push(Finding {
                                rule: "min",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: format!("Value `{value}` is below {min}"),
                            });
                        }
                    }
                    if let Some(max) = rule.max {
                        if numeric_value(array.as_ref(), row)?.is_some_and(|number| number > max)
                            && findings.len() < max_findings
                        {
                            findings.push(Finding {
                                rule: "max",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: format!("Value `{value}` is above {max}"),
                            });
                        }
                    }
                    if let Some(regex) = patterns.get(field.name()) {
                        if !regex.is_match(&value) && findings.len() < max_findings {
                            findings.push(Finding {
                                rule: "pattern",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: format!(
                                    "Value `{value}` does not match `{}`",
                                    regex.as_str()
                                ),
                            });
                        }
                    }
                    if let Some(allowed) = &rule.allowed {
                        if !allowed.contains(&value) && findings.len() < max_findings {
                            findings.push(Finding {
                                rule: "allowed",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: format!("Value `{value}` is not in the allowlist"),
                            });
                        }
                    }
                }
            }
        }
        rows += batch.num_rows() as u64;
    }

    let columns = schema
        .fields()
        .iter()
        .zip(states)
        .map(|(field, state)| ColumnProfile {
            name: field.name().clone(),
            data_type: field.data_type().to_string(),
            null_count: state.null_count,
            non_null_count: state.non_null_count,
            distinct_count: state.distinct.len(),
            min: state.min,
            max: state.max,
        })
        .collect();
    Ok((
        Profile {
            rows,
            columns,
            fingerprint: format!("pf-fp-v1:{}", hasher.finalize().to_hex()),
        },
        findings,
    ))
}

fn row_partition(key: &str) -> usize {
    let digest = blake3::hash(key.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    (u64::from_le_bytes(bytes) as usize) % DIFF_PARTITIONS
}

fn write_u64(writer: &mut BufWriter<File>, value: u64) -> Result<(), String> {
    writer
        .write_all(&value.to_le_bytes())
        .map_err(|error| error.to_string())
}

fn read_u64(reader: &mut BufReader<File>) -> Result<Option<u64>, String> {
    let mut bytes = [0_u8; 8];
    match reader.read_exact(&mut bytes) {
        Ok(()) => Ok(Some(u64::from_le_bytes(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_bytes(writer: &mut BufWriter<File>, value: &[u8]) -> Result<(), String> {
    write_u64(writer, value.len() as u64)?;
    writer.write_all(value).map_err(|error| error.to_string())
}

fn read_bytes(reader: &mut BufReader<File>) -> Result<Vec<u8>, String> {
    let len = read_u64(reader)?.ok_or_else(|| "Truncated diff partition record".to_string())?;
    let mut value = vec![0_u8; len as usize];
    reader
        .read_exact(&mut value)
        .map_err(|error| error.to_string())?;
    Ok(value)
}

fn write_row_record(
    writer: &mut BufWriter<File>,
    key: &str,
    entry: &RowEntry,
) -> Result<(), String> {
    write_bytes(writer, key.as_bytes())?;
    write_bytes(writer, entry.hash.as_bytes())?;
    write_u64(writer, entry.values.len() as u64)?;
    for value in &entry.values {
        match value {
            Some(bytes) => write_bytes(writer, bytes)?,
            None => write_u64(writer, u64::MAX)?,
        }
    }
    Ok(())
}

fn read_row_record(reader: &mut BufReader<File>) -> Result<Option<(String, RowEntry)>, String> {
    let Some(key_len) = read_u64(reader)? else {
        return Ok(None);
    };
    let mut key = vec![0_u8; key_len as usize];
    reader
        .read_exact(&mut key)
        .map_err(|error| error.to_string())?;
    let hash = String::from_utf8(read_bytes(reader)?).map_err(|error| error.to_string())?;
    let value_count =
        read_u64(reader)?.ok_or_else(|| "Truncated diff partition record".to_string())?;
    let mut values = Vec::with_capacity(value_count as usize);
    for _ in 0..value_count {
        let len = read_u64(reader)?.ok_or_else(|| "Truncated diff value".to_string())?;
        if len == u64::MAX {
            values.push(None);
        } else {
            let mut value = vec![0_u8; len as usize];
            reader
                .read_exact(&mut value)
                .map_err(|error| error.to_string())?;
            values.push(Some(value));
        }
    }
    let key = String::from_utf8(key).map_err(|error| error.to_string())?;
    Ok(Some((key, RowEntry { values, hash })))
}

fn partition_rows<R>(
    reader: R,
    keys: &[String],
    directory: &Path,
    prefix: &str,
) -> Result<(Vec<String>, usize, Vec<PathBuf>), String>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let key_indexes = keys
        .iter()
        .map(|key| {
            schema
                .index_of(key)
                .map_err(|_| format!("Key column `{key}` is missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let names = schema
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect::<Vec<_>>();
    let paths = (0..DIFF_PARTITIONS)
        .map(|partition| directory.join(format!("{prefix}-{partition}.pfpart")))
        .collect::<Vec<_>>();
    let mut writers = paths
        .iter()
        .map(|path| File::create(path).map(BufWriter::new))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut row_count = 0_usize;
    for maybe_batch in reader {
        let batch: RecordBatch = maybe_batch.map_err(|error| error.to_string())?;
        for row in 0..batch.num_rows() {
            let key = key_indexes
                .iter()
                .map(|index| {
                    let array = batch.column(*index);
                    if array.is_null(row) {
                        "<null>".to_string()
                    } else {
                        value_for_rules(array.as_ref(), row).unwrap_or_default()
                    }
                })
                .collect::<Vec<_>>()
                .join("\u{1f}");
            let values = batch
                .columns()
                .iter()
                .map(|array| {
                    if array.is_null(row) {
                        Ok(None)
                    } else {
                        canonical_value_bytes(array.as_ref(), row).map(Some)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let hash = row_values_hash(&values);
            let partition = row_partition(&key);
            write_row_record(&mut writers[partition], &key, &RowEntry { values, hash })?;
            row_count += 1;
        }
    }
    for writer in &mut writers {
        writer.flush().map_err(|error| error.to_string())?;
    }
    Ok((names, row_count, paths))
}

fn process_diff_partition(
    before_path: &Path,
    after_path: &Path,
    column_names: &[String],
    added_keys: &mut Vec<String>,
    removed_keys: &mut Vec<String>,
    changed: &mut Vec<ChangedRow>,
) -> Result<(), String> {
    let mut before_rows = HashMap::new();
    let mut before_reader =
        BufReader::new(File::open(before_path).map_err(|error| error.to_string())?);
    while let Some((key, entry)) = read_row_record(&mut before_reader)? {
        if before_rows.insert(key.clone(), entry).is_some() {
            return Err(format!("Duplicate key `{key}`; diff keys must be unique"));
        }
    }

    let mut seen_after = HashSet::new();
    let mut after_reader =
        BufReader::new(File::open(after_path).map_err(|error| error.to_string())?);
    while let Some((key, entry)) = read_row_record(&mut after_reader)? {
        if !seen_after.insert(key.clone()) {
            return Err(format!("Duplicate key `{key}`; diff keys must be unique"));
        }
        if let Some(before_entry) = before_rows.get(&key) {
            if before_entry.hash != entry.hash {
                let columns = before_entry
                    .values
                    .iter()
                    .zip(&entry.values)
                    .zip(column_names)
                    .filter(|((before, after), _)| before != after)
                    .map(|(_, name)| name.clone())
                    .collect();
                changed.push(ChangedRow { key, columns });
            }
        } else {
            added_keys.push(key);
        }
    }

    removed_keys.extend(
        before_rows
            .keys()
            .filter(|key| !seen_after.contains(*key))
            .cloned(),
    );
    Ok(())
}

fn privacy_fingerprint(value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proofframe:privacy:v1\0");
    hasher.update(value.as_bytes());
    hasher.finalize().to_hex()[..16].to_string()
}

fn collect_leakage_ids<R>(
    reader: R,
    keys: &[String],
) -> Result<(Vec<String>, usize, HashSet<String>), String>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let names = schema
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect::<Vec<_>>();
    let indexes = if keys.is_empty() {
        (0..schema.fields().len()).collect::<Vec<_>>()
    } else {
        keys.iter()
            .map(|key| {
                schema
                    .index_of(key)
                    .map_err(|_| format!("Key column `{key}` is missing"))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut row_count = 0_usize;
    let mut ids = HashSet::new();
    for maybe_batch in reader {
        let batch = maybe_batch.map_err(|error| error.to_string())?;
        for row in 0..batch.num_rows() {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"proofframe:leakage:v1\0");
            for index in &indexes {
                let array = batch.column(*index);
                update_hash(&mut hasher, *index, array.as_ref(), row)?;
            }
            ids.insert(hasher.finalize().to_hex().to_string());
            row_count += 1;
        }
    }
    Ok((names, row_count, ids))
}

fn numeric_value(array: &dyn Array, row: usize) -> Result<Option<f64>, String> {
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        Ok(Some(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<Float32Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt64Array>() {
        Ok(Some(values.value(row) as f64))
    } else {
        Ok(array_value_to_string(array, row)
            .map_err(|error| error.to_string())?
            .parse::<f64>()
            .ok())
    }
}

fn is_numeric_array(array: &dyn Array) -> bool {
    array.as_any().is::<Int8Array>()
        || array.as_any().is::<Int16Array>()
        || array.as_any().is::<Int32Array>()
        || array.as_any().is::<Int64Array>()
        || array.as_any().is::<UInt8Array>()
        || array.as_any().is::<UInt16Array>()
        || array.as_any().is::<UInt32Array>()
        || array.as_any().is::<UInt64Array>()
        || array.as_any().is::<Float32Array>()
        || array.as_any().is::<Float64Array>()
}

fn push_duplicate(findings: &mut Vec<Finding>, column: &str, row: u64, max_findings: usize) {
    if findings.len() < max_findings {
        findings.push(Finding {
            rule: "unique",
            column: column.to_string(),
            row: Some(row),
            message: "Duplicate value detected".to_string(),
        });
    }
}

fn check_unique(
    state: &mut UniqueState,
    array: &dyn Array,
    column: &str,
    row_offset: u64,
    findings: &mut Vec<Finding>,
    max_findings: usize,
) -> Result<(), String> {
    match state {
        UniqueState::Int64(seen) => {
            let values = array
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("type fixed from schema");
            for row in 0..values.len() {
                if values.is_valid(row) && !seen.insert((values.value(row) as u64) ^ (1_u64 << 63))
                {
                    push_duplicate(findings, column, row_offset + row as u64, max_findings);
                }
            }
        }
        UniqueState::UInt64(seen) => {
            let values = array
                .as_any()
                .downcast_ref::<UInt64Array>()
                .expect("type fixed from schema");
            for row in 0..values.len() {
                if values.is_valid(row) && !seen.insert(values.value(row)) {
                    push_duplicate(findings, column, row_offset + row as u64, max_findings);
                }
            }
        }
        UniqueState::Float64(seen) => {
            let values = array
                .as_any()
                .downcast_ref::<Float64Array>()
                .expect("type fixed from schema");
            for row in 0..values.len() {
                if values.is_valid(row) && !seen.insert(values.value(row).to_bits()) {
                    push_duplicate(findings, column, row_offset + row as u64, max_findings);
                }
            }
        }
        UniqueState::Utf8(seen) => {
            let values = array
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("type fixed from schema");
            for row in 0..values.len() {
                if values.is_valid(row) && !seen.insert(values.value(row).to_owned()) {
                    push_duplicate(findings, column, row_offset + row as u64, max_findings);
                }
            }
        }
        UniqueState::Generic(seen) => {
            for row in 0..array.len() {
                if array.is_valid(row) {
                    let value = canonical_value_bytes(array, row)?;
                    if !seen.insert(value) {
                        push_duplicate(findings, column, row_offset + row as u64, max_findings);
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_range(
    array: &dyn Array,
    rule: &ColumnContract,
    column: &str,
    row_offset: u64,
    findings: &mut Vec<Finding>,
    max_findings: usize,
) -> Result<(), String> {
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        for row in 0..values.len() {
            if values.is_valid(row) {
                push_range_findings(
                    values.value(row),
                    rule,
                    column,
                    row_offset + row as u64,
                    findings,
                    max_findings,
                );
            }
        }
    } else {
        for row in 0..array.len() {
            if array.is_valid(row) {
                if let Some(value) = numeric_value(array, row)? {
                    push_range_findings(
                        value,
                        rule,
                        column,
                        row_offset + row as u64,
                        findings,
                        max_findings,
                    );
                }
            }
        }
    }
    Ok(())
}

fn push_range_findings(
    number: f64,
    rule: &ColumnContract,
    column: &str,
    row: u64,
    findings: &mut Vec<Finding>,
    max_findings: usize,
) {
    if rule.min.is_some_and(|minimum| number < minimum) && findings.len() < max_findings {
        findings.push(Finding {
            rule: "min",
            column: column.to_string(),
            row: Some(row),
            message: format!("Value is below {}", rule.min.unwrap()),
        });
    }
    if rule.max.is_some_and(|maximum| number > maximum) && findings.len() < max_findings {
        findings.push(Finding {
            rule: "max",
            column: column.to_string(),
            row: Some(row),
            message: format!("Value is above {}", rule.max.unwrap()),
        });
    }
}

fn validate_fast_batches<R>(reader: R, contract: &Contract) -> Result<FastValidationReport, String>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let mut findings = Vec::new();
    let mut patterns = HashMap::new();
    let mut unique_states: HashMap<String, UniqueState> = HashMap::new();
    for (name, rule) in &contract.columns {
        if rule.required && schema.index_of(name).is_err() {
            findings.push(Finding {
                rule: "required",
                column: name.clone(),
                row: None,
                message: format!("Required column `{name}` is missing"),
            });
        }
        if let Some(pattern) = &rule.pattern {
            patterns.insert(
                name.clone(),
                Regex::new(pattern).map_err(|error| error.to_string())?,
            );
        }
    }

    let mut rows = 0_u64;
    for maybe_batch in reader {
        let batch = maybe_batch.map_err(|error| error.to_string())?;
        for (column_index, field) in schema.fields().iter().enumerate() {
            let Some(rule) = contract.columns.get(field.name()) else {
                continue;
            };
            let array = batch.column(column_index);
            if rule.unique {
                unique_states
                    .entry(field.name().clone())
                    .or_insert_with(|| UniqueState::for_array(array.as_ref()));
            }
            if rule.not_null && array.null_count() > 0 {
                for row in 0..batch.num_rows() {
                    if array.is_null(row) && findings.len() < contract.max_findings {
                        findings.push(Finding {
                            rule: "not_null",
                            column: field.name().clone(),
                            row: Some(rows + row as u64),
                            message: "Null value is not allowed".to_string(),
                        });
                    }
                }
            }
            if rule.unique {
                check_unique(
                    unique_states
                        .get_mut(field.name())
                        .expect("unique state initialized"),
                    array.as_ref(),
                    field.name(),
                    rows,
                    &mut findings,
                    contract.max_findings,
                )?;
            }
            if rule.min.is_some() || rule.max.is_some() {
                check_range(
                    array.as_ref(),
                    rule,
                    field.name(),
                    rows,
                    &mut findings,
                    contract.max_findings,
                )?;
            }
            if rule.pattern.is_some() || rule.allowed.is_some() {
                for row in 0..batch.num_rows() {
                    if array.is_null(row) || findings.len() >= contract.max_findings {
                        continue;
                    }
                    let value = value_for_rules(array.as_ref(), row)?;
                    if patterns
                        .get(field.name())
                        .is_some_and(|pattern| !pattern.is_match(&value))
                    {
                        findings.push(Finding {
                            rule: "pattern",
                            column: field.name().clone(),
                            row: Some(rows + row as u64),
                            message: "Value does not match the required pattern".to_string(),
                        });
                    }
                    if rule
                        .allowed
                        .as_ref()
                        .is_some_and(|allowed| !allowed.contains(&value))
                        && findings.len() < contract.max_findings
                    {
                        findings.push(Finding {
                            rule: "allowed",
                            column: field.name().clone(),
                            row: Some(rows + row as u64),
                            message: "Value is not in the allowlist".to_string(),
                        });
                    }
                }
            }
        }
        rows += batch.num_rows() as u64;
    }
    Ok(FastValidationReport {
        valid: findings.is_empty(),
        findings,
        rows,
        mode: "rules_only",
    })
}

#[pyfunction]
fn profile_arrow(source: PyArrowType<ArrowArrayStreamReader>) -> PyResult<String> {
    let (profile, _) = inspect_batches(source.0, None).map_err(py_err)?;
    serde_json::to_string(&profile).map_err(py_err)
}

#[pyfunction]
fn validate_arrow(
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: &str,
) -> PyResult<String> {
    let contract: Contract = serde_json::from_str(contract_json).map_err(py_err)?;
    let (profile, findings) = inspect_batches(source.0, Some(&contract)).map_err(py_err)?;
    serde_json::to_string(&ValidationReport {
        valid: findings.is_empty(),
        findings,
        profile,
    })
    .map_err(py_err)
}

#[pyfunction]
fn validate_fast_arrow(
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: &str,
) -> PyResult<String> {
    let contract: Contract = serde_json::from_str(contract_json).map_err(py_err)?;
    let report = validate_fast_batches(source.0, &contract).map_err(py_err)?;
    serde_json::to_string(&report).map_err(py_err)
}

#[pyfunction]
fn diff_arrow(
    before: PyArrowType<ArrowArrayStreamReader>,
    after: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
) -> PyResult<String> {
    if keys.is_empty() {
        return Err(py_err("At least one key column is required"));
    }
    let directory = TempDir::new().map_err(py_err)?;
    let (before_names, before_count, before_paths) =
        partition_rows(before.0, &keys, directory.path(), "before").map_err(py_err)?;
    let (after_names, after_count, after_paths) =
        partition_rows(after.0, &keys, directory.path(), "after").map_err(py_err)?;
    if before_names != after_names {
        return Err(py_err(
            "Schemas differ; normalize columns before row-level diff",
        ));
    }

    let mut added_keys = Vec::new();
    let mut removed_keys = Vec::new();
    let mut changed = Vec::new();
    for (before_path, after_path) in before_paths.iter().zip(&after_paths) {
        process_diff_partition(
            before_path,
            after_path,
            &before_names,
            &mut added_keys,
            &mut removed_keys,
            &mut changed,
        )
        .map_err(py_err)?;
    }
    added_keys.sort();
    removed_keys.sort();
    changed.sort_by(|left, right| left.key.cmp(&right.key));
    let report = DiffReport {
        keys,
        before_rows: before_count,
        after_rows: after_count,
        added_count: added_keys.len(),
        removed_count: removed_keys.len(),
        changed_count: changed.len(),
        added_keys,
        removed_keys,
        changed,
    };
    serde_json::to_string(&report).map_err(py_err)
}

#[pyfunction]
#[pyo3(signature = (source, max_findings=100))]
fn scan_pii_arrow(
    source: PyArrowType<ArrowArrayStreamReader>,
    max_findings: usize,
) -> PyResult<String> {
    let reader = source.0;
    let schema = reader.schema();
    let detector = pii::Detector::new().map_err(py_err)?;
    let mut findings = Vec::new();
    let mut counts = BTreeMap::new();
    let mut scanned_rows = 0_u64;
    let mut total_findings = 0_usize;
    for maybe_batch in reader {
        let batch = maybe_batch.map_err(py_err)?;
        for row in 0..batch.num_rows() {
            for (column, array) in batch.columns().iter().enumerate() {
                if array.is_null(row) {
                    continue;
                }
                let value = value_for_rules(array.as_ref(), row).map_err(py_err)?;
                if let Some(classification) =
                    detector.classify_cell(&value, is_numeric_array(array.as_ref()))
                {
                    total_findings += 1;
                    *counts.entry(classification.kind).or_insert(0) += 1;
                    if findings.len() < max_findings {
                        findings.push(PiiFinding {
                            kind: classification.kind,
                            confidence: classification.confidence,
                            column: schema.field(column).name().clone(),
                            row: scanned_rows + row as u64,
                            value_fingerprint: privacy_fingerprint(&value),
                        });
                    }
                }
            }
        }
        scanned_rows += batch.num_rows() as u64;
    }
    serde_json::to_string(&PiiReport {
        detected: total_findings > 0,
        scanned_rows,
        finding_count: total_findings,
        counts_by_kind: counts,
        truncated: total_findings > findings.len(),
        findings,
    })
    .map_err(py_err)
}

#[pyfunction]
#[pyo3(signature = (train, test, keys, max_samples=20))]
fn detect_leakage_arrow(
    train: PyArrowType<ArrowArrayStreamReader>,
    test: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
    max_samples: usize,
) -> PyResult<String> {
    let (train_names, train_rows, train_ids) =
        collect_leakage_ids(train.0, &keys).map_err(py_err)?;
    let (test_names, test_rows, test_ids) = collect_leakage_ids(test.0, &keys).map_err(py_err)?;
    if keys.is_empty() && train_names != test_names {
        return Err(py_err(
            "Schemas differ; full-row leakage detection requires identical columns",
        ));
    }
    let mut overlap = train_ids
        .intersection(&test_ids)
        .cloned()
        .collect::<Vec<_>>();
    overlap.sort();
    let overlap_count = overlap.len();
    let truncated = overlap_count > max_samples;
    overlap.truncate(max_samples);
    let rate = |count: usize, total: usize| {
        if total == 0 {
            0.0
        } else {
            count as f64 / total as f64
        }
    };
    serde_json::to_string(&LeakageReport {
        detected: overlap_count > 0,
        mode: if keys.is_empty() { "full_row" } else { "key" },
        keys,
        train_rows,
        test_rows,
        overlap_count,
        train_overlap_rate: rate(overlap_count, train_ids.len()),
        test_overlap_rate: rate(overlap_count, test_ids.len()),
        sample_fingerprints: overlap,
        truncated,
    })
    .map_err(py_err)
}

#[pyfunction]
fn generate_signing_keypair() -> PyResult<String> {
    receipt::generate_keypair_json().map_err(py_err)
}

#[pyfunction]
fn sign_proof_receipt(report_json: &str, private_key: &str) -> PyResult<String> {
    receipt::sign_json(report_json, private_key).map_err(py_err)
}

#[pyfunction]
fn verify_proof_receipt(receipt_json: &str) -> PyResult<String> {
    let verification = receipt::verify_json(receipt_json).map_err(py_err)?;
    serde_json::to_string(&verification).map_err(py_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow::array::ArrayRef;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatchIterator;
    use proptest::prelude::*;

    fn reader_from_batch(batch: RecordBatch) -> impl RecordBatchReader {
        let schema = batch.schema();
        RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema)
    }

    fn finding_signature(findings: &[Finding]) -> Vec<(&'static str, String, Option<u64>)> {
        let mut signature = findings
            .iter()
            .map(|finding| (finding.rule, finding.column.clone(), finding.row))
            .collect::<Vec<_>>();
        signature.sort();
        signature
    }

    proptest! {
        #[test]
        fn full_and_fast_paths_have_same_rule_verdicts(
            rows in prop::collection::vec((-10_i64..10, -250_i32..250), 0..40)
        ) {
            let ids = rows.iter().map(|(id, _)| *id).collect::<Vec<_>>();
            let scores = rows
                .iter()
                .map(|(_, score)| f64::from(*score) / 100.0)
                .collect::<Vec<_>>();
            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Int64, false),
                Field::new("score", DataType::Float64, false),
            ]));
            let batch = RecordBatch::try_new(
                schema,
                vec![
                    Arc::new(Int64Array::from(ids)) as ArrayRef,
                    Arc::new(Float64Array::from(scores)) as ArrayRef,
                ],
            )
            .unwrap();
            let contract = Contract {
                columns: HashMap::from([
                    (
                        "id".to_string(),
                        ColumnContract {
                            unique: true,
                            min: Some(-3.0),
                            max: Some(3.0),
                            ..ColumnContract::default()
                        },
                    ),
                    (
                        "score".to_string(),
                        ColumnContract {
                            unique: true,
                            min: Some(-1.0),
                            max: Some(1.0),
                            ..ColumnContract::default()
                        },
                    ),
                ]),
                max_findings: 1_000,
            };

            let (_, full_findings) =
                inspect_batches(reader_from_batch(batch.clone()), Some(&contract)).unwrap();
            let fast_report = validate_fast_batches(reader_from_batch(batch), &contract).unwrap();

            prop_assert_eq!(
                finding_signature(&full_findings),
                finding_signature(&fast_report.findings)
            );
        }
    }
}

#[pymodule]
fn _proofframe(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(profile_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(validate_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(validate_fast_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(diff_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(scan_pii_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(detect_leakage_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(generate_signing_keypair, module)?)?;
    module.add_function(wrap_pyfunction!(sign_proof_receipt, module)?)?;
    module.add_function(wrap_pyfunction!(verify_proof_receipt, module)?)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
