//! Browser surface for the ProofFrame engine.
//!
//! Every function here compiles the caller's contract against the schema the CSV
//! actually produced and runs the same native execution path the Python and Rust
//! APIs use. Nothing is reimplemented in JavaScript, and no data leaves the tab.

use std::io::Cursor;
use std::sync::Arc;

use arrow::csv::ReaderBuilder;
use arrow::csv::reader::Format;
use arrow::datatypes::{DataType, Field, Schema};
use proofframe::evidence::{contract_source_digest, evidence_for_check};
use proofframe::{
    CancellationToken, CompiledContract, ContractDocument, ExecutionOptions, ReferenceBindings,
    ResourceLimits, SuggestOptions, execute_reader_with_fingerprint_and_references, review_html,
    suggest_reader_with_options,
};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

/// Rows inspected when inferring the Arrow schema of an uploaded file.
const SCHEMA_INFERENCE_ROWS: usize = 1000;

/// Route Rust panics to the host console so a browser failure is readable.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Version of the engine this module was compiled from.
#[wasm_bindgen]
pub fn engine_version() -> String {
    proofframe_version().to_string()
}

const fn proofframe_version() -> &'static str {
    // The wasm crate is versioned in lockstep with the engine it links.
    env!("CARGO_PKG_VERSION")
}

fn csv_format(delimiter: u8, has_header: bool) -> Format {
    Format::default()
        .with_header(has_header)
        .with_delimiter(delimiter)
}

/// Read a column that held no values at all as text.
///
/// Arrow infers `Null` for a column that was empty in every sampled row, and the
/// engine refuses that type for canonical fingerprinting. An empty column in an
/// uploaded file is a text column nobody filled in, so it is read as one.
fn as_text_where_empty(schema: Schema) -> Schema {
    let fields = schema
        .fields()
        .iter()
        .map(|field| match field.data_type() {
            DataType::Null => Arc::new(
                Field::new(field.name(), DataType::Utf8, true)
                    .with_metadata(field.metadata().clone()),
            ),
            _ => Arc::clone(field),
        })
        .collect::<Vec<_>>();
    Schema::new_with_metadata(fields, schema.metadata().clone())
}

/// Infer the Arrow schema of `csv` and build a reader over the same bytes.
fn read_csv(
    csv: &[u8],
    delimiter: u8,
    has_header: bool,
) -> Result<(Arc<Schema>, arrow::csv::Reader<Cursor<&[u8]>>), String> {
    let format = csv_format(delimiter, has_header);
    let (schema, _) = format
        .infer_schema(Cursor::new(csv), Some(SCHEMA_INFERENCE_ROWS))
        .map_err(|error| format!("could not read the CSV: {error}"))?;
    let schema = Arc::new(as_text_where_empty(schema));
    let reader = ReaderBuilder::new(Arc::clone(&schema))
        .with_format(csv_format(delimiter, has_header))
        .build(Cursor::new(csv))
        .map_err(|error| format!("could not read the CSV: {error}"))?;
    Ok((schema, reader))
}

fn schema_columns(schema: &Schema) -> Vec<Value> {
    schema
        .fields()
        .iter()
        .map(|field| json!({ "name": field.name(), "type": field.data_type().to_string() }))
        .collect()
}

/// Rules whose exact-set machinery spills to a temporary directory.
///
/// The browser has no filesystem, and the spill path panics inside `std` rather
/// than returning an error, so these are refused before the engine is entered.
/// Everything else runs the same native code the CLI runs.
fn unsupported_rule(contract: &Value) -> Option<&'static str> {
    let columns = contract.get("columns").and_then(Value::as_object);
    if let Some(columns) = columns
        && columns
            .values()
            .any(|rule| rule.get("unique").and_then(Value::as_bool) == Some(true))
    {
        return Some("unique");
    }
    let dataset = contract.get("dataset_rules")?;
    let occupied = |key: &str| {
        dataset.get(key).is_some_and(|value| match value {
            Value::Array(items) => !items.is_empty(),
            Value::Object(entries) => !entries.is_empty(),
            _ => false,
        })
    };
    [
        "composite_unique",
        "distinct_count",
        "distinct_ratio",
        "references",
    ]
    .into_iter()
    .find(|key| occupied(key))
}

fn reject_unsupported(contract_json: &str) -> Result<(), JsError> {
    let contract: Value = serde_json::from_str(contract_json)
        .map_err(|error| JsError::new(&format!("contract is not valid JSON: {error}")))?;
    match unsupported_rule(&contract) {
        None => Ok(()),
        Some(rule) => Err(JsError::new(&format!(
            "`{rule}` needs the spilling exact-set path, which requires a filesystem. Run it with the CLI or the Python package; every other rule runs here."
        ))),
    }
}

/// Suggest a review-required V2 contract for an uploaded CSV.
///
/// The result is a starting point a human is expected to edit, which is why the
/// engine marks it as suggested rather than authoritative.
#[wasm_bindgen]
pub fn suggest_contract(csv: &[u8], delimiter: u8, has_header: bool) -> Result<String, JsError> {
    let (schema, reader) =
        read_csv(csv, delimiter, has_header).map_err(|error| JsError::new(&error))?;
    let options = SuggestOptions {
        infer_categories: true,
        infer_required: true,
        ..SuggestOptions::default()
    };
    let contract = suggest_reader_with_options(reader, options, None)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let payload = json!({
        "engine": { "name": "proofframe", "version": proofframe_version() },
        "schema": schema_columns(&schema),
        "contract": contract,
    });
    serde_json::to_string(&payload).map_err(|error| JsError::new(&error.to_string()))
}

/// Validate an uploaded CSV against a contract and return the report as JSON.
#[wasm_bindgen]
pub fn check_csv(
    csv: &[u8],
    contract_json: &str,
    delimiter: u8,
    has_header: bool,
    max_samples: usize,
) -> Result<String, JsError> {
    reject_unsupported(contract_json)?;
    let (schema, reader) =
        read_csv(csv, delimiter, has_header).map_err(|error| JsError::new(&error))?;
    let document = ContractDocument::from_json(contract_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let plan = CompiledContract::compile_document(&document, schema.as_ref())
        .map_err(|error| JsError::new(&error.to_string()))?;
    let options = ExecutionOptions {
        row_count_hint: None,
        resources: ResourceLimits {
            max_samples,
            ..ResourceLimits::default()
        },
        cancellation: CancellationToken::new(),
        threads: None,
    };
    let source_digest =
        contract_source_digest(contract_json).map_err(|error| JsError::new(&error.to_string()))?;
    let (report, fingerprint) = execute_reader_with_fingerprint_and_references(
        reader,
        &plan,
        &options,
        ReferenceBindings::new(),
    )
    .map_err(|error| JsError::new(&error.to_string()))?;

    // Same shape the Python bindings publish, so the review renderer and any
    // verifier read one evidence document rather than a browser-shaped variant.
    let mut report_value =
        serde_json::to_value(&report).map_err(|error| JsError::new(&error.to_string()))?;
    report_value
        .as_object_mut()
        .ok_or_else(|| JsError::new("Validation report is not a JSON object"))?
        .insert(
            "contract_source_digest".to_owned(),
            Value::String(source_digest.clone()),
        );
    let evidence = evidence_for_check(&report, &report_value, &fingerprint, source_digest)
        .map_err(|error| JsError::new(&error.to_string()))?;

    let payload = json!({
        "engine": { "name": "proofframe", "version": proofframe_version() },
        "schema": schema_columns(&schema),
        "fingerprint": fingerprint.to_string(),
        "report": report_value,
        "evidence": evidence,
    });
    serde_json::to_string(&payload).map_err(|error| JsError::new(&error.to_string()))
}

/// Render the offline HTML review for a result this module produced.
///
/// The engine renders it, so the page a visitor downloads is the page
/// `proofframe review` writes. No HTML is assembled in JavaScript.
#[wasm_bindgen]
pub fn review_report(
    result_json: &str,
    contract_json: &str,
    label: &str,
) -> Result<String, JsError> {
    let result: Value =
        serde_json::from_str(result_json).map_err(|error| JsError::new(&error.to_string()))?;
    let contract: Value =
        serde_json::from_str(contract_json).map_err(|error| JsError::new(&error.to_string()))?;
    let names = result["schema"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|field| field["name"].as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    review_html(
        &result["report"],
        &result["evidence"],
        &contract,
        label,
        &names,
    )
    .map_err(|error| JsError::new(&error.to_string()))
}
