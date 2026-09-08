//! Browser surface for the ProofFrame engine.
//!
//! Every function here compiles the caller's contract against the schema the CSV
//! actually produced and runs the same native execution path the Python and Rust
//! APIs use. Nothing is reimplemented in JavaScript, and no data leaves the tab.

use std::io::Cursor;
use std::sync::Arc;

use arrow::csv::ReaderBuilder;
use arrow::util::display::array_value_to_string;
use arrow::csv::reader::Format;
use arrow::datatypes::{DataType, Field, Schema};
use proofframe::evidence::{contract_source_digest, evidence_for_check};
use proofframe::{
    CancellationToken, CompiledContract, ContractDocument, ExecutionOptions, ReferenceBindings,
    ResourceLimits, SpillPolicy, SuggestOptions, execute_reader_with_fingerprint_and_references,
    review_html, review_markdown,
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

/// Rules that still need a filesystem after the in-memory exact path.
///
/// Uniqueness and the dataset-level exact rules now run in memory and fail closed
/// on the budget, so they are allowed. Reference rules are different: they resolve
/// against a second dataset the browser never bound, and a foreign key that is
/// never evaluated reports as one that held.
fn unsupported_rule(contract: &Value) -> Option<&'static str> {
    let dataset = contract.get("dataset_rules")?;
    dataset
        .get("references")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .map(|_| "references")
}

/// The rules in `contract_json` that this build cannot run, before it is run.
///
/// Answering ahead of the scan is the difference between a page that tells you what
/// it can do and one that lets you press the button and then explains itself.
#[wasm_bindgen]
pub fn unsupported_rules(contract_json: &str) -> String {
    let Ok(contract) = serde_json::from_str::<Value>(contract_json) else {
        return "[]".to_owned();
    };
    match unsupported_rule(&contract) {
        None => "[]".to_owned(),
        Some(rule) => json!([rule]).to_string(),
    }
}

/// The contract with the rules this build cannot run removed.
///
/// Removing them is what makes the rest runnable, and it is also why such a run may
/// never be called valid: the plan that executed is not the plan the author wrote.
fn without_unsupported(contract_json: &str) -> Result<String, JsError> {
    let mut contract: Value = serde_json::from_str(contract_json)
        .map_err(|error| JsError::new(&format!("contract is not valid JSON: {error}")))?;
    if unsupported_rule(&contract).is_none() {
        return Ok(contract_json.to_owned());
    }
    if let Some(rules) = contract
        .get_mut("dataset_rules")
        .and_then(Value::as_object_mut)
    {
        rules.remove("references");
    }
    serde_json::to_string(&contract).map_err(|error| JsError::new(&error.to_string()))
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
        infer_uniqueness: true,
        infer_categories: true,
        infer_required: true,
        spill: SpillPolicy::Never,
        ..SuggestOptions::default()
    };
    let contract = suggest_reader_with_options(reader, options, None)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let text = serde_json::to_string_pretty(&contract)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let payload = json!({
        "engine": { "name": "proofframe", "version": proofframe_version() },
        "schema": schema_columns(&schema),
        "contract": text,
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
    let skipped = unsupported_rule(
        &serde_json::from_str(contract_json)
            .map_err(|error| JsError::new(&format!("contract is not valid JSON: {error}")))?,
    );
    let runnable = without_unsupported(contract_json)?;
    let contract_json = runnable.as_str();
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
        // There is no filesystem here. Uniqueness runs in memory and fails closed
        // when the budget cannot hold the next value.
        spill: SpillPolicy::Never,
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

    // A run that skipped a rule is not a run of the contract that was written, so it
    // never reports as valid and never carries evidence. Evidence is what a verifier
    // trusts; issuing it for a partial scan would be the one lie this page exists to
    // avoid.
    let payload = match skipped {
        None => json!({
            "engine": { "name": "proofframe", "version": proofframe_version() },
            "schema": schema_columns(&schema),
            "fingerprint": fingerprint.to_string(),
            "report": report_value,
            "evidence": evidence,
        }),
        Some(rule) => json!({
            "engine": { "name": "proofframe", "version": proofframe_version() },
            "schema": schema_columns(&schema),
            "fingerprint": fingerprint.to_string(),
            "report": report_value,
            "status": "incomplete",
            "skipped_rules": [rule],
        }),
    };
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

/// Set a contract's status without letting the document through a JSON round-trip.
///
/// `JSON.parse` in a browser rounds every integer past 2^53, so a bound of
/// `9223372036854775807` came back as `9223372036854776000` and the engine refused
/// it — correctly, and for a value the author never wrote. Rewriting the status
/// here keeps every literal exactly as the author typed it.
#[wasm_bindgen]
pub fn set_contract_status(contract_json: &str, status: &str) -> Result<String, JsError> {
    let mut contract: Value =
        serde_json::from_str(contract_json).map_err(|error| JsError::new(&error.to_string()))?;
    contract
        .as_object_mut()
        .ok_or_else(|| JsError::new("contract is not a JSON object"))?
        .insert("status".to_owned(), Value::String(status.to_owned()));
    serde_json::to_string_pretty(&contract).map_err(|error| JsError::new(&error.to_string()))
}

/// Read a window of rows back as text, for showing a finding in context.
///
/// The rows come from the same Arrow reader the check used, so the preview shows
/// what the engine saw rather than a second, disagreeing parse of the same file.
#[wasm_bindgen]
pub fn preview_rows(
    csv: &[u8],
    delimiter: u8,
    has_header: bool,
    start: usize,
    count: usize,
) -> Result<String, JsError> {
    let (schema, reader) = read_csv(csv, delimiter, has_header).map_err(|error| JsError::new(&error))?;
    let mut rows = Vec::new();
    let mut offset = 0usize;
    for batch in reader {
        let batch = batch.map_err(|error| JsError::new(&error.to_string()))?;
        let height = batch.num_rows();
        if offset + height > start && rows.len() < count {
            let first = start.saturating_sub(offset);
            for row in first..height {
                if rows.len() >= count {
                    break;
                }
                let cells = (0..batch.num_columns())
                    .map(|column| {
                        let array = batch.column(column);
                        if array.is_null(row) {
                            Ok(Value::Null)
                        } else {
                            array_value_to_string(array.as_ref(), row)
                                .map(Value::String)
                                .map_err(|error| JsError::new(&error.to_string()))
                        }
                    })
                    .collect::<Result<Vec<_>, JsError>>()?;
                rows.push(json!({ "row": offset + row, "cells": cells }));
            }
        }
        offset += height;
        if rows.len() >= count {
            break;
        }
    }
    serde_json::to_string(&json!({
        "schema": schema_columns(&schema),
        "rows": rows,
    }))
    .map_err(|error| JsError::new(&error.to_string()))
}

/// Render the Markdown CI summary for a result this module produced.
#[wasm_bindgen]
pub fn review_summary(result_json: &str, label: &str) -> Result<String, JsError> {
    let result: Value =
        serde_json::from_str(result_json).map_err(|error| JsError::new(&error.to_string()))?;
    let names = result["schema"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|field| field["name"].as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    review_markdown(&result["report"], &result["evidence"], label, &names)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// The whole review bundle, as `{ filename: contents }`.
///
/// Every file is produced here rather than assembled in the page, so the report and
/// the evidence a visitor downloads are the engine's own bytes and not a JavaScript
/// re-serialization of them.
#[wasm_bindgen]
pub fn bundle_files(
    result_json: &str,
    contract_json: &str,
    label: &str,
) -> Result<String, JsError> {
    let result: Value =
        serde_json::from_str(result_json).map_err(|error| JsError::new(&error.to_string()))?;
    let pretty = |value: &Value| {
        serde_json::to_string_pretty(value).map_err(|error| JsError::new(&error.to_string()))
    };
    Ok(json!({
        "index.html": review_report(result_json, contract_json, label)?,
        "summary.md": review_summary(result_json, label)?,
        "report.json": pretty(&result["report"])?,
        "evidence.json": pretty(&result["evidence"])?,
        "contract.json": contract_json,
    })
    .to_string())
}
