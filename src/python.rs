//! Typed, non-blocking Python boundary.

use arrow::ffi_stream::ArrowArrayStreamReader;
use arrow::pyarrow::PyArrowType;
use arrow::record_batch::RecordBatchReader;
use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};
use serde::Serialize;
use serde_json::Value;

use crate::{
    CompiledContract, ContractAst, DiffOptions, DistinctMode, ErrorCode, ExecutionOptions,
    LeakageOptions, ProofFrameError, ResourceLimits, detect_leakage_with_options,
    diff_readers_with_options, execute_reader, fingerprint_reader, profile_reader_with_distinct,
    scan_pii_reader,
};

create_exception!(proofframe, ProofFrameException, PyValueError);
create_exception!(proofframe, ContractError, ProofFrameException);
create_exception!(proofframe, SchemaError, ProofFrameException);
create_exception!(proofframe, ResourceLimitError, ProofFrameException);
create_exception!(proofframe, CorruptDataError, ProofFrameException);
create_exception!(proofframe, ReceiptError, ProofFrameException);

const DEFAULT_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_TEMP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DEFAULT_OUTPUT_RECORDS: u64 = 100_000;
const DEFAULT_SAMPLES: usize = 100;

#[pyfunction]
#[pyo3(signature = (source, distinct="exact"))]
fn profile_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    distinct: &str,
) -> PyResult<Py<PyAny>> {
    let distinct = DistinctMode::from_name(distinct).map_err(|error| map_error(py, error))?;
    let result = py.detach(move || profile_reader_with_distinct(source.0, distinct));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
}

#[pyfunction]
fn fingerprint_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
) -> PyResult<String> {
    py.detach(move || fingerprint_reader(source.0))
        .map_err(|error| map_error(py, error))
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
fn check_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    row_count_hint: Option<u64>,
    max_memory_bytes: u64,
    max_temp_bytes: u64,
    max_output_records: u64,
    max_samples: usize,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || {
        let schema = source.0.schema();
        let ast = ContractAst::from_json(&contract_json)?;
        let plan = CompiledContract::compile(&ast, schema.as_ref())?;
        let options = ExecutionOptions {
            row_count_hint,
            resources: limits(
                max_memory_bytes,
                max_temp_bytes,
                max_output_records,
                max_samples,
            ),
            cancellation: crate::CancellationToken::new(),
        };
        execute_reader(source.0, &plan, &options)
    });
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
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
    max_samples=DEFAULT_SAMPLES
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
) -> PyResult<Py<PyAny>> {
    let options = DiffOptions {
        resources: limits(
            max_memory_bytes,
            max_temp_bytes,
            max_output_records,
            max_samples,
        ),
        max_samples,
        output: None,
        cancellation: crate::CancellationToken::new(),
    };
    let result = py.detach(move || diff_readers_with_options(before.0, after.0, &keys, &options));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|report| serialize_to_python(py, &report))
}

#[pyfunction]
#[pyo3(signature = (source, max_findings=100))]
fn scan_pii_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    max_findings: usize,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || scan_pii_reader(source.0, max_findings));
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
fn verify_proof_receipt(py: Python<'_>, receipt_json: String) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || crate::receipt::verify_json(&receipt_json));
    result
        .map_err(|error| map_error(py, error))
        .and_then(|verification| serialize_to_python(py, &verification))
}

fn limits(memory: u64, temp: u64, output: u64, samples: usize) -> ResourceLimits {
    ResourceLimits {
        max_memory_bytes: memory,
        max_temp_bytes: temp,
        max_output_records: output,
        max_samples: samples,
    }
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
        | ErrorCode::ContractTypeMismatch => ContractError::new_err(message),
        ErrorCode::SchemaMismatch | ErrorCode::MissingColumn | ErrorCode::UnsupportedType => {
            SchemaError::new_err(message)
        }
        ErrorCode::ResourceLimit => ResourceLimitError::new_err(message),
        ErrorCode::CorruptPartition | ErrorCode::Io | ErrorCode::Arrow => {
            CorruptDataError::new_err(message)
        }
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
    module.add_function(wrap_pyfunction!(profile_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(fingerprint_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(check_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(validate_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(validate_fast_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(diff_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(scan_pii_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(detect_leakage_arrow, module)?)?;
    module.add_function(wrap_pyfunction!(generate_signing_keypair, module)?)?;
    module.add_function(wrap_pyfunction!(sign_proof_receipt, module)?)?;
    module.add_function(wrap_pyfunction!(verify_proof_receipt, module)?)?;
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
        "CorruptDataError",
        module.py().get_type::<CorruptDataError>(),
    )?;
    module.add("ReceiptError", module.py().get_type::<ReceiptError>())?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
