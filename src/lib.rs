#![forbid(unsafe_code)]
//! Native ProofFrame engine for Arrow-backed data contracts.
//!
//! The crate exposes a Rust-native API by default. The Python extension module is available behind
//! the `python` feature and is enabled by the PyPI build configuration. Core invariants are stable
//! enough to publish as an alpha: `pf-fp-v1` canonical dataset fingerprints, disk-backed exact
//! keyed diffs, privacy-preserving PII findings, leakage checks, and signed proof receipts.

mod error;
mod pii;
pub mod receipt;

pub use error::ProofFrameError;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use ahash::RandomState;
use arrow::array::{
    Array, BinaryArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    FixedSizeListArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    LargeBinaryArray, LargeListArray, LargeStringArray, ListArray, MapArray, RecordBatch,
    StringArray, StructArray, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
#[cfg(feature = "python")]
use arrow::ffi_stream::ArrowArrayStreamReader;
#[cfg(feature = "python")]
use arrow::pyarrow::PyArrowType;
use arrow::record_batch::RecordBatchReader;
use arrow::util::display::array_value_to_string;
#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use regex::Regex;
use roaring::RoaringTreemap;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const DEFAULT_MAX_FINDINGS: usize = 100;
const DIFF_PARTITIONS: usize = 64;

type RowValues = Vec<Option<Vec<u8>>>;
struct RowEntry {
    display_key: String,
    values: RowValues,
    hash: String,
}

#[derive(Debug, Eq, PartialEq)]
struct ColumnSchema {
    name: String,
    data_type: String,
    nullable: bool,
}

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
    /// Count of distinct canonical values seen in the column.
    pub distinct_count: usize,
    /// Minimum numeric value, when the column parses as a number.
    pub min: Option<f64>,
    /// Maximum numeric value, when the column parses as a number.
    pub max: Option<f64>,
}

#[derive(Default)]
struct ColumnState {
    null_count: u64,
    non_null_count: u64,
    distinct: HashSet<Vec<u8>>,
    min: Option<f64>,
    max: Option<f64>,
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
}

impl ValidationState {
    fn new(max_findings: usize) -> Self {
        Self {
            findings: Vec::new(),
            violation_count: 0,
            max_findings,
        }
    }

    fn record(&mut self, finding: Finding) {
        self.violation_count += 1;
        if self.findings.len() < self.max_findings {
            self.findings.push(finding);
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

#[cfg(feature = "python")]
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
) -> Result<(Profile, ValidationOutcome), ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let mut states = schema
        .fields()
        .iter()
        .map(|_| ColumnState::default())
        .collect::<Vec<_>>();
    let mut seen_unique: HashMap<String, HashSet<Vec<u8>>> = HashMap::new();
    let mut patterns: HashMap<String, Regex> = HashMap::new();
    let max_findings = contract.map_or(DEFAULT_MAX_FINDINGS, |value| value.max_findings);
    let mut validation = ValidationState::new(max_findings);

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

    let mut rows = 0_u64;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pf-fp-v1\0");
    for field in schema.fields() {
        let data_type = field.data_type().to_string();
        update_schema_hash(&mut hasher, field.name(), &data_type, field.is_nullable());
    }
    hasher.update(b"pf-fp-body-v1\0");
    for maybe_batch in reader {
        let batch = maybe_batch?;
        for row in 0..batch.num_rows() {
            let global_row = rows + row as u64;
            for (column_index, array) in batch.columns().iter().enumerate() {
                let field = schema.field(column_index);
                let state = &mut states[column_index];
                if array.is_null(row) {
                    state.null_count += 1;
                    update_hash(&mut hasher, column_index, array.as_ref(), row)?;
                    if let Some(rule) = contract.and_then(|value| value.columns.get(field.name())) {
                        if rule.not_null {
                            validation.record(Finding {
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
                    {
                        validation.record(Finding {
                            rule: "unique",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: format!("Duplicate value `{value}`"),
                        });
                    }
                    if let Some(min) = rule.min {
                        if numeric_value(array.as_ref(), row)?.is_some_and(|number| number < min) {
                            validation.record(Finding {
                                rule: "min",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: format!("Value `{value}` is below {min}"),
                            });
                        }
                    }
                    if let Some(max) = rule.max {
                        if numeric_value(array.as_ref(), row)?.is_some_and(|number| number > max) {
                            validation.record(Finding {
                                rule: "max",
                                column: field.name().clone(),
                                row: Some(global_row),
                                message: format!("Value `{value}` is above {max}"),
                            });
                        }
                    }
                    if let Some(regex) = patterns.get(field.name()) {
                        if !regex.is_match(&value) {
                            validation.record(Finding {
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
                        if !allowed.contains(&value) {
                            validation.record(Finding {
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
        validation.finish(),
    ))
}

fn row_partition(key: &[u8]) -> usize {
    let digest = blake3::hash(key);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    (u64::from_le_bytes(bytes) as usize) % DIFF_PARTITIONS
}

fn schema_signature<R: RecordBatchReader>(reader: &R) -> Vec<ColumnSchema> {
    reader
        .schema()
        .fields()
        .iter()
        .map(|field| ColumnSchema {
            name: field.name().clone(),
            data_type: field.data_type().to_string(),
            nullable: field.is_nullable(),
        })
        .collect()
}

fn schema_names(signature: &[ColumnSchema]) -> Vec<String> {
    signature.iter().map(|field| field.name.clone()).collect()
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
        let value = canonical_value_bytes(array.as_ref(), row)?;
        canonical.extend_from_slice(&(*index as u64).to_le_bytes());
        canonical.extend_from_slice(&(value.len() as u64).to_le_bytes());
        canonical.extend_from_slice(&value);
        if array.is_null(row) {
            display.push("<null>".to_string());
        } else {
            display.push(value_for_rules(array.as_ref(), row)?);
        }
    }
    Ok((canonical, display.join("\u{1f}")))
}

fn write_u64(writer: &mut BufWriter<File>, value: u64) -> Result<(), ProofFrameError> {
    writer.write_all(&value.to_le_bytes()).map_err(Into::into)
}

fn read_u64(reader: &mut BufReader<File>) -> Result<Option<u64>, ProofFrameError> {
    let mut bytes = [0_u8; 8];
    match reader.read_exact(&mut bytes) {
        Ok(()) => Ok(Some(u64::from_le_bytes(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_bytes(writer: &mut BufWriter<File>, value: &[u8]) -> Result<(), ProofFrameError> {
    write_u64(writer, value.len() as u64)?;
    writer.write_all(value).map_err(Into::into)
}

fn read_bytes(reader: &mut BufReader<File>) -> Result<Vec<u8>, ProofFrameError> {
    let len = read_u64(reader)?.ok_or_else(|| {
        ProofFrameError::CorruptData("Truncated diff partition record".to_string())
    })?;
    let mut value = vec![0_u8; len as usize];
    reader.read_exact(&mut value)?;
    Ok(value)
}

fn write_row_record(
    writer: &mut BufWriter<File>,
    key: &[u8],
    entry: &RowEntry,
) -> Result<(), ProofFrameError> {
    write_bytes(writer, key)?;
    write_bytes(writer, entry.display_key.as_bytes())?;
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

fn read_row_record(
    reader: &mut BufReader<File>,
) -> Result<Option<(Vec<u8>, RowEntry)>, ProofFrameError> {
    let Some(key_len) = read_u64(reader)? else {
        return Ok(None);
    };
    let mut key = vec![0_u8; key_len as usize];
    reader.read_exact(&mut key)?;
    let display_key = String::from_utf8(read_bytes(reader)?)?;
    let hash = String::from_utf8(read_bytes(reader)?)?;
    let value_count = read_u64(reader)?.ok_or_else(|| {
        ProofFrameError::CorruptData("Truncated diff partition record".to_string())
    })?;
    let mut values = Vec::with_capacity(value_count as usize);
    for _ in 0..value_count {
        let len = read_u64(reader)?
            .ok_or_else(|| ProofFrameError::CorruptData("Truncated diff value".to_string()))?;
        if len == u64::MAX {
            values.push(None);
        } else {
            let mut value = vec![0_u8; len as usize];
            reader.read_exact(&mut value)?;
            values.push(Some(value));
        }
    }
    Ok(Some((
        key,
        RowEntry {
            display_key,
            values,
            hash,
        },
    )))
}

fn partition_rows<R>(
    reader: R,
    keys: &[String],
    directory: &Path,
    prefix: &str,
) -> Result<(Vec<ColumnSchema>, usize, Vec<PathBuf>), ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let key_indexes = keys
        .iter()
        .map(|key| {
            schema
                .index_of(key)
                .map_err(|_| ProofFrameError::MissingColumn(key.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signature = schema_signature(&reader);
    let paths = (0..DIFF_PARTITIONS)
        .map(|partition| directory.join(format!("{prefix}-{partition}.pfpart")))
        .collect::<Vec<_>>();
    let mut writers = paths
        .iter()
        .map(|path| File::create(path).map(BufWriter::new))
        .collect::<Result<Vec<_>, _>>()?;
    let mut row_count = 0_usize;
    for maybe_batch in reader {
        let batch: RecordBatch = maybe_batch?;
        for row in 0..batch.num_rows() {
            let (key, display_key) = diff_key(&batch, &key_indexes, row)?;
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
            write_row_record(
                &mut writers[partition],
                &key,
                &RowEntry {
                    display_key,
                    values,
                    hash,
                },
            )?;
            row_count += 1;
        }
    }
    for writer in &mut writers {
        writer.flush()?;
    }
    Ok((signature, row_count, paths))
}

fn process_diff_partition(
    before_path: &Path,
    after_path: &Path,
    column_names: &[String],
    added_keys: &mut Vec<String>,
    removed_keys: &mut Vec<String>,
    changed: &mut Vec<ChangedRow>,
) -> Result<(), ProofFrameError> {
    let mut before_rows: HashMap<Vec<u8>, RowEntry> = HashMap::new();
    let mut before_reader = BufReader::new(File::open(before_path)?);
    while let Some((key, entry)) = read_row_record(&mut before_reader)? {
        let display_key = entry.display_key.clone();
        if before_rows.insert(key, entry).is_some() {
            return Err(ProofFrameError::DuplicateKey(display_key));
        }
    }

    let mut seen_after: HashSet<Vec<u8>> = HashSet::new();
    let mut after_reader = BufReader::new(File::open(after_path)?);
    while let Some((key, entry)) = read_row_record(&mut after_reader)? {
        if !seen_after.insert(key.clone()) {
            return Err(ProofFrameError::DuplicateKey(entry.display_key.clone()));
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
                changed.push(ChangedRow {
                    key: entry.display_key,
                    columns,
                });
            }
        } else {
            added_keys.push(entry.display_key);
        }
    }

    removed_keys.extend(before_rows.iter().filter_map(|(key, entry)| {
        if seen_after.contains(key) {
            None
        } else {
            Some(entry.display_key.clone())
        }
    }));
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
) -> Result<(Vec<String>, usize, HashSet<String>), ProofFrameError>
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
                    .map_err(|_| ProofFrameError::MissingColumn(key.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut row_count = 0_usize;
    let mut ids = HashSet::new();
    for maybe_batch in reader {
        let batch = maybe_batch?;
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

fn numeric_value(array: &dyn Array, row: usize) -> Result<Option<f64>, ProofFrameError> {
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        Ok(Some(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<Float32Array>() {
        Ok(Some(f64::from(values.value(row))))
    } else if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        Ok(Some(values.value(row) as f64))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt64Array>() {
        Ok(Some(values.value(row) as f64))
    } else {
        Ok(array_value_to_string(array, row)?.parse::<f64>().ok())
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

fn push_duplicate(validation: &mut ValidationState, column: &str, row: u64) {
    validation.record(Finding {
        rule: "unique",
        column: column.to_string(),
        row: Some(row),
        message: "Duplicate value detected".to_string(),
    });
}

fn check_unique(
    state: &mut UniqueState,
    array: &dyn Array,
    column: &str,
    row_offset: u64,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    match state {
        UniqueState::Int64(seen) => {
            let values = array
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("type fixed from schema");
            for row in 0..values.len() {
                if values.is_valid(row) && !seen.insert((values.value(row) as u64) ^ (1_u64 << 63))
                {
                    push_duplicate(validation, column, row_offset + row as u64);
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
                    push_duplicate(validation, column, row_offset + row as u64);
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
                    push_duplicate(validation, column, row_offset + row as u64);
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
                    push_duplicate(validation, column, row_offset + row as u64);
                }
            }
        }
        UniqueState::Generic(seen) => {
            for row in 0..array.len() {
                if array.is_valid(row) {
                    let value = canonical_value_bytes(array, row)?;
                    if !seen.insert(value) {
                        push_duplicate(validation, column, row_offset + row as u64);
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
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        for row in 0..values.len() {
            if values.is_valid(row) {
                push_range_findings(
                    values.value(row),
                    rule,
                    column,
                    row_offset + row as u64,
                    validation,
                );
            }
        }
    } else {
        for row in 0..array.len() {
            if array.is_valid(row) {
                if let Some(value) = numeric_value(array, row)? {
                    push_range_findings(value, rule, column, row_offset + row as u64, validation);
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
    validation: &mut ValidationState,
) {
    if rule.min.is_some_and(|minimum| number < minimum) {
        validation.record(Finding {
            rule: "min",
            column: column.to_string(),
            row: Some(row),
            message: format!("Value is below {}", rule.min.unwrap()),
        });
    }
    if rule.max.is_some_and(|maximum| number > maximum) {
        validation.record(Finding {
            rule: "max",
            column: column.to_string(),
            row: Some(row),
            message: format!("Value is above {}", rule.max.unwrap()),
        });
    }
}

fn validate_fast_batches<R>(
    reader: R,
    contract: &Contract,
) -> Result<FastValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let mut validation = ValidationState::new(contract.max_findings);
    let mut patterns = HashMap::new();
    let mut unique_states: HashMap<String, UniqueState> = HashMap::new();
    for (name, rule) in &contract.columns {
        if rule.required && schema.index_of(name).is_err() {
            validation.record(Finding {
                rule: "required",
                column: name.clone(),
                row: None,
                message: format!("Required column `{name}` is missing"),
            });
        }
        if let Some(pattern) = &rule.pattern {
            patterns.insert(name.clone(), Regex::new(pattern)?);
        }
    }

    let mut rows = 0_u64;
    for maybe_batch in reader {
        let batch = maybe_batch?;
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
                    if array.is_null(row) {
                        validation.record(Finding {
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
                    &mut validation,
                )?;
            }
            if rule.min.is_some() || rule.max.is_some() {
                check_range(array.as_ref(), rule, field.name(), rows, &mut validation)?;
            }
            if rule.pattern.is_some() || rule.allowed.is_some() {
                for row in 0..batch.num_rows() {
                    if array.is_null(row) {
                        continue;
                    }
                    let value = value_for_rules(array.as_ref(), row)?;
                    if patterns
                        .get(field.name())
                        .is_some_and(|pattern| !pattern.is_match(&value))
                    {
                        validation.record(Finding {
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
                    {
                        validation.record(Finding {
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
    let outcome = validation.finish();
    Ok(FastValidationReport {
        valid: outcome.violation_count == 0,
        violation_count: outcome.violation_count,
        truncated: outcome.truncated,
        findings: outcome.findings,
        rows,
        mode: "rules_only",
    })
}

/// Profile Arrow record batches and return typed Rust metadata.
pub fn profile_reader<R>(reader: R) -> Result<Profile, ProofFrameError>
where
    R: RecordBatchReader,
{
    inspect_batches(reader, None).map(|(profile, _)| profile)
}

/// Validate Arrow record batches with the full profiling path.
pub fn validate_reader<R>(
    reader: R,
    contract: &Contract,
) -> Result<ValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let (profile, outcome) = inspect_batches(reader, Some(contract))?;
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
    if keys.is_empty() {
        return Err(ProofFrameError::NoKeyColumns);
    }
    let directory = TempDir::new()?;
    let (before_schema, before_count, before_paths) =
        partition_rows(before, keys, directory.path(), "before")?;
    let (after_schema, after_count, after_paths) =
        partition_rows(after, keys, directory.path(), "after")?;
    if before_schema != after_schema {
        return Err(ProofFrameError::SchemaMismatch(
            "normalize columns before row-level diff".to_string(),
        ));
    }
    let column_names = schema_names(&before_schema);

    let mut added_keys = Vec::new();
    let mut removed_keys = Vec::new();
    let mut changed = Vec::new();
    for (before_path, after_path) in before_paths.iter().zip(&after_paths) {
        process_diff_partition(
            before_path,
            after_path,
            &column_names,
            &mut added_keys,
            &mut removed_keys,
            &mut changed,
        )?;
    }
    added_keys.sort();
    removed_keys.sort();
    changed.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(DiffReport {
        keys: keys.to_vec(),
        before_rows: before_count,
        after_rows: after_count,
        added_count: added_keys.len(),
        removed_count: removed_keys.len(),
        changed_count: changed.len(),
        added_keys,
        removed_keys,
        changed,
    })
}

/// Scan Arrow record batches for high-signal PII patterns.
pub fn scan_pii_reader<R>(reader: R, max_findings: usize) -> Result<PiiReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let detector = pii::Detector::new()?;
    let mut findings = Vec::new();
    let mut counts = BTreeMap::new();
    let mut scanned_rows = 0_u64;
    let mut total_findings = 0_usize;
    for maybe_batch in reader {
        let batch = maybe_batch?;
        for row in 0..batch.num_rows() {
            for (column, array) in batch.columns().iter().enumerate() {
                if array.is_null(row) {
                    continue;
                }
                let value = value_for_rules(array.as_ref(), row)?;
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
    Ok(PiiReport {
        detected: total_findings > 0,
        scanned_rows,
        finding_count: total_findings,
        counts_by_kind: counts,
        truncated: total_findings > findings.len(),
        findings,
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
    let (train_names, train_rows, train_ids) = collect_leakage_ids(train, keys)?;
    let (test_names, test_rows, test_ids) = collect_leakage_ids(test, keys)?;
    if keys.is_empty() && train_names != test_names {
        return Err(ProofFrameError::SchemaMismatch(
            "full-row leakage detection requires identical columns".to_string(),
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
    Ok(LeakageReport {
        detected: overlap_count > 0,
        mode: if keys.is_empty() { "full_row" } else { "key" },
        keys: keys.to_vec(),
        train_rows,
        test_rows,
        overlap_count,
        train_overlap_rate: rate(overlap_count, train_ids.len()),
        test_overlap_rate: rate(overlap_count, test_ids.len()),
        sample_fingerprints: overlap,
        truncated,
    })
}

#[cfg(feature = "python")]
#[pyfunction]
fn profile_arrow(source: PyArrowType<ArrowArrayStreamReader>) -> PyResult<String> {
    let profile = profile_reader(source.0).map_err(py_err)?;
    serde_json::to_string(&profile).map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
fn validate_arrow(
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: &str,
) -> PyResult<String> {
    let contract: Contract = serde_json::from_str(contract_json)
        .map_err(|error| py_err(ProofFrameError::InvalidContract(error.to_string())))?;
    let report = validate_reader(source.0, &contract).map_err(py_err)?;
    serde_json::to_string(&report).map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
fn validate_fast_arrow(
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: &str,
) -> PyResult<String> {
    let contract: Contract = serde_json::from_str(contract_json)
        .map_err(|error| py_err(ProofFrameError::InvalidContract(error.to_string())))?;
    let report = validate_fast_batches(source.0, &contract).map_err(py_err)?;
    serde_json::to_string(&report).map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
fn diff_arrow(
    before: PyArrowType<ArrowArrayStreamReader>,
    after: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
) -> PyResult<String> {
    let report = diff_readers(before.0, after.0, &keys).map_err(py_err)?;
    serde_json::to_string(&report).map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (source, max_findings=100))]
fn scan_pii_arrow(
    source: PyArrowType<ArrowArrayStreamReader>,
    max_findings: usize,
) -> PyResult<String> {
    let report = scan_pii_reader(source.0, max_findings).map_err(py_err)?;
    serde_json::to_string(&report).map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (train, test, keys, max_samples=20))]
fn detect_leakage_arrow(
    train: PyArrowType<ArrowArrayStreamReader>,
    test: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
    max_samples: usize,
) -> PyResult<String> {
    let report = detect_leakage_readers(train.0, test.0, &keys, max_samples).map_err(py_err)?;
    serde_json::to_string(&report).map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
fn generate_signing_keypair() -> PyResult<String> {
    receipt::generate_keypair_json().map_err(py_err)
}

#[cfg(feature = "python")]
#[pyfunction]
fn sign_proof_receipt(report_json: &str, private_key: &str) -> PyResult<String> {
    receipt::sign_json(report_json, private_key).map_err(py_err)
}

#[cfg(feature = "python")]
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

            let (_, full_outcome) =
                inspect_batches(reader_from_batch(batch.clone()), Some(&contract)).unwrap();
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
