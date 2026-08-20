#![forbid(unsafe_code)]
//! Native ProofFrame engine for Arrow-backed data contracts.
//!
//! The crate exposes a Rust-native API by default. The Python extension module is available behind
//! the `python` feature and is enabled by the PyPI build configuration. Core invariants are stable
//! enough to publish as a beta: versioned canonical dataset fingerprints, disk-backed exact
//! keyed diffs, privacy-preserving PII findings, leakage checks, and signed proof receipts.

mod contract;
mod diff;
mod distinct;
mod encoding;
mod error;
pub mod evidence;
mod execution;
mod leakage;
mod pii;
#[cfg(feature = "python")]
mod python;
pub mod receipt;

pub use contract::{
    BoundAst, ColumnPlan, CompiledContract, CompiledRules, ContractAst, ContractVersion,
    KernelKind, NaNPolicy, NaNPolicyAst, RuleAst, TypedBound,
};
pub use diff::{DiffMetrics, DiffOptions, DiffOutput, SpillPolicy, diff_readers_with_options};
pub use distinct::{DuplicateSample, ExactMetrics, ExactState, ExactSummary, ValueKind, ValueRef};
pub use encoding::{Fingerprint, FingerprintOptions, FingerprintVersion};
pub use error::{ErrorCode, ProofFrameError};
pub use execution::{
    CancellationToken, ExecutionOptions, MemoryReservation, ResourceAccount, ResourceLimits,
    TempReservation, execute_reader, execute_reader_with_fingerprint,
};
pub use leakage::{LeakageOptions, detect_leakage_with_options};

/// Exercise the checksummed diff-partition decoder with a hard one-MiB input cap.
///
/// This API exists for `cargo-fuzz` and is only available with the `fuzzing` feature. It uses the
/// same parser and limits as production code; malformed input is expected to return an error.
#[cfg(feature = "fuzzing")]
pub fn fuzz_partition_bytes(input: &[u8]) -> Result<(), ProofFrameError> {
    diff::fuzz_partition_bytes(input)
}

#[cfg(miri)]
mod miri_tests;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use arrow::array::{
    Array, BinaryArray, BinaryViewArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    FixedSizeListArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    LargeBinaryArray, LargeListArray, LargeStringArray, ListArray, MapArray, StringArray,
    StringViewArray, StructArray, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
use arrow::datatypes::SchemaRef;
use arrow::record_batch::{RecordBatch, RecordBatchReader};
use arrow::util::display::array_value_to_string;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

const DEFAULT_MAX_FINDINGS: usize = 100;
/// Per-column summary produced while profiling a dataset.
#[derive(Debug, Serialize)]
pub struct ColumnProfile {
    /// Column name as declared in the Arrow schema.
    pub name: String,
    /// Arrow data type rendered as a stable string.
    pub data_type: String,
    /// Number of null values observed in the column.
    pub null_count: u64,
    /// Number of non-null values observed in the column.
    pub non_null_count: u64,
    /// Count of distinct canonical values seen in the column, when exact distinct is enabled.
    pub distinct_count: Option<usize>,
    /// Minimum numeric value, when the column parses as a number.
    pub min: Option<f64>,
    /// Maximum numeric value, when the column parses as a number.
    pub max: Option<f64>,
}

struct ColumnState {
    null_count: u64,
    non_null_count: u64,
    distinct: Option<ExactState>,
    min: Option<f64>,
    max: Option<f64>,
}

impl ColumnState {
    fn new(
        distinct_mode: DistinctMode,
        account: &ResourceAccount,
        directory: Option<&std::path::Path>,
        row_count_hint: Option<u64>,
    ) -> Result<Self, ProofFrameError> {
        Ok(Self {
            null_count: 0,
            non_null_count: 0,
            distinct: match distinct_mode {
                DistinctMode::None => None,
                DistinctMode::Exact => Some(ExactState::new(
                    ValueKind::Bytes,
                    account.child(
                        account.limits().max_memory_bytes,
                        account.limits().max_temp_bytes,
                    ),
                    directory
                        .expect("exact profiling owns a temporary directory")
                        .to_path_buf(),
                    row_count_hint,
                )?),
            },
            min: None,
            max: None,
        })
    }
}

/// Exact distinct counting can dominate large profiles, so callers can opt out.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DistinctMode {
    None,
    Exact,
}

impl DistinctMode {
    pub fn from_name(value: &str) -> Result<Self, ProofFrameError> {
        match value {
            "none" => Ok(Self::None),
            "exact" => Ok(Self::Exact),
            other => Err(ProofFrameError::InvalidContract(format!(
                "Unsupported distinct mode `{other}`; expected `none` or `exact`"
            ))),
        }
    }
}

/// Deterministic dataset profile with a canonical content fingerprint.
#[derive(Debug, Serialize)]
pub struct Profile {
    /// Total number of rows scanned across all record batches.
    pub rows: u64,
    /// Per-column profiles in schema order.
    pub columns: Vec<ColumnProfile>,
    /// `pf-fp-v1`-tagged BLAKE3 fingerprint of the ordered data.
    pub fingerprint: String,
}

/// Validation contract: per-column rules plus a bound on emitted findings.
#[derive(Debug, Deserialize, Default)]
pub struct Contract {
    /// Column name to its rule set.
    #[serde(default)]
    pub columns: HashMap<String, ColumnContract>,
    /// Maximum number of findings to retain; the true count is still reported.
    #[serde(default = "default_max_findings")]
    pub max_findings: usize,
}

fn default_max_findings() -> usize {
    DEFAULT_MAX_FINDINGS
}

/// Rule set applied to a single column during validation.
#[derive(Debug, Deserialize, Default)]
pub struct ColumnContract {
    /// Require the column to be present in the schema.
    #[serde(default)]
    pub required: bool,
    /// Reject null values in the column.
    #[serde(default)]
    pub not_null: bool,
    /// Reject duplicate values in the column.
    #[serde(default)]
    pub unique: bool,
    /// Inclusive lower bound for numeric values.
    pub min: Option<f64>,
    /// Inclusive upper bound for numeric values.
    pub max: Option<f64>,
    /// Regular expression each value must match.
    pub pattern: Option<String>,
    /// Allowlist the value must belong to.
    pub allowed: Option<HashSet<String>>,
}

/// A single rule violation with bounded, row-level evidence.
#[derive(Debug, Serialize)]
pub struct Finding {
    /// Rule that produced the finding (for example `not_null` or `unique`).
    pub rule: &'static str,
    /// Column the finding applies to.
    pub column: String,
    /// Zero-based row index, or `None` for schema-level findings.
    pub row: Option<u64>,
    /// Human-readable description of the violation.
    pub message: String,
}

/// Full validation result: findings plus the dataset profile and fingerprint.
#[derive(Debug, Serialize)]
pub struct ValidationReport {
    /// `true` when no violations were found.
    pub valid: bool,
    /// Total number of violations, even if `findings` was truncated.
    pub violation_count: u64,
    /// `true` when `findings` was capped by the contract's `max_findings`.
    pub truncated: bool,
    /// Bounded list of individual findings.
    pub findings: Vec<Finding>,
    /// Dataset profile and canonical fingerprint from the same pass.
    pub profile: Profile,
}

/// Accounted engine state for a compiled validation execution.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ExecutionMetrics {
    /// Peak bytes reserved from the operation memory budget.
    pub peak_memory_bytes: u64,
    /// Peak bytes reserved from the operation temporary-storage budget.
    pub peak_temp_bytes: u64,
    /// Checksummed exact-state bytes written to temporary runs.
    pub spill_bytes: u64,
    /// Number of sorted exact-state runs merged during finalization.
    pub exact_runs: u64,
    /// Unplanned `Vec` capacity growths in prepared engine buffers.
    pub capacity_growth_events: u64,
}

/// Rules-only validation result that skips profiling and fingerprinting.
#[derive(Debug, Serialize)]
pub struct FastValidationReport {
    /// `true` when no violations were found.
    pub valid: bool,
    /// Total number of violations, even if `findings` was truncated.
    pub violation_count: u64,
    /// `true` when `findings` was capped by the contract's `max_findings`.
    pub truncated: bool,
    /// Bounded list of individual findings.
    pub findings: Vec<Finding>,
    /// Total number of rows scanned.
    pub rows: u64,
    /// Evaluation mode identifier (`rules_only`).
    pub mode: &'static str,
    /// Accounted native state and spill activity for release diagnostics.
    pub metrics: ExecutionMetrics,
    /// Effective hard limits applied to this validation execution.
    pub resources: ResourceLimits,
    /// Schema-resolved typed plan identity used for this report.
    pub compiled_plan_digest: String,
    /// Arrow schema identity used for plan compilation and every scanned batch.
    pub schema_digest: String,
}

struct ValidationOutcome {
    findings: Vec<Finding>,
    violation_count: u64,
    truncated: bool,
}

struct ValidationState {
    findings: Vec<Finding>,
    violation_count: u64,
    max_findings: usize,
    finding_memory: Option<ResourceAccount>,
    finding_reservations: Vec<MemoryReservation>,
    resource_error: Option<ProofFrameError>,
}

impl ValidationState {
    fn new(max_findings: usize) -> Self {
        Self {
            findings: Vec::new(),
            violation_count: 0,
            max_findings,
            finding_memory: None,
            finding_reservations: Vec::new(),
            resource_error: None,
        }
    }

    fn new_accounted(max_findings: usize, account: ResourceAccount) -> Self {
        Self {
            finding_memory: Some(account),
            ..Self::new(max_findings)
        }
    }

    fn record(&mut self, finding: Finding) {
        self.violation_count += 1;
        if self.findings.len() < self.max_findings {
            if !self.reserve_finding(&finding) {
                return;
            }
            self.findings.push(finding);
        }
    }

    fn reserve_finding(&mut self, finding: &Finding) -> bool {
        let Some(account) = self.finding_memory.as_ref() else {
            return true;
        };
        if self.resource_error.is_some() {
            return false;
        }
        let vector_growth = if self.findings.len() == self.findings.capacity() {
            let next = if self.findings.capacity() == 0 {
                4
            } else {
                self.findings.capacity().saturating_mul(2)
            };
            next.saturating_sub(self.findings.capacity())
                .saturating_mul(std::mem::size_of::<Finding>())
        } else {
            0
        };
        let bytes = vector_growth
            .saturating_add(finding.column.capacity())
            .saturating_add(finding.message.capacity()) as u64;
        match account.try_reserve_memory(bytes) {
            Ok(reservation) => {
                self.finding_reservations.push(reservation);
                true
            }
            Err(error) => {
                self.resource_error = Some(error);
                false
            }
        }
    }

    fn check_resources(&mut self) -> Result<(), ProofFrameError> {
        match self.resource_error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn finish(self) -> ValidationOutcome {
        ValidationOutcome {
            truncated: self.violation_count as usize > self.findings.len(),
            violation_count: self.violation_count,
            findings: self.findings,
        }
    }
}

/// A row present in both datasets whose values changed.
#[derive(Debug, Serialize)]
pub struct ChangedRow {
    /// Human-readable business key of the changed row.
    pub key: String,
    /// Names of the columns whose values differ.
    pub columns: Vec<String>,
}

/// Exact keyed diff between two datasets.
#[derive(Debug, Serialize)]
pub struct DiffReport {
    /// Business key columns used to align rows.
    pub keys: Vec<String>,
    /// Row count of the "before" dataset.
    pub before_rows: usize,
    /// Row count of the "after" dataset.
    pub after_rows: usize,
    /// Number of keys present only in "after".
    pub added_count: usize,
    /// Number of keys present only in "before".
    pub removed_count: usize,
    /// Number of keys present in both with differing values.
    pub changed_count: usize,
    /// Sorted keys present only in "after".
    pub added_keys: Vec<String>,
    /// Sorted keys present only in "before".
    pub removed_keys: Vec<String>,
    /// Per-key column-level changes, sorted by key.
    pub changed: Vec<ChangedRow>,
    /// True when exact counts exceed the bounded in-memory samples.
    pub truncated: bool,
    /// Resource and output counters for the diff execution.
    pub metrics: DiffMetrics,
}

/// A single PII detection that never carries the matched value.
#[derive(Debug, Serialize)]
pub struct PiiFinding {
    /// Detected PII class (for example `email` or `payment_card`).
    pub kind: &'static str,
    /// Detector confidence (`high` or `medium`).
    pub confidence: &'static str,
    /// Column the value was found in.
    pub column: String,
    /// Zero-based row index of the value.
    pub row: u64,
    /// Domain-separated BLAKE3 fingerprint of the value, never the value itself.
    pub value_fingerprint: String,
}

/// Aggregated PII scan result with bounded per-cell findings.
#[derive(Debug, Serialize)]
pub struct PiiReport {
    /// `true` when at least one PII value was detected.
    pub detected: bool,
    /// Total number of rows scanned.
    pub scanned_rows: u64,
    /// Total number of PII detections, even if `findings` was truncated.
    pub finding_count: usize,
    /// Detection counts grouped by PII class.
    pub counts_by_kind: BTreeMap<&'static str, usize>,
    /// `true` when `findings` was capped by `max_findings`.
    pub truncated: bool,
    /// Bounded list of individual detections.
    pub findings: Vec<PiiFinding>,
    /// Whether fingerprints are stable under a caller key or unlinkable across runs.
    pub fingerprint_mode: PiiFingerprintMode,
    /// Non-secret identifier for key rotation and provenance.
    pub key_id: String,
}

/// Linkability policy for redacted PII value fingerprints.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PiiFingerprintMode {
    Stable,
    Unlinkable,
}

/// Secret-bearing scan configuration. The key is deliberately never serializable.
pub struct PiiFingerprintOptions {
    mode: PiiFingerprintMode,
    key: [u8; 32],
    key_id: String,
}

impl PiiFingerprintOptions {
    /// Generate a fresh per-run key so equal values cannot be linked across scans.
    pub fn unlinkable() -> Result<Self, ProofFrameError> {
        let mut key = [0_u8; 32];
        getrandom::fill(&mut key)
            .map_err(|error| ProofFrameError::InvalidContract(error.to_string()))?;
        let identifier = blake3::keyed_hash(&key, b"proofframe:pii-key-id:v2\0");
        let mut key_id = String::with_capacity(20);
        key_id.push_str("run:");
        key_id.push_str(&identifier.to_hex()[..16]);
        Ok(Self {
            mode: PiiFingerprintMode::Unlinkable,
            key,
            key_id,
        })
    }

    /// Use a caller-managed 256-bit key for stable fingerprints across runs.
    pub fn stable(key: [u8; 32], key_id: impl Into<String>) -> Result<Self, ProofFrameError> {
        let key_id = key_id.into();
        if key_id.trim().is_empty() {
            return Err(ProofFrameError::InvalidContract(
                "stable PII fingerprints require a non-empty key_id".to_string(),
            ));
        }
        Ok(Self {
            mode: PiiFingerprintMode::Stable,
            key,
            key_id,
        })
    }

    /// Decode a URL-safe, unpadded base64 32-byte caller key.
    pub fn stable_base64(key: &str, key_id: impl Into<String>) -> Result<Self, ProofFrameError> {
        let bytes = URL_SAFE_NO_PAD.decode(key).map_err(|_| {
            ProofFrameError::InvalidContract(
                "stable PII fingerprint_key must encode exactly 32 bytes".to_string(),
            )
        })?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| {
            ProofFrameError::InvalidContract(
                "stable PII fingerprint_key must encode exactly 32 bytes".to_string(),
            )
        })?;
        Self::stable(key, key_id)
    }
}

/// Train/test overlap result that exposes only hashed sample identifiers.
#[derive(Debug, Serialize)]
pub struct LeakageReport {
    /// `true` when any overlap was found between the two datasets.
    pub detected: bool,
    /// Detection mode: `key` for keyed overlap or `full_row` for exact rows.
    pub mode: &'static str,
    /// Key columns used for overlap, empty in full-row mode.
    pub keys: Vec<String>,
    /// Number of rows in the train dataset.
    pub train_rows: usize,
    /// Number of rows in the test dataset.
    pub test_rows: usize,
    /// Number of overlapping identities.
    pub overlap_count: usize,
    /// Overlap as a fraction of distinct train identities.
    pub train_overlap_rate: f64,
    /// Overlap as a fraction of distinct test identities.
    pub test_overlap_rate: f64,
    /// Sorted, bounded sample of hashed overlapping identifiers.
    pub sample_fingerprints: Vec<String>,
    /// `true` when the sample list was capped by `max_samples`.
    pub truncated: bool,
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

fn canonical_value_bytes(array: &dyn Array, row: usize) -> Result<Vec<u8>, ProofFrameError> {
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
    if let Some(values) = array.as_any().downcast_ref::<StringViewArray>() {
        let value = values.value(row).as_bytes();
        let mut encoded = Vec::with_capacity(9 + value.len());
        encoded.push(28);
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
    if let Some(values) = array.as_any().downcast_ref::<BinaryViewArray>() {
        let value = values.value(row);
        let mut encoded = Vec::with_capacity(9 + value.len());
        encoded.push(29);
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value);
        return Ok(encoded);
    }

    // Nested types recurse into their children. Element counts and per-element
    // length prefixes keep boundaries unambiguous, and each nested kind carries a
    // distinct tag so a one-element list cannot collide with its bare element.
    if let Some(values) = array.as_any().downcast_ref::<ListArray>() {
        return encode_child_sequence(23, values.value(row).as_ref());
    }
    if let Some(values) = array.as_any().downcast_ref::<LargeListArray>() {
        return encode_child_sequence(24, values.value(row).as_ref());
    }
    if let Some(values) = array.as_any().downcast_ref::<FixedSizeListArray>() {
        return encode_child_sequence(25, values.value(row).as_ref());
    }
    if let Some(values) = array.as_any().downcast_ref::<StructArray>() {
        let mut encoded = vec![26];
        encoded.extend_from_slice(&(values.num_columns() as u64).to_le_bytes());
        for column in values.columns() {
            let element = canonical_value_bytes(column.as_ref(), row)?;
            encoded.extend_from_slice(&(element.len() as u64).to_le_bytes());
            encoded.extend_from_slice(&element);
        }
        return Ok(encoded);
    }
    if let Some(values) = array.as_any().downcast_ref::<MapArray>() {
        // A map row is a struct array of {key, value} entries in physical order.
        return encode_child_sequence(27, &values.value(row));
    }

    Err(ProofFrameError::UnsupportedType(
        array.data_type().to_string(),
    ))
}

/// Encode every element of a nested child array with a tag, an element count,
/// and per-element length prefixes.
fn encode_child_sequence(tag: u8, child: &dyn Array) -> Result<Vec<u8>, ProofFrameError> {
    let mut encoded = vec![tag];
    encoded.extend_from_slice(&(child.len() as u64).to_le_bytes());
    for index in 0..child.len() {
        let element = canonical_value_bytes(child, index)?;
        encoded.extend_from_slice(&(element.len() as u64).to_le_bytes());
        encoded.extend_from_slice(&element);
    }
    Ok(encoded)
}

fn value_for_rules(array: &dyn Array, row: usize) -> Result<String, ProofFrameError> {
    array_value_to_string(array, row).map_err(Into::into)
}

fn update_hash(
    hasher: &mut blake3::Hasher,
    column: usize,
    array: &dyn Array,
    row: usize,
) -> Result<(), ProofFrameError> {
    hasher.update(b"pf-cell-v1\0");
    hasher.update(&(column as u64).to_le_bytes());
    let encoded = canonical_value_bytes(array, row)?;
    update_len_prefixed(hasher, &encoded);
    Ok(())
}

fn inspect_batches<R>(
    reader: R,
    contract: Option<&Contract>,
    distinct_mode: DistinctMode,
    resources: ResourceLimits,
    row_count_hint: Option<u64>,
) -> Result<(Profile, ValidationOutcome), ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let resource_root = ResourceAccount::root(resources);
    let distinct_directory = (distinct_mode == DistinctMode::Exact)
        .then(tempfile::TempDir::new)
        .transpose()?;
    let mut states = schema
        .fields()
        .iter()
        .map(|_| {
            ColumnState::new(
                distinct_mode,
                &resource_root,
                distinct_directory.as_ref().map(tempfile::TempDir::path),
                row_count_hint,
            )
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
    let mut rules = prepare_validation(&schema, contract)?;
    let mut rows = 0_u64;
    let mut hasher = profile_hasher(&schema);
    for maybe_batch in reader {
        let batch = maybe_batch?;
        BatchInspection {
            contract,
            states: &mut states,
            rules: &mut rules,
            hasher: &mut hasher,
        }
        .inspect(&batch, &schema, rows)?;
        rows += batch.num_rows() as u64;
    }
    let columns = finish_column_profiles(&schema, states)?;
    Ok((
        Profile {
            rows,
            columns,
            fingerprint: format!("pf-fp-v1:{}", hasher.finalize().to_hex()),
        },
        rules.validation.finish(),
    ))
}

struct RuleValidation {
    validation: ValidationState,
    seen_unique: HashMap<String, HashSet<Vec<u8>>>,
    patterns: HashMap<String, Regex>,
}

fn prepare_validation(
    schema: &SchemaRef,
    contract: Option<&Contract>,
) -> Result<RuleValidation, ProofFrameError> {
    let max_findings = contract.map_or(DEFAULT_MAX_FINDINGS, |value| value.max_findings);
    let mut validation = ValidationState::new(max_findings);
    let mut seen_unique = HashMap::new();
    let mut patterns = HashMap::new();
    if let Some(contract) = contract {
        for (name, rule) in &contract.columns {
            if rule.required && schema.index_of(name).is_err() {
                validation.record(Finding {
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
                patterns.insert(name.clone(), Regex::new(pattern)?);
            }
        }
    }
    Ok(RuleValidation {
        validation,
        seen_unique,
        patterns,
    })
}

fn profile_hasher(schema: &SchemaRef) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pf-fp-v1\0");
    for field in schema.fields() {
        update_schema_hash(
            &mut hasher,
            field.name(),
            &field.data_type().to_string(),
            field.is_nullable(),
        );
    }
    hasher.update(b"pf-fp-body-v1\0");
    hasher
}

struct BatchInspection<'a> {
    contract: Option<&'a Contract>,
    states: &'a mut [ColumnState],
    rules: &'a mut RuleValidation,
    hasher: &'a mut blake3::Hasher,
}

impl BatchInspection<'_> {
    fn inspect(
        &mut self,
        batch: &RecordBatch,
        schema: &SchemaRef,
        row_offset: u64,
    ) -> Result<(), ProofFrameError> {
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ProofFrameError::SchemaMismatch(
                "record batch schema changed during profiling".to_string(),
            ));
        }
        for row in 0..batch.num_rows() {
            let global_row = row_offset + row as u64;
            for (column_index, array) in batch.columns().iter().enumerate() {
                self.inspect_cell(
                    array.as_ref(),
                    schema.field(column_index).name(),
                    column_index,
                    row,
                    global_row,
                )?;
            }
        }
        Ok(())
    }

    fn inspect_cell(
        &mut self,
        array: &dyn Array,
        column_name: &str,
        column_index: usize,
        row: usize,
        global_row: u64,
    ) -> Result<(), ProofFrameError> {
        update_hash(self.hasher, column_index, array, row)?;
        let rule = self
            .contract
            .and_then(|value| value.columns.get(column_name));
        let state = &mut self.states[column_index];
        if array.is_null(row) {
            state.null_count += 1;
            if rule.is_some_and(|value| value.not_null) {
                self.rules.validation.record(Finding {
                    rule: "not_null",
                    column: column_name.to_string(),
                    row: Some(global_row),
                    message: "Null value is not allowed".to_string(),
                });
            }
            return Ok(());
        }

        state.non_null_count += 1;
        let value_key = canonical_value_bytes(array, row)?;
        if let Some(distinct) = &mut state.distinct {
            distinct.insert(ValueRef::Bytes(&value_key), global_row)?;
        }
        if let Some(number) = numeric_value(array, row)? {
            state.min = Some(state.min.map_or(number, |current| current.min(number)));
            state.max = Some(state.max.map_or(number, |current| current.max(number)));
        }
        if let Some(rule) = rule {
            self.rules.validate_column_rule(
                array,
                row,
                global_row,
                column_name,
                &value_key,
                rule,
            )?;
        }
        Ok(())
    }
}

impl RuleValidation {
    fn validate_column_rule(
        &mut self,
        array: &dyn Array,
        row: usize,
        global_row: u64,
        column_name: &str,
        value_key: &[u8],
        rule: &ColumnContract,
    ) -> Result<(), ProofFrameError> {
        if rule.unique
            && !self
                .seen_unique
                .get_mut(column_name)
                .expect("unique set initialized")
                .insert(value_key.to_vec())
        {
            let value = value_for_rules(array, row)?;
            self.validation.record(Finding {
                rule: "unique",
                column: column_name.to_string(),
                row: Some(global_row),
                message: format!("Duplicate value `{value}`"),
            });
        }
        validate_numeric_rule(
            array,
            row,
            global_row,
            column_name,
            rule,
            &mut self.validation,
        )?;
        validate_text_rule(
            array,
            row,
            global_row,
            column_name,
            rule,
            &self.patterns,
            &mut self.validation,
        )
    }
}

fn validate_numeric_rule(
    array: &dyn Array,
    row: usize,
    global_row: u64,
    column_name: &str,
    rule: &ColumnContract,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    if rule.min.is_none() && rule.max.is_none() {
        return Ok(());
    }
    let numeric = numeric_value(array, row)?;
    if rule
        .min
        .is_some_and(|minimum| numeric.is_some_and(|value| value < minimum))
    {
        let value = value_for_rules(array, row)?;
        validation.record(Finding {
            rule: "min",
            column: column_name.to_string(),
            row: Some(global_row),
            message: format!(
                "Value `{value}` is below {}",
                rule.min.expect("minimum exists")
            ),
        });
    }
    if rule
        .max
        .is_some_and(|maximum| numeric.is_some_and(|value| value > maximum))
    {
        let value = value_for_rules(array, row)?;
        validation.record(Finding {
            rule: "max",
            column: column_name.to_string(),
            row: Some(global_row),
            message: format!(
                "Value `{value}` is above {}",
                rule.max.expect("maximum exists")
            ),
        });
    }
    Ok(())
}

fn validate_text_rule(
    array: &dyn Array,
    row: usize,
    global_row: u64,
    column_name: &str,
    rule: &ColumnContract,
    patterns: &HashMap<String, Regex>,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    let pattern = patterns.get(column_name);
    if pattern.is_none() && rule.allowed.is_none() {
        return Ok(());
    }
    let value = value_for_rules(array, row)?;
    if pattern.is_some_and(|regex| !regex.is_match(&value)) {
        validation.record(Finding {
            rule: "pattern",
            column: column_name.to_string(),
            row: Some(global_row),
            message: format!(
                "Value `{value}` does not match `{}`",
                pattern.expect("pattern exists").as_str()
            ),
        });
    }
    if rule
        .allowed
        .as_ref()
        .is_some_and(|allowed| !allowed.contains(&value))
    {
        validation.record(Finding {
            rule: "allowed",
            column: column_name.to_string(),
            row: Some(global_row),
            message: format!("Value `{value}` is not in the allowlist"),
        });
    }
    Ok(())
}

fn finish_column_profiles(
    schema: &SchemaRef,
    states: Vec<ColumnState>,
) -> Result<Vec<ColumnProfile>, ProofFrameError> {
    schema
        .fields()
        .iter()
        .zip(states)
        .map(|(field, state)| {
            let distinct_count = state
                .distinct
                .map(ExactState::finish)
                .transpose()?
                .map(|summary| summary.distinct_count as usize);
            Ok(ColumnProfile {
                name: field.name().clone(),
                data_type: field.data_type().to_string(),
                null_count: state.null_count,
                non_null_count: state.non_null_count,
                distinct_count,
                min: state.min,
                max: state.max,
            })
        })
        .collect()
}

fn privacy_fingerprint(key: &[u8; 32], value: &str) -> String {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(b"proofframe:privacy:v2\0");
    hasher.update(value.as_bytes());
    format!("pf-pii-v2:{}", hasher.finalize().to_hex())
}

fn numeric_value(array: &dyn Array, row: usize) -> Result<Option<f64>, ProofFrameError> {
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        Ok(Some(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<Float32Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<Int8Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<Int16Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<Int32Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt8Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt16Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt32Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt64Array>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<TimestampSecondArray>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<TimestampMillisecondArray>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<TimestampMicrosecondArray>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<TimestampNanosecondArray>() {
        Ok(Some(values.value(row) as f64))
    } else {
        Ok(None)
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

fn validate_fast_batches<R>(
    reader: R,
    contract: &Contract,
) -> Result<FastValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let ast = legacy_contract_ast(contract)?;
    let plan = CompiledContract::compile(&ast, schema.as_ref())?;
    execute_reader(reader, &plan, &ExecutionOptions::default())
}

fn legacy_contract_ast(contract: &Contract) -> Result<ContractAst, ProofFrameError> {
    let mut columns = BTreeMap::new();
    for (name, rule) in &contract.columns {
        columns.insert(
            name.clone(),
            RuleAst {
                required: rule.required,
                not_null: rule.not_null,
                unique: rule.unique,
                min: rule.min.map(legacy_bound).transpose()?,
                max: rule.max.map(legacy_bound).transpose()?,
                nan: None,
                pattern: rule.pattern.clone(),
                allowed: rule
                    .allowed
                    .as_ref()
                    .map(|values| values.iter().cloned().collect()),
            },
        );
    }
    Ok(ContractAst {
        version: ContractVersion::V1,
        columns,
        max_findings: contract.max_findings,
    })
}

fn legacy_bound(value: f64) -> Result<BoundAst, ProofFrameError> {
    if !value.is_finite() {
        return Err(ProofFrameError::contract(
            ErrorCode::ContractInvalidBound,
            "Legacy floating-point bounds must be finite",
            None,
        ));
    }
    if value.fract() == 0.0 {
        Ok(BoundAst::Text(format!("{value:.0}")))
    } else {
        Ok(BoundAst::Number(
            serde_json::Number::from_f64(value).expect("finite values are valid JSON numbers"),
        ))
    }
}

/// Profile Arrow record batches and return typed Rust metadata.
pub fn profile_reader<R>(reader: R) -> Result<Profile, ProofFrameError>
where
    R: RecordBatchReader,
{
    profile_reader_with_distinct(reader, DistinctMode::None)
}

/// Profile Arrow record batches with configurable exact distinct counting.
pub fn profile_reader_with_distinct<R>(
    reader: R,
    distinct_mode: DistinctMode,
) -> Result<Profile, ProofFrameError>
where
    R: RecordBatchReader,
{
    profile_reader_with_resources(reader, distinct_mode, ResourceLimits::default())
}

/// Profile with a hard resource budget for optional exact distinct state.
pub fn profile_reader_with_resources<R>(
    reader: R,
    distinct_mode: DistinctMode,
    resources: ResourceLimits,
) -> Result<Profile, ProofFrameError>
where
    R: RecordBatchReader,
{
    profile_reader_with_resources_and_hint(reader, distinct_mode, resources, None)
}

/// Profile with hard resource budgets and an exact, non-consuming row-count hint.
pub fn profile_reader_with_resources_and_hint<R>(
    reader: R,
    distinct_mode: DistinctMode,
    resources: ResourceLimits,
    row_count_hint: Option<u64>,
) -> Result<Profile, ProofFrameError>
where
    R: RecordBatchReader,
{
    inspect_batches(reader, None, distinct_mode, resources, row_count_hint)
        .map(|(profile, _)| profile)
}

fn fingerprint_batches<R>(reader: R) -> Result<(u64, String), ProofFrameError>
where
    R: RecordBatchReader,
{
    let fingerprint = encoding::fingerprint_v1(reader)?;
    Ok((fingerprint.rows(), fingerprint.to_tagged_string()))
}

/// Return only the canonical dataset fingerprint, without profiling or exact distinct state.
pub fn fingerprint_reader<R>(reader: R) -> Result<String, ProofFrameError>
where
    R: RecordBatchReader,
{
    fingerprint_batches(reader).map(|(_, fingerprint)| fingerprint)
}

/// Return a typed canonical fingerprint using an explicitly selected version.
pub fn fingerprint_reader_with_options<R>(
    reader: R,
    options: &FingerprintOptions,
) -> Result<Fingerprint, ProofFrameError>
where
    R: RecordBatchReader,
{
    encoding::fingerprint(reader, options)
}

/// Validate Arrow record batches with the full profiling path.
pub fn validate_reader<R>(
    reader: R,
    contract: &Contract,
) -> Result<ValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let (profile, outcome) = inspect_batches(
        reader,
        Some(contract),
        DistinctMode::Exact,
        ResourceLimits::default(),
        None,
    )?;
    Ok(ValidationReport {
        valid: outcome.violation_count == 0,
        violation_count: outcome.violation_count,
        truncated: outcome.truncated,
        findings: outcome.findings,
        profile,
    })
}

/// Validate Arrow record batches with the rules-only fast path.
pub fn validate_fast_reader<R>(
    reader: R,
    contract: &Contract,
) -> Result<FastValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    validate_fast_batches(reader, contract)
}

/// Compute an exact keyed diff between two Arrow readers.
pub fn diff_readers<B, A>(
    before: B,
    after: A,
    keys: &[String],
) -> Result<DiffReport, ProofFrameError>
where
    B: RecordBatchReader,
    A: RecordBatchReader,
{
    diff::diff_readers_with_options(before, after, keys, &DiffOptions::default())
}

/// Scan Arrow record batches for high-signal PII patterns.
pub fn scan_pii_reader<R>(reader: R, max_findings: usize) -> Result<PiiReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    scan_pii_reader_with_options(reader, max_findings, &PiiFingerprintOptions::unlinkable()?)
}

/// Scan for PII using an explicit linkability and key-management policy.
pub fn scan_pii_reader_with_options<R>(
    reader: R,
    max_findings: usize,
    fingerprint: &PiiFingerprintOptions,
) -> Result<PiiReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let detector = pii::detector();
    let mut findings = Vec::new();
    let mut counts = BTreeMap::new();
    let mut scanned_rows = 0_u64;
    let mut total_findings = 0_usize;
    for maybe_batch in reader {
        let batch = maybe_batch?;
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ProofFrameError::SchemaMismatch(
                "record batch schema changed during PII scanning".to_string(),
            ));
        }
        for row in 0..batch.num_rows() {
            for (column, array) in batch.columns().iter().enumerate() {
                if array.is_null(row) {
                    continue;
                }
                let value = if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
                    Cow::Borrowed(values.value(row))
                } else if let Some(values) = array.as_any().downcast_ref::<LargeStringArray>() {
                    Cow::Borrowed(values.value(row))
                } else {
                    Cow::Owned(value_for_rules(array.as_ref(), row)?)
                };
                if let Some(classification) =
                    detector.classify_cell(value.as_ref(), is_numeric_array(array.as_ref()))
                {
                    total_findings += 1;
                    *counts.entry(classification.kind).or_insert(0) += 1;
                    if findings.len() < max_findings {
                        findings.push(PiiFinding {
                            kind: classification.kind,
                            confidence: classification.confidence,
                            column: schema.field(column).name().clone(),
                            row: scanned_rows + row as u64,
                            value_fingerprint: privacy_fingerprint(
                                &fingerprint.key,
                                value.as_ref(),
                            ),
                        });
                    }
                }
            }
        }
        scanned_rows += batch.num_rows() as u64;
    }
    Ok(PiiReport {
        detected: total_findings > 0,
        scanned_rows,
        finding_count: total_findings,
        counts_by_kind: counts,
        truncated: total_findings > findings.len(),
        findings,
        fingerprint_mode: fingerprint.mode,
        key_id: fingerprint.key_id.clone(),
    })
}

/// Detect train/test row or key leakage between two Arrow readers.
pub fn detect_leakage_readers<TR, TE>(
    train: TR,
    test: TE,
    keys: &[String],
    max_samples: usize,
) -> Result<LeakageReport, ProofFrameError>
where
    TR: RecordBatchReader,
    TE: RecordBatchReader,
{
    detect_leakage_with_options(
        train,
        test,
        keys,
        &LeakageOptions {
            max_samples,
            ..LeakageOptions::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow::array::ArrayRef;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::{RecordBatch, RecordBatchIterator};
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

            let (_, full_outcome) =
                inspect_batches(
                    reader_from_batch(batch.clone()),
                    Some(&contract),
                    DistinctMode::Exact,
                    ResourceLimits::default(),
                    None,
                )
                    .unwrap();
            let fast_report = validate_fast_batches(reader_from_batch(batch), &contract).unwrap();

            prop_assert_eq!(
                finding_signature(&full_outcome.findings),
                finding_signature(&fast_report.findings)
            );
            prop_assert_eq!(full_outcome.violation_count, fast_report.violation_count);
            prop_assert_eq!(full_outcome.truncated, fast_report.truncated);
        }
    }

    #[test]
    fn nested_columns_fingerprint_without_error() {
        use arrow::array::{Int64Builder, ListBuilder, StructArray};

        fn nested_batch(second: i64) -> RecordBatch {
            let mut list_builder = ListBuilder::new(Int64Builder::new());
            list_builder.values().append_value(1);
            list_builder.values().append_value(2);
            list_builder.append(true);
            list_builder.values().append_value(second);
            list_builder.append(true);
            let tags = Arc::new(list_builder.finish()) as ArrayRef;

            let group = StructArray::from(vec![
                (
                    Arc::new(Field::new("id", DataType::Int64, false)),
                    Arc::new(Int64Array::from(vec![10_i64, 20])) as ArrayRef,
                ),
                (
                    Arc::new(Field::new("team", DataType::Utf8, false)),
                    Arc::new(StringArray::from(vec!["a", "b"])) as ArrayRef,
                ),
            ]);

            let schema = Arc::new(Schema::new(vec![
                Field::new("tags", tags.data_type().clone(), true),
                Field::new("group", group.data_type().clone(), false),
            ]));
            RecordBatch::try_new(schema, vec![tags, Arc::new(group) as ArrayRef]).unwrap()
        }

        let first = profile_reader(reader_from_batch(nested_batch(3))).unwrap();
        let repeat = profile_reader(reader_from_batch(nested_batch(3))).unwrap();
        let different = profile_reader(reader_from_batch(nested_batch(99))).unwrap();

        assert!(first.fingerprint.starts_with("pf-fp-v1:"));
        assert_eq!(first.fingerprint, repeat.fingerprint);
        assert_ne!(first.fingerprint, different.fingerprint);
    }

    fn int_string_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![1_i64, 2, 3])) as ArrayRef,
                Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])) as ArrayRef,
            ],
        )
        .unwrap()
    }

    /// Golden fingerprint. If the canonical encoding changes, this pin must be
    /// updated together with a new `pf-fp` tag — a silent change is a bug.
    #[test]
    fn fingerprint_is_pinned() {
        let fingerprint = profile_reader(reader_from_batch(int_string_batch()))
            .unwrap()
            .fingerprint;
        assert_eq!(
            fingerprint,
            "pf-fp-v1:4dc74e666725f040dea7e788827d0411e59c19a72b55f2ce27f22ed9a00afb42",
            "canonical fingerprint changed; update the pin and bump the tag if intentional"
        );
    }

    #[test]
    fn fingerprint_ignores_batch_boundaries() {
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
        let whole = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![1_i64, 2, 3, 4])) as ArrayRef],
        )
        .unwrap();
        let first = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef],
        )
        .unwrap();
        let second = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![3_i64, 4])) as ArrayRef],
        )
        .unwrap();

        let single = profile_reader(reader_from_batch(whole))
            .unwrap()
            .fingerprint;
        let split = profile_reader(RecordBatchIterator::new(
            vec![Ok(first), Ok(second)].into_iter(),
            schema,
        ))
        .unwrap()
        .fingerprint;
        assert_eq!(single, split);
    }

    #[test]
    fn typed_unique_semantics_are_explicit() {
        let timestamp_batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new(
                "ts",
                DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None),
                false,
            )])),
            vec![Arc::new(TimestampMicrosecondArray::from(vec![10_i64, 11, 10])) as ArrayRef],
        )
        .unwrap();
        let contract = Contract {
            columns: HashMap::from([(
                "ts".to_string(),
                ColumnContract {
                    unique: true,
                    ..ColumnContract::default()
                },
            )]),
            max_findings: 100,
        };
        let report = validate_fast_reader(reader_from_batch(timestamp_batch), &contract).unwrap();
        assert!(!report.valid);
        assert_eq!(report.findings[0].row, Some(2));

        let nan_a = f64::from_bits(0x7ff8_0000_0000_0001);
        let nan_b = f64::from_bits(0x7ff8_0000_0000_0002);
        let floats = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("v", DataType::Float64, false)])),
            vec![Arc::new(Float64Array::from(vec![-0.0, 0.0, nan_a, nan_b, nan_a])) as ArrayRef],
        )
        .unwrap();
        let contract = Contract {
            columns: HashMap::from([(
                "v".to_string(),
                ColumnContract {
                    unique: true,
                    ..ColumnContract::default()
                },
            )]),
            max_findings: 100,
        };
        let report = validate_fast_reader(reader_from_batch(floats), &contract).unwrap();
        assert_eq!(report.violation_count, 1);
        assert_eq!(report.findings[0].row, Some(4));
    }

    proptest! {
        #[test]
        fn fingerprint_tracks_data_changes(
            left in prop::collection::vec(any::<i64>(), 1..20),
            right in prop::collection::vec(any::<i64>(), 1..20),
        ) {
            let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
            let fingerprint = |data: &[i64]| {
                let batch = RecordBatch::try_new(
                    schema.clone(),
                    vec![Arc::new(Int64Array::from(data.to_vec())) as ArrayRef],
                )
                .unwrap();
                profile_reader(reader_from_batch(batch)).unwrap().fingerprint
            };
            let left_fp = fingerprint(&left);
            let right_fp = fingerprint(&right);
            if left == right {
                prop_assert_eq!(left_fp, right_fp);
            } else {
                prop_assert_ne!(left_fp, right_fp);
            }
        }
    }
}

#[cfg(feature = "python")]
#[pymodule]
fn _proofframe(module: &Bound<'_, PyModule>) -> PyResult<()> {
    python::register(module)
}
