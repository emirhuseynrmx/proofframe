//! Typed, non-blocking Python boundary.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Instant;

use arrow::ffi_stream::ArrowArrayStreamReader;
use arrow::pyarrow::PyArrowType;
use arrow::record_batch::RecordBatchReader;
use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};
use serde::Serialize;
use serde_json::Value;

use crate::evidence::{
    DatasetEvidence, EngineEvidence, EvidenceSchema, EvidenceV2, ExecutionEvidence, ResultEvidence,
    contract_source_digest, validation_findings_digest, validation_metrics_digest,
    validation_report_digest, validation_result_digest,
};
use crate::{
    CompiledContract, ContractDocument, DiffOptions, DiffOutput, DistinctMode, ErrorCode,
    ExecutionOptions, FingerprintOptions, FingerprintVersion, LeakageOptions, PartitionReader,
    PiiFingerprintOptions, ProofFrameError, ResourceLimits, SpillPolicy, check_partition_readers,
    check_partition_readers_with_evidence, detect_leakage_with_options, diff_readers_with_options,
    execute_reader, execute_reader_with_fingerprint, fingerprint_reader_with_options,
    profile_reader_with_resources_and_hint, scan_pii_reader_with_options,
};

create_exception!(proofframe, ProofFrameException, PyValueError);
create_exception!(proofframe, ContractError, ProofFrameException);
create_exception!(proofframe, SchemaError, ProofFrameException);
create_exception!(proofframe, ResourceLimitError, ProofFrameException);
create_exception!(proofframe, ProofFrameCorruptDataError, ProofFrameException);
create_exception!(proofframe, ProofFrameIoError, ProofFrameException);
create_exception!(proofframe, ProofFrameArrowError, ProofFrameException);
create_exception!(proofframe, ReceiptError, ProofFrameException);

const DEFAULT_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_TEMP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DEFAULT_OUTPUT_RECORDS: u64 = 100_000;
const DEFAULT_SAMPLES: usize = 100;

#[pyfunction]
#[pyo3(signature = (
    source,
    distinct="none",
    row_count_hint=None,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES
))]
fn profile_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    distinct: &str,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
) -> PyResult<Py<PyAny>> {
    let distinct = DistinctMode::from_name(distinct).map_err(|error| map_error(py, error))?;
    let resources = limits(
        max_memory_bytes,
        max_temp_bytes,
        DEFAULT_OUTPUT_RECORDS,
        DEFAULT_SAMPLES,
    );
    let result = py.detach(move || {
        profile_reader_with_resources_and_hint(source.0, distinct, resources, row_count_hint)
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
}

#[pyfunction]
#[pyo3(signature = (source, version="v1"))]
fn fingerprint_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    version: &str,
) -> PyResult<String> {
    let version = parse_fingerprint_version(py, version)?;
    py.detach(move || {
        fingerprint_reader_with_options(source.0, &FingerprintOptions::new(version))
            .map(|fingerprint| fingerprint.to_tagged_string())
    })
    .map_err(|error| map_error(py, error))
}

fn parse_fingerprint_version(py: Python<'_>, version: &str) -> PyResult<FingerprintVersion> {
    Ok(match version {
        "v1" | "pf-fp-v1" => FingerprintVersion::V1,
        "v2" | "pf-fp-v2" => FingerprintVersion::V2,
        other => {
            return Err(map_error(
                py,
                ProofFrameError::InvalidContract(format!(
                    "Unsupported fingerprint version `{other}`; expected v1 or v2"
                )),
            ));
        }
    })
}

#[pyfunction]
#[pyo3(signature = (source, version="v1"))]
fn benchmark_fingerprint_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    version: &str,
) -> PyResult<Py<PyAny>> {
    let version = parse_fingerprint_version(py, version)?;
    let result = py.detach(move || {
        let started = Instant::now();
        let fingerprint =
            fingerprint_reader_with_options(source.0, &FingerprintOptions::new(version))?;
        let native_elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        Ok(serde_json::json!({
            "native_elapsed_ns": native_elapsed_ns,
            "fingerprint": fingerprint.to_tagged_string(),
        }))
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|value| value_to_python(py, value))
}

#[pyfunction]
#[pyo3(signature = (
    source,
    contract_json,
    row_count_hint=None,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES,
    threads=None
))]
#[allow(clippy::too_many_arguments)]
fn check_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
    threads: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let schema = source.0.schema();
        let contract_source_digest = contract_source_digest(&contract_json)?;
        let document = ContractDocument::from_json(&contract_json)?;
        let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
        let options = ExecutionOptions {
            row_count_hint,
            resources: limits(
                max_memory_bytes,
                max_temp_bytes,
                max_output_records,
                max_samples,
            ),
            cancellation: crate::CancellationToken::new(),
            threads: worker_count(threads)?,
        };
        execute_reader(source.0, &plan, &options).map(|report| (report, contract_source_digest))
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|(report, source_digest)| {
            serialize_check_report_to_python(py, &report, source_digest)
        })
}

/// Internal release-gate hook: compile outside the timer, then measure exactly
/// the same native execution path used by `check_arrow`.
#[pyfunction]
#[pyo3(signature = (
    source,
    contract_json,
    row_count_hint=None,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES
))]
fn benchmark_check_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let schema = source.0.schema();
        let document = ContractDocument::from_json(&contract_json)?;
        let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
        let options = ExecutionOptions {
            row_count_hint,
            resources: limits(
                max_memory_bytes,
                max_temp_bytes,
                DEFAULT_OUTPUT_RECORDS,
                DEFAULT_SAMPLES,
            ),
            cancellation: crate::CancellationToken::new(),
            threads: None,
        };
        let started = Instant::now();
        let report = execute_reader(source.0, &plan, &options)?;
        let native_elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        Ok(serde_json::json!({
            "native_elapsed_ns": native_elapsed_ns,
            "report": report,
        }))
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|value| value_to_python(py, value))
}

#[pyfunction]
#[pyo3(signature = (
    sources,
    contract_json,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES,
    threads=None
))]
#[allow(clippy::too_many_arguments)]
fn check_partitions_arrow(
    py: Python<'_>,
    sources: Vec<PyArrowType<ArrowArrayStreamReader>>,
    contract_json: String,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
    threads: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let readers = sources
            .into_iter()
            .map(|source| Box::new(source.0) as PartitionReader)
            .collect::<Vec<_>>();
        let schema = readers
            .first()
            .ok_or_else(|| {
                ProofFrameError::InvalidContract("At least one partition is required".into())
            })?
            .schema();
        let source_digest = contract_source_digest(&contract_json)?;
        let document = ContractDocument::from_json(&contract_json)?;
        let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
        let options = ExecutionOptions {
            row_count_hint: None,
            resources: limits(
                max_memory_bytes,
                max_temp_bytes,
                max_output_records,
                max_samples,
            ),
            cancellation: crate::CancellationToken::new(),
            threads: worker_count(threads)?,
        };
        check_partition_readers(readers, &plan, &options).map(|report| (report, source_digest))
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|(report, source_digest)| {
            serialize_check_report_to_python(py, &report, source_digest)
        })
}

#[pyfunction]
#[pyo3(signature = (
    sources,
    contract_json,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES,
    threads=None
))]
#[allow(clippy::too_many_arguments)]
fn check_partitions_with_evidence_arrow(
    py: Python<'_>,
    sources: Vec<PyArrowType<ArrowArrayStreamReader>>,
    contract_json: String,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
    threads: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let readers = sources
            .into_iter()
            .map(|source| Box::new(source.0) as PartitionReader)
            .collect::<Vec<_>>();
        let schema = readers
            .first()
            .ok_or_else(|| {
                ProofFrameError::InvalidContract("At least one partition is required".into())
            })?
            .schema();
        let source_digest = contract_source_digest(&contract_json)?;
        let document = ContractDocument::from_json(&contract_json)?;
        let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
        let options = ExecutionOptions {
            row_count_hint: None,
            resources: limits(
                max_memory_bytes,
                max_temp_bytes,
                max_output_records,
                max_samples,
            ),
            cancellation: crate::CancellationToken::new(),
            threads: worker_count(threads)?,
        };
        let (report, manifest) =
            check_partition_readers_with_evidence(readers, &plan, &source_digest, &options)?;
        let mut report = serde_json::to_value(report)?;
        report
            .as_object_mut()
            .ok_or_else(|| {
                ProofFrameError::CorruptData("Partition report is not a JSON object".into())
            })?
            .insert(
                "contract_source_digest".to_string(),
                Value::String(source_digest),
            );
        Ok(serde_json::json!({"report": report, "manifest": manifest}))
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|value| value_to_python(py, value))
}

#[pyfunction]
fn assemble_evidence_unchecked_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    report_json: String,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let schema = source.0.schema();
        let document = ContractDocument::from_json(&contract_json)?;
        let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
        let source_digest = contract_source_digest(&contract_json)?;
        let compiled_plan_digest = plan.compiled_plan_digest()?;
        let schema_digest = plan.schema_digest()?;
        let fingerprint = fingerprint_reader_with_options(
            source.0,
            &FingerprintOptions::new(FingerprintVersion::V2),
        )?;
        let report: Value = serde_json::from_str(&report_json)?;
        let report_rows = report_u64(&report, "rows")?;
        if report_rows != fingerprint.rows() {
            return Err(ProofFrameError::InvalidReceipt(format!(
                "Evidence data has {} rows but the validation report claims {report_rows}",
                fingerprint.rows()
            )));
        }
        let resources: ResourceLimits =
            serde_json::from_value(report.get("resources").cloned().ok_or_else(|| {
                ProofFrameError::InvalidReceipt(
                    "Validation report is missing `resources`".to_string(),
                )
            })?)?;
        let output_records = report
            .get("findings")
            .and_then(Value::as_array)
            .map_or(0, |findings| findings.len() as u64);
        require_report_digest(&report, "contract_source_digest", &source_digest)?;
        require_report_digest(&report, "compiled_plan_digest", &compiled_plan_digest)?;
        require_report_digest(&report, "schema_digest", &schema_digest)?;
        let evidence = EvidenceV2 {
            schema: EvidenceSchema::V2,
            dataset: DatasetEvidence {
                fingerprint_version: FingerprintVersion::V2,
                fingerprint_digest: *fingerprint.digest(),
                rows: fingerprint.rows(),
            },
            contract_source_digest: source_digest,
            compiled_plan_digest,
            schema_digest,
            engine: EngineEvidence {
                name: "proofframe".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            execution: ExecutionEvidence {
                operation: "check".to_string(),
                resources,
            },
            result: ResultEvidence {
                valid: report_bool(&report, "valid")?,
                violation_count: report_u64(&report, "violation_count")?,
                output_records,
                truncated: report_bool(&report, "truncated")?,
                result_digest: validation_result_digest(&report)?,
                report_digest: validation_report_digest(&report)?,
                findings_digest: validation_findings_digest(&report)?,
                metrics_digest: validation_metrics_digest(&report)?,
            },
        };
        evidence.validate()?;
        Ok(evidence)
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|evidence| serialize_to_python(py, &evidence))
}

#[pyfunction]
#[pyo3(signature = (
    source,
    contract_json,
    row_count_hint=None,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES,
    threads=None
))]
#[allow(clippy::too_many_arguments)]
fn check_with_evidence_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
    threads: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let schema = source.0.schema();
        let source_digest = contract_source_digest(&contract_json)?;
        let document = ContractDocument::from_json(&contract_json)?;
        let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
        let options = ExecutionOptions {
            row_count_hint,
            resources: limits(
                max_memory_bytes,
                max_temp_bytes,
                max_output_records,
                max_samples,
            ),
            cancellation: crate::CancellationToken::new(),
            threads: worker_count(threads)?,
        };
        let (report, fingerprint) = execute_reader_with_fingerprint(source.0, &plan, &options)?;
        let mut report_value = serde_json::to_value(&report)?;
        let report_object = report_value.as_object_mut().ok_or_else(|| {
            ProofFrameError::InvalidReceipt("Validation report is not a JSON object".into())
        })?;
        report_object.insert(
            "contract_source_digest".to_string(),
            Value::String(source_digest.clone()),
        );
        let output_records = report.findings.len() as u64;
        let evidence = EvidenceV2 {
            schema: EvidenceSchema::V2,
            dataset: DatasetEvidence {
                fingerprint_version: FingerprintVersion::V2,
                fingerprint_digest: *fingerprint.digest(),
                rows: fingerprint.rows(),
            },
            contract_source_digest: source_digest,
            compiled_plan_digest: report.compiled_plan_digest.clone(),
            schema_digest: report.schema_digest.clone(),
            engine: EngineEvidence {
                name: "proofframe".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            execution: ExecutionEvidence {
                operation: "check".to_string(),
                resources: report.resources,
            },
            result: ResultEvidence {
                valid: report.valid,
                violation_count: report.violation_count,
                output_records,
                truncated: report.truncated,
                result_digest: validation_result_digest(&report_value)?,
                report_digest: validation_report_digest(&report_value)?,
                findings_digest: validation_findings_digest(&report_value)?,
                metrics_digest: validation_metrics_digest(&report_value)?,
            },
        };
        evidence.validate()?;
        Ok(serde_json::json!({"report": report_value, "evidence": evidence}))
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|value| value_to_python(py, value))
}

#[pyfunction]
#[pyo3(signature = (
    source,
    contract_json,
    row_count_hint=None,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES
))]
#[allow(clippy::too_many_arguments)]
fn validate_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
) -> PyResult<Py<PyAny>> {
    check_arrow(
        py,
        source,
        contract_json,
        row_count_hint,
        max_memory_bytes,
        max_temp_bytes,
        max_output_records,
        max_samples,
        None,
    )
}

#[pyfunction]
#[pyo3(signature = (
    source,
    contract_json,
    row_count_hint=None,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES
))]
#[allow(clippy::too_many_arguments)]
fn validate_fast_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
) -> PyResult<Py<PyAny>> {
    check_arrow(
        py,
        source,
        contract_json,
        row_count_hint,
        max_memory_bytes,
        max_temp_bytes,
        max_output_records,
        max_samples,
        None,
    )
}

#[pyfunction]
#[pyo3(signature = (
    before,
    after,
    keys,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES,
    max_output_records=DEFAULT_OUTPUT_RECORDS,
    max_samples=DEFAULT_SAMPLES,
    output_path=None,
    output_format="jsonl",
    before_row_count_hint=None,
    after_row_count_hint=None,
    input_bytes_hint=None,
    spill="auto"
))]
#[allow(clippy::too_many_arguments)]
fn diff_arrow(
    py: Python<'_>,
    before: PyArrowType<ArrowArrayStreamReader>,
    after: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
    output_path: Option<String>,
    output_format: &str,
    before_row_count_hint: Option<u64>,
    after_row_count_hint: Option<u64>,
    input_bytes_hint: Option<u64>,
    spill: &str,
) -> PyResult<Py<PyAny>> {
    let spill = SpillPolicy::from_name(spill).map_err(|error| map_error(py, error))?;
    let output = match (output_path, output_format) {
        (None, _) => None,
        (Some(path), "jsonl") => Some(DiffOutput::JsonLines(PathBuf::from(path))),
        (Some(path), "arrow") | (Some(path), "ipc") => {
            Some(DiffOutput::ArrowIpc(PathBuf::from(path)))
        }
        (Some(_), other) => {
            return Err(map_error(
                py,
                ProofFrameError::InvalidContract(format!(
                    "Unsupported diff output format `{other}`; expected jsonl or arrow"
                )),
            ));
        }
    };
    let options = DiffOptions {
        resources: limits(
            max_memory_bytes,
            max_temp_bytes,
            max_output_records,
            max_samples,
        ),
        max_samples,
        output,
        cancellation: crate::CancellationToken::new(),
        row_count_hints: before_row_count_hint.zip(after_row_count_hint),
        input_bytes_hint,
        spill,
    };
    let result = py.detach(move || diff_readers_with_options(before.0, after.0, &keys, &options));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
}

#[pyfunction]
#[pyo3(signature = (
    source,
    max_findings=100,
    fingerprint_mode="unlinkable",
    fingerprint_key=None,
    key_id=None
))]
fn scan_pii_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    max_findings: usize,
    fingerprint_mode: &str,
    fingerprint_key: Option<String>,
    key_id: Option<String>,
) -> PyResult<Py<PyAny>> {
    let fingerprint = match fingerprint_mode {
        "unlinkable" => {
            if fingerprint_key.is_some() || key_id.is_some() {
                return Err(map_error(
                    py,
                    ProofFrameError::InvalidContract(
                        "unlinkable PII mode does not accept fingerprint_key or key_id".to_string(),
                    ),
                ));
            }
            PiiFingerprintOptions::unlinkable().map_err(|error| map_error(py, error))?
        }
        "stable" => {
            let key = fingerprint_key.ok_or_else(|| {
                map_error(
                    py,
                    ProofFrameError::InvalidContract(
                        "stable PII mode requires a 32-byte fingerprint_key".to_string(),
                    ),
                )
            })?;
            let key_id = key_id.ok_or_else(|| {
                map_error(
                    py,
                    ProofFrameError::InvalidContract("stable PII mode requires key_id".to_string()),
                )
            })?;
            PiiFingerprintOptions::stable_base64(&key, key_id)
                .map_err(|error| map_error(py, error))?
        }
        other => {
            return Err(map_error(
                py,
                ProofFrameError::InvalidContract(format!(
                    "Unsupported PII fingerprint_mode `{other}`; expected stable or unlinkable"
                )),
            ));
        }
    };
    let result =
        py.detach(move || scan_pii_reader_with_options(source.0, max_findings, &fingerprint));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
}

#[pyfunction]
#[pyo3(signature = (
    train,
    test,
    keys,
    max_samples=20,
    max_memory_bytes=DEFAULT_MEMORY_BYTES,
    max_temp_bytes=DEFAULT_TEMP_BYTES
))]
#[allow(clippy::too_many_arguments)]
fn detect_leakage_arrow(
    py: Python<'_>,
    train: PyArrowType<ArrowArrayStreamReader>,
    test: PyArrowType<ArrowArrayStreamReader>,
    keys: Vec<String>,
    max_samples: usize,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
) -> PyResult<Py<PyAny>> {
    let options = LeakageOptions {
        resources: limits(
            max_memory_bytes,
            max_temp_bytes,
            DEFAULT_OUTPUT_RECORDS,
            max_samples,
        ),
        max_samples,
        cancellation: crate::CancellationToken::new(),
    };
    let result = py.detach(move || detect_leakage_with_options(train.0, test.0, &keys, &options));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
}

#[pyfunction]
fn generate_signing_keypair(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let json = crate::receipt::generate_keypair_json().map_err(|error| map_error(py, error))?;
    json_text_to_python(py, &json)
}

#[pyfunction]
fn sign_proof_receipt(
    py: Python<'_>,
    report_json: String,
    private_key: String,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || crate::receipt::sign_json(&report_json, &private_key));
    let json = result.map_err(|error| map_error(py, error))?;
    json_text_to_python(py, &json)
}

#[pyfunction]
fn sign_evidence_receipt(
    py: Python<'_>,
    evidence_json: String,
    private_key: String,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || crate::receipt::sign_v2_json(&evidence_json, &private_key));
    let json = result.map_err(|error| map_error(py, error))?;
    json_text_to_python(py, &json)
}

#[pyfunction]
fn verify_proof_receipt(py: Python<'_>, receipt_json: String) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || crate::receipt::verify_json(&receipt_json));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|verification| serialize_to_python(py, &verification))
}

#[pyfunction]
#[pyo3(signature = (receipt_json, expected_public_key=None))]
fn verify_proof_receipt_any(
    py: Python<'_>,
    receipt_json: String,
    expected_public_key: Option<String>,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        crate::receipt::verify_json_with_expected_key(&receipt_json, expected_public_key.as_deref())
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|verification| serialize_to_python(py, &verification))
}

fn report_u64(report: &Value, field: &str) -> Result<u64, ProofFrameError> {
    report.get(field).and_then(Value::as_u64).ok_or_else(|| {
        ProofFrameError::InvalidReceipt(format!(
            "Validation report field `{field}` must be an unsigned integer"
        ))
    })
}

fn report_bool(report: &Value, field: &str) -> Result<bool, ProofFrameError> {
    report.get(field).and_then(Value::as_bool).ok_or_else(|| {
        ProofFrameError::InvalidReceipt(format!(
            "Validation report field `{field}` must be a boolean"
        ))
    })
}

fn require_report_digest(
    report: &Value,
    field: &str,
    expected: &str,
) -> Result<(), ProofFrameError> {
    let observed = report.get(field).and_then(Value::as_str).ok_or_else(|| {
        ProofFrameError::InvalidReceipt(format!(
            "Validation report field `{field}` must be a digest string"
        ))
    })?;
    if observed != expected {
        return Err(ProofFrameError::InvalidReceipt(format!(
            "Validation report `{field}` digest does not match the supplied data contract"
        )));
    }
    Ok(())
}

fn limits(memory: u64, temp: u64, output: u64, samples: usize) -> ResourceLimits {
    ResourceLimits {
        max_memory_bytes: memory,
        max_temp_bytes: temp,
        max_output_records: output,
        max_samples: samples,
    }
}

fn worker_count(threads: Option<usize>) -> Result<Option<NonZeroUsize>, ProofFrameError> {
    threads
        .map(|threads| {
            NonZeroUsize::new(threads).ok_or_else(|| {
                ProofFrameError::InvalidContract("threads must be at least 1".into())
            })
        })
        .transpose()
}

fn json_text_to_python(py: Python<'_>, json: &str) -> PyResult<Py<PyAny>> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| map_error(py, ProofFrameError::Json(error)))?;
    value_to_python(py, value)
}

fn serialize_to_python<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    let value =
        serde_json::to_value(value).map_err(|error| map_error(py, ProofFrameError::Json(error)))?;
    value_to_python(py, value)
}

fn serialize_check_report_to_python<T: Serialize>(
    py: Python<'_>,
    report: &T,
    contract_source_digest: String,
) -> PyResult<Py<PyAny>> {
    let mut value = serde_json::to_value(report)
        .map_err(|error| map_error(py, ProofFrameError::Json(error)))?;
    value
        .as_object_mut()
        .expect("serialized validation reports are JSON objects")
        .insert(
            "contract_source_digest".to_string(),
            Value::String(contract_source_digest),
        );
    value_to_python(py, value)
}

fn value_to_python(py: Python<'_>, value: Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(value) => Ok(value.into_pyobject(py)?.to_owned().into_any().unbind()),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(value.into_pyobject(py)?.into_any().unbind())
            } else if let Some(value) = value.as_u64() {
                Ok(value.into_pyobject(py)?.into_any().unbind())
            } else if let Some(value) = value.as_f64() {
                Ok(value.into_pyobject(py)?.into_any().unbind())
            } else {
                Err(map_error(
                    py,
                    ProofFrameError::InvalidContract(
                        "serialized result contains an unsupported JSON number".into(),
                    ),
                ))
            }
        }
        Value::String(value) => Ok(value.into_pyobject(py)?.into_any().unbind()),
        Value::Array(values) => {
            let values = values
                .into_iter()
                .map(|value| value_to_python(py, value))
                .collect::<PyResult<Vec<_>>>()?;
            Ok(PyList::new(py, values)?.into_any().unbind())
        }
        Value::Object(values) => {
            let output = PyDict::new(py);
            for (key, value) in values {
                output.set_item(key, value_to_python(py, value)?)?;
            }
            Ok(output.into_any().unbind())
        }
    }
}

fn map_error(py: Python<'_>, error: ProofFrameError) -> PyErr {
    let code = error.code();
    let path = error.path().map(str::to_owned);
    let resource = match &error {
        ProofFrameError::ResourceLimit {
            resource,
            requested,
            used,
            limit,
        } => Some((*resource, *requested, *used, *limit)),
        _ => None,
    };
    let message = error.to_string();
    let py_error = match code {
        ErrorCode::ContractInvalidJson
        | ErrorCode::ContractUnknownField
        | ErrorCode::ContractInvalidBound
        | ErrorCode::ContractDraft
        | ErrorCode::ContractTypeMismatch => ContractError::new_err(message),
        ErrorCode::SchemaMismatch | ErrorCode::MissingColumn | ErrorCode::UnsupportedType => {
            SchemaError::new_err(message)
        }
        ErrorCode::ResourceLimit => ResourceLimitError::new_err(message),
        ErrorCode::CorruptPartition => ProofFrameCorruptDataError::new_err(message),
        ErrorCode::Io => ProofFrameIoError::new_err(message),
        ErrorCode::Arrow => ProofFrameArrowError::new_err(message),
        ErrorCode::ReceiptInvalid
        | ErrorCode::ReceiptInvalidSignature
        | ErrorCode::ReceiptUntrustedSigner => ReceiptError::new_err(message),
        _ => ProofFrameException::new_err(message),
    };
    let value = py_error.value(py);
    value
        .setattr("code", code.as_str())
        .expect("ProofFrame exceptions accept structured attributes");
    if let Some(path) = path {
        value
            .setattr("path", path)
            .expect("ProofFrame exceptions accept a contract path");
    }
    if let Some((name, requested, used, limit)) = resource {
        value
            .setattr("resource", name)
            .expect("ProofFrame exceptions accept a resource name");
        value
            .setattr("requested_bytes", requested)
            .expect("ProofFrame exceptions accept requested bytes");
        value
            .setattr("used_bytes", used)
            .expect("ProofFrame exceptions accept used bytes");
        value
            .setattr("limit_bytes", limit)
            .expect("ProofFrame exceptions accept limit bytes");
        value
            .setattr("remaining_bytes", limit.saturating_sub(used))
            .expect("ProofFrame exceptions accept remaining bytes");
    }
    py_error
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    register_data_functions(module)?;
    register_receipt_functions(module)?;
    register_exceptions(module)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

fn register_data_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    register_profile_functions(module)?;
    register_contract_functions(module)?;
    register_analysis_functions(module)?;
    Ok(())
}

fn register_profile_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(profile_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(fingerprint_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(benchmark_fingerprint_arrow, module)?)?;
    Ok(())
}

fn register_contract_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(check_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(benchmark_check_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(check_partitions_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(
        check_partitions_with_evidence_arrow,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(check_with_evidence_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(assemble_evidence_unchecked_arrow, module)?)?;
    Ok(())
}

fn register_analysis_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(validate_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(validate_fast_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(diff_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(scan_pii_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(detect_leakage_arrow, module)?)?;
    Ok(())
}

fn register_receipt_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(generate_signing_keypair, module)?)?;
    module.add_function(wrap_pyfunction!(sign_proof_receipt, module)?)?;
    module.add_function(wrap_pyfunction!(sign_evidence_receipt, module)?)?;
    module.add_function(wrap_pyfunction!(verify_proof_receipt, module)?)?;
    module.add_function(wrap_pyfunction!(verify_proof_receipt_any, module)?)?;
    Ok(())
}

fn register_exceptions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add(
        "ProofFrameError",
        module.py().get_type::<ProofFrameException>(),
    )?;
    module.add("ContractError", module.py().get_type::<ContractError>())?;
    module.add("SchemaError", module.py().get_type::<SchemaError>())?;
    module.add(
        "ResourceLimitError",
        module.py().get_type::<ResourceLimitError>(),
    )?;
    module.add(
        "ProofFrameCorruptDataError",
        module.py().get_type::<ProofFrameCorruptDataError>(),
    )?;
    module.add(
        "CorruptDataError",
        module.py().get_type::<ProofFrameCorruptDataError>(),
    )?;
    module.add(
        "ProofFrameIoError",
        module.py().get_type::<ProofFrameIoError>(),
    )?;
    module.add(
        "ProofFrameArrowError",
        module.py().get_type::<ProofFrameArrowError>(),
    )?;
    module.add("ReceiptError", module.py().get_type::<ReceiptError>())?;
    Ok(())
}
