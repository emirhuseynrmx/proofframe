//! Resource-bounded exact referential integrity against caller-supplied datasets.
//!
//! A reference rule is checked with the same sorted-run machinery as exact uniqueness.
//! The reference dataset is scanned once into a sealed state; the local keys accumulate
//! during the ordinary validation pass; the two are then anti-joined, which reports the
//! distinct local keys that the reference never contained.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::datatypes::{DataType, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchReader};

use super::record_lazy;
use crate::distinct::{SealedState, anti_join_exact_states};
use crate::{
    CancellationToken, ErrorCode, ExactState, FingerprintOptions, FingerprintVersion,
    ProofFrameError, ReferenceNullPolicyAst, ReferencePlan, ResourceAccount, ResourceLimits,
    ValidationState, ValueKind, ValueRef, encoding::V2FingerprintState,
};

/// What a single reference rule was checked against.
///
/// The fingerprint is the point of the record: a report saying a foreign key held is not
/// verifiable unless it also identifies the dataset the key was resolved against.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReferenceOutcome {
    /// Contract rule name.
    pub name: String,
    /// Logical name the contract bound to a caller-supplied dataset.
    pub reference: String,
    /// `pf-fp-v2` fingerprint of the reference dataset as it was scanned.
    pub reference_fingerprint: String,
    /// Rows read from the reference dataset.
    pub reference_rows: u64,
    /// Distinct non-null keys the reference dataset contained.
    pub reference_distinct_keys: u64,
    /// Distinct local keys that were looked up.
    pub checked_distinct_keys: u64,
    /// Distinct local keys absent from the reference dataset.
    pub missing_distinct_keys: u64,
}

/// Reference datasets supplied for one execution, keyed by their contract name.
///
/// Readers are consumed, so a binding set is used once.
#[derive(Default)]
pub struct ReferenceBindings {
    readers: BTreeMap<String, Box<dyn RecordBatchReader + Send>>,
}

impl std::fmt::Debug for ReferenceBindings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReferenceBindings")
            .field("names", &self.readers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ReferenceBindings {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a reader to the name a contract's reference rules refer to.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        reader: Box<dyn RecordBatchReader + Send>,
    ) -> Option<Box<dyn RecordBatchReader + Send>> {
        self.readers.insert(name.into(), reader)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.readers.is_empty()
    }

    fn take(&mut self, name: &str) -> Option<Box<dyn RecordBatchReader + Send>> {
        self.readers.remove(name)
    }

    fn remaining(&self) -> Vec<&str> {
        self.readers.keys().map(String::as_str).collect()
    }
}

/// One reference rule prepared for probing: the reference side is already sealed.
pub(super) struct PreparedReference {
    plan: ReferencePlan,
    reference: Arc<SealedState>,
    fingerprint: String,
    reference_rows: u64,
}

/// A per-execution accumulator of local keys for every reference rule.
pub(super) struct ReferenceState {
    entries: Vec<ReferenceEntry>,
    _directory: Option<tempfile::TempDir>,
}

struct ReferenceEntry {
    plan: ReferencePlan,
    local: ExactState,
    scratch: Vec<u8>,
}

/// Scan every bound reference dataset and reject a binding set that does not match the plan.
///
/// Both directions fail closed. A rule with no binding cannot be silently skipped, because a
/// skipped foreign key reports as a pass. A binding with no rule is almost always a
/// misspelled name, which would otherwise look like it had been checked.
pub(super) fn prepare(
    plans: &[ReferencePlan],
    mut bindings: ReferenceBindings,
    local_schema: &Schema,
    account: &ResourceAccount,
    limits: ResourceLimits,
    cancellation: &CancellationToken,
) -> Result<Vec<PreparedReference>, ProofFrameError> {
    let mut prepared = Vec::with_capacity(plans.len());
    // Several rules may cite the same dataset; the first one consumes the reader and the
    // rest reuse the scan through this cache.
    let mut scanned = BTreeMap::<String, (Arc<SealedState>, String, u64)>::new();
    for plan in plans {
        let key = reference_cache_key(plan);
        if !scanned.contains_key(&key) {
            let Some(reader) = bindings.take(plan.reference()) else {
                return Err(ProofFrameError::contract(
                    ErrorCode::ReferenceUnbound,
                    format!(
                        "Reference rule `{}` needs a dataset bound to `{}`",
                        plan.name(),
                        plan.reference()
                    ),
                    Some(format!("$.dataset_rules.references[{}]", plan.name())),
                ));
            };
            let (sealed, fingerprint, rows) =
                scan_reference(plan, reader, local_schema, account, limits, cancellation)?;
            scanned.insert(key.clone(), (Arc::new(sealed), fingerprint, rows));
        }
        // A cached scan is reused by rules that share both the dataset and the key columns,
        // so the same sealed state can serve more than one rule only when it is the same
        // question. Distinct key columns produce distinct cache keys and a fresh scan.
        let (reference, fingerprint, reference_rows) = scanned
            .get(&key)
            .expect("the reference was just scanned or already cached");
        prepared.push(PreparedReference {
            plan: plan.clone(),
            reference: Arc::clone(reference),
            fingerprint: fingerprint.clone(),
            reference_rows: *reference_rows,
        });
    }
    let unused = bindings.remaining();
    if !unused.is_empty() {
        return Err(ProofFrameError::contract(
            ErrorCode::ReferenceUnbound,
            format!(
                "No reference rule uses the bound dataset(s): {}",
                unused.join(", ")
            ),
            None,
        ));
    }
    Ok(prepared)
}

fn reference_cache_key(plan: &ReferencePlan) -> String {
    let mut key = String::from(plan.reference());
    for column in plan.reference_columns() {
        key.push('\u{1f}');
        key.push_str(column);
    }
    key
}

fn scan_reference(
    plan: &ReferencePlan,
    reader: Box<dyn RecordBatchReader + Send>,
    local_schema: &Schema,
    account: &ResourceAccount,
    limits: ResourceLimits,
    cancellation: &CancellationToken,
) -> Result<(SealedState, String, u64), ProofFrameError> {
    let schema = reader.schema();
    let indexes = resolve_reference_key(plan, local_schema, schema.as_ref())?;
    let directory = tempfile::TempDir::new()?;
    let mut state = ExactState::new_with_cancellation(
        ValueKind::Bytes,
        account.child(limits.max_memory_bytes, limits.max_temp_bytes),
        Some(directory.path().to_path_buf()),
        None,
        cancellation.clone(),
    )?;
    let mut fingerprint = V2FingerprintState::new(
        schema.as_ref(),
        &FingerprintOptions::new(FingerprintVersion::V2),
    )?;
    let mut scratch = Vec::with_capacity(indexes.len().saturating_mul(24));
    let mut rows = 0_u64;
    for batch in reader {
        cancellation.check()?;
        let batch = batch?;
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ProofFrameError::SchemaMismatch(format!(
                "reference dataset `{}` changed schema while it was scanned",
                plan.reference()
            )));
        }
        fingerprint.update(&batch)?;
        for row in 0..batch.num_rows() {
            // A null on the reference side is not an identity anything can match, so it
            // never enters the set. Local nulls are governed separately by the rule's
            // null policy.
            if encode_key(&batch, &indexes, row, &mut scratch)? {
                state.insert(ValueRef::Bytes(&scratch), rows + row as u64)?;
            }
        }
        rows += batch.num_rows() as u64;
    }
    let sealed = SealedState::seal(state, directory)?;
    Ok((sealed, fingerprint.finish().to_tagged_string(), rows))
}

/// Resolve the reference key columns and require both sides to carry the same Arrow types.
///
/// Names may differ across the two datasets, types may not: the composite key encoding is
/// type-tagged, so an `Int32` local key and an `Int64` reference key would never match and
/// every row would report as a violation. Rejecting the pair is the honest answer.
fn resolve_reference_key(
    plan: &ReferencePlan,
    local_schema: &Schema,
    reference_schema: &Schema,
) -> Result<Vec<usize>, ProofFrameError> {
    plan.reference_columns()
        .iter()
        .enumerate()
        .map(|(ordinal, column)| {
            let index = reference_schema.index_of(column).map_err(|_| {
                ProofFrameError::contract(
                    ErrorCode::MissingColumn,
                    format!(
                        "Reference dataset `{}` has no column `{column}` for rule `{}`",
                        plan.reference(),
                        plan.name()
                    ),
                    None,
                )
            })?;
            let local = local_schema.field(plan.columns()[ordinal]).data_type();
            let remote = reference_schema.field(index).data_type();
            if !types_match(local, remote) {
                return Err(ProofFrameError::contract(
                    ErrorCode::ContractTypeMismatch,
                    format!(
                        "Reference rule `{}` pairs `{local}` with `{remote}` at key position {ordinal}",
                        plan.name()
                    ),
                    None,
                ));
            }
            Ok(index)
        })
        .collect()
}

/// Compare key types while ignoring the parts that do not change a value's identity.
///
/// Timestamps are compared by unit only. Two timestamp columns with the same unit hold the
/// same integer ticks whatever timezone metadata each carries, and Arrow timezone strings
/// are a display attribute rather than a different instant.
fn types_match(local: &DataType, remote: &DataType) -> bool {
    match (local, remote) {
        (DataType::Timestamp(left, _), DataType::Timestamp(right, _)) => left == right,
        _ => local == remote,
    }
}

impl ReferenceState {
    pub(super) fn new(
        prepared: Vec<PreparedReference>,
        account: &ResourceAccount,
        limits: ResourceLimits,
        row_count_hint: Option<u64>,
        cancellation: &CancellationToken,
    ) -> Result<(Self, Vec<PreparedReference>), ProofFrameError> {
        if prepared.is_empty() {
            return Ok((
                Self {
                    entries: Vec::new(),
                    _directory: None,
                },
                prepared,
            ));
        }
        let directory = tempfile::TempDir::new()?;
        let mut entries = Vec::with_capacity(prepared.len());
        for reference in &prepared {
            entries.push(ReferenceEntry {
                plan: reference.plan.clone(),
                local: ExactState::new_with_cancellation(
                    ValueKind::Bytes,
                    account.child(limits.max_memory_bytes, limits.max_temp_bytes),
                    Some(directory.path().to_path_buf()),
                    row_count_hint,
                    cancellation.clone(),
                )?,
                scratch: Vec::with_capacity(reference.plan.columns().len().saturating_mul(24)),
            });
        }
        Ok((
            Self {
                entries,
                _directory: Some(directory),
            },
            prepared,
        ))
    }

    pub(super) fn update(
        &mut self,
        batch: &RecordBatch,
        row_offset: u64,
        validation: &mut ValidationState,
    ) -> Result<(), ProofFrameError> {
        for entry in &mut self.entries {
            for row in 0..batch.num_rows() {
                let global_row = row_offset + row as u64;
                if encode_key(batch, entry.plan.columns(), row, &mut entry.scratch)? {
                    entry
                        .local
                        .insert(ValueRef::Bytes(&entry.scratch), global_row)?;
                } else if entry.plan.nulls() == ReferenceNullPolicyAst::Reject {
                    record_lazy(
                        validation,
                        "references",
                        "$dataset",
                        Some(global_row),
                        || format!("Reference rule `{}` rejects null keys", entry.plan.name()),
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        prepared: Vec<PreparedReference>,
        max_samples: usize,
        validation: &mut ValidationState,
    ) -> Result<(Vec<ReferenceOutcome>, super::ExactMetrics), ProofFrameError> {
        let mut outcomes = Vec::with_capacity(prepared.len());
        let mut metrics = super::ExactMetrics::default();
        for (entry, reference) in self.entries.into_iter().zip(prepared) {
            let summary = anti_join_exact_states(entry.local, &reference.reference, max_samples)?;
            metrics.spill_bytes = metrics.spill_bytes.saturating_add(summary.spill_bytes);
            metrics.exact_runs = metrics.exact_runs.saturating_add(summary.runs);
            let sampled = summary.samples.len() as u64;
            let column = entry
                .plan
                .fields()
                .first()
                .map_or("$dataset", |field| field.name().as_str())
                .to_string();
            for sample in &summary.samples {
                record_lazy(validation, "references", &column, Some(sample.row), || {
                    format!(
                        "Reference rule `{}` found a key absent from `{}`",
                        entry.plan.name(),
                        entry.plan.reference()
                    )
                });
            }
            validation.violation_count = validation
                .violation_count
                .saturating_add(summary.missing.saturating_sub(sampled));
            outcomes.push(ReferenceOutcome {
                name: entry.plan.name().to_string(),
                reference: entry.plan.reference().to_string(),
                reference_fingerprint: reference.fingerprint,
                reference_rows: reference.reference_rows,
                reference_distinct_keys: summary.right_distinct,
                checked_distinct_keys: summary.left_distinct,
                missing_distinct_keys: summary.missing,
            });
        }
        Ok((outcomes, metrics))
    }
}

/// Encode one composite key into `output`, reporting `false` when it carried a null.
///
/// The key is positional and type-tagged, never named: the two sides of a reference rule
/// routinely use different column names for the same identity.
fn encode_key(
    batch: &RecordBatch,
    indexes: &[usize],
    row: usize,
    output: &mut Vec<u8>,
) -> Result<bool, ProofFrameError> {
    output.clear();
    for (ordinal, index) in indexes.iter().enumerate() {
        let array = batch.column(*index);
        if array.is_null(row) {
            return Ok(false);
        }
        output.extend_from_slice(&(ordinal as u64).to_le_bytes());
        super::dataset_state::append_scalar(array.as_ref(), row, output)?;
    }
    Ok(true)
}

/// Count the rows of every dataset a `row_count_delta` rule names.
///
/// Only the count is needed, so the reader is drained without building key state.
/// A rule whose dataset was never bound fails here, before the subject is scanned,
/// for the same reason an unbound reference does: a comparison that never happened
/// must not report as one that held.
pub(super) fn count_delta_references(
    plans: &[crate::RowCountDeltaPlan],
    bindings: &mut ReferenceBindings,
    cancellation: &CancellationToken,
) -> Result<BTreeMap<String, u64>, ProofFrameError> {
    let mut counted = BTreeMap::<String, u64>::new();
    for plan in plans {
        if counted.contains_key(plan.reference()) {
            continue;
        }
        let Some(reader) = bindings.take(plan.reference()) else {
            return Err(ProofFrameError::contract(
                ErrorCode::ReferenceUnbound,
                format!(
                    "Row count rule `{}` needs a dataset bound to `{}`",
                    plan.name(),
                    plan.reference()
                ),
                Some(format!("$.dataset_rules.row_count_delta[{}]", plan.name())),
            ));
        };
        let mut rows = 0u64;
        for batch in reader {
            cancellation.check()?;
            rows = rows.saturating_add(batch?.num_rows() as u64);
        }
        counted.insert(plan.reference().to_string(), rows);
    }
    Ok(counted)
}
