mod pii;
mod receipt;

use std::collections::{BTreeMap, HashMap, HashSet};

use arrow::array::{Array, RecordBatch};
use arrow::ffi_stream::ArrowArrayStreamReader;
use arrow::pyarrow::PyArrowType;
use arrow::record_batch::RecordBatchReader;
use arrow::util::display::array_value_to_string;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

const DEFAULT_MAX_FINDINGS: usize = 100;

type RowValues = Vec<Option<String>>;
type RowMap = HashMap<String, RowValues>;
type CollectedRows = (Vec<String>, RowMap);

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
    distinct: HashSet<String>,
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

fn update_hash(hasher: &mut blake3::Hasher, column: usize, value: Option<&str>) {
    hasher.update(&(column as u64).to_le_bytes());
    match value {
        Some(value) => {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        None => {
            hasher.update(&u64::MAX.to_le_bytes());
        }
    }
}

fn inspect_batches(
    reader: ArrowArrayStreamReader,
    contract: Option<&Contract>,
) -> Result<(Profile, Vec<Finding>), String> {
    let schema = reader.schema();
    let mut states = schema
        .fields()
        .iter()
        .map(|_| ColumnState::default())
        .collect::<Vec<_>>();
    let mut findings = Vec::new();
    let mut seen_unique: HashMap<String, HashSet<String>> = HashMap::new();
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
    for field in schema.fields() {
        hasher.update(&(field.name().len() as u64).to_le_bytes());
        hasher.update(field.name().as_bytes());
        let data_type = field.data_type().to_string();
        hasher.update(&(data_type.len() as u64).to_le_bytes());
        hasher.update(data_type.as_bytes());
        hasher.update(&[u8::from(field.is_nullable())]);
    }
    for maybe_batch in reader {
        let batch = maybe_batch.map_err(|error| error.to_string())?;
        for row in 0..batch.num_rows() {
            let global_row = rows + row as u64;
            for (column_index, array) in batch.columns().iter().enumerate() {
                let field = schema.field(column_index);
                let state = &mut states[column_index];
                if array.is_null(row) {
                    state.null_count += 1;
                    update_hash(&mut hasher, column_index, None);
                    if let Some(rule) = contract.and_then(|value| value.columns.get(field.name()))
                        && rule.not_null
                        && findings.len() < max_findings
                    {
                        findings.push(Finding {
                            rule: "not_null",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: "Null value is not allowed".to_string(),
                        });
                    }
                    continue;
                }

                state.non_null_count += 1;
                let value = array_value_to_string(array.as_ref(), row)
                    .map_err(|error| error.to_string())?;
                update_hash(&mut hasher, column_index, Some(&value));
                state.distinct.insert(value.clone());
                if let Ok(number) = value.parse::<f64>() {
                    state.min = Some(state.min.map_or(number, |current| current.min(number)));
                    state.max = Some(state.max.map_or(number, |current| current.max(number)));
                }

                if let Some(rule) = contract.and_then(|value| value.columns.get(field.name())) {
                    if rule.unique
                        && !seen_unique
                            .get_mut(field.name())
                            .expect("unique set initialized")
                            .insert(value.clone())
                        && findings.len() < max_findings
                    {
                        findings.push(Finding {
                            rule: "unique",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: format!("Duplicate value `{value}`"),
                        });
                    }
                    if let Some(min) = rule.min
                        && value.parse::<f64>().is_ok_and(|number| number < min)
                        && findings.len() < max_findings
                    {
                        findings.push(Finding {
                            rule: "min",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: format!("Value `{value}` is below {min}"),
                        });
                    }
                    if let Some(max) = rule.max
                        && value.parse::<f64>().is_ok_and(|number| number > max)
                        && findings.len() < max_findings
                    {
                        findings.push(Finding {
                            rule: "max",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: format!("Value `{value}` is above {max}"),
                        });
                    }
                    if let Some(regex) = patterns.get(field.name())
                        && !regex.is_match(&value)
                        && findings.len() < max_findings
                    {
                        findings.push(Finding {
                            rule: "pattern",
                            column: field.name().clone(),
                            row: Some(global_row),
                            message: format!("Value `{value}` does not match `{}`", regex.as_str()),
                        });
                    }
                    if let Some(allowed) = &rule.allowed
                        && !allowed.contains(&value)
                        && findings.len() < max_findings
                    {
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
            fingerprint: hasher.finalize().to_hex().to_string(),
        },
        findings,
    ))
}

fn collect_rows(reader: ArrowArrayStreamReader, keys: &[String]) -> Result<CollectedRows, String> {
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
        .collect();
    let mut rows = HashMap::new();
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
                        array_value_to_string(array.as_ref(), row).unwrap_or_default()
                    }
                })
                .collect::<Vec<_>>()
                .join("\u{1f}");
            let values = batch
                .columns()
                .iter()
                .map(|array| {
                    if array.is_null(row) {
                        None
                    } else {
                        array_value_to_string(array.as_ref(), row).ok()
                    }
                })
                .collect();
            if rows.insert(key.clone(), values).is_some() {
                return Err(format!("Duplicate key `{key}`; diff keys must be unique"));
            }
        }
    }
    Ok((names, rows))
}

fn privacy_fingerprint(value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proofframe:privacy:v1\0");
    hasher.update(value.as_bytes());
    hasher.finalize().to_hex()[..16].to_string()
}

fn collect_leakage_ids(
    reader: ArrowArrayStreamReader,
    keys: &[String],
) -> Result<(Vec<String>, usize, HashSet<String>), String> {
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
                if array.is_null(row) {
                    update_hash(&mut hasher, *index, None);
                } else {
                    let value = array_value_to_string(array.as_ref(), row)
                        .map_err(|error| error.to_string())?;
                    update_hash(&mut hasher, *index, Some(&value));
                }
            }
            ids.insert(hasher.finalize().to_hex().to_string());
            row_count += 1;
        }
    }
    Ok((names, row_count, ids))
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
fn diff_arrow(
    before: PyArrowType<ArrowArrayStreamReader>,
    after: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
) -> PyResult<String> {
    if keys.is_empty() {
        return Err(py_err("At least one key column is required"));
    }
    let (before_names, before_rows) = collect_rows(before.0, &keys).map_err(py_err)?;
    let (after_names, after_rows) = collect_rows(after.0, &keys).map_err(py_err)?;
    if before_names != after_names {
        return Err(py_err(
            "Schemas differ; normalize columns before row-level diff",
        ));
    }

    let mut added_keys = after_rows
        .keys()
        .filter(|key| !before_rows.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let mut removed_keys = before_rows
        .keys()
        .filter(|key| !after_rows.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let mut changed = Vec::new();
    for (key, before_values) in &before_rows {
        if let Some(after_values) = after_rows.get(key)
            && before_values != after_values
        {
            let columns = before_values
                .iter()
                .zip(after_values)
                .zip(&before_names)
                .filter(|((before, after), _)| before != after)
                .map(|(_, name)| name.clone())
                .collect();
            changed.push(ChangedRow {
                key: key.clone(),
                columns,
            });
        }
    }
    added_keys.sort();
    removed_keys.sort();
    changed.sort_by(|left, right| left.key.cmp(&right.key));
    let report = DiffReport {
        keys,
        before_rows: before_rows.len(),
        after_rows: after_rows.len(),
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
                let value = array_value_to_string(array.as_ref(), row).map_err(py_err)?;
                if let Some(classification) = detector.classify(&value) {
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

#[pymodule]
fn _proofframe(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(profile_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(validate_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(diff_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(scan_pii_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(detect_leakage_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(generate_signing_keypair, module)?)?;
    module.add_function(wrap_pyfunction!(sign_proof_receipt, module)?)?;
    module.add_function(wrap_pyfunction!(verify_proof_receipt, module)?)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
