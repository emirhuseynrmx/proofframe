mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    CompiledContract, ContractDocument, ExecutionOptions, ReferenceBindings,
    execute_reader_with_references,
};

use support::reader_from_batches;

fn rows(count: i64) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    let column: ArrayRef = Arc::new(Int64Array::from((0..count).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    (schema, vec![batch])
}

fn contract(bounds: &str) -> String {
    format!(
        r#"{{"version":"proofframe.contract.v2","status":"active","max_findings":10,
            "dataset_rules":{{"row_count_delta":[
              {{"name":"volume","against_reference":"yesterday"{bounds}}}]}}}}"#
    )
}

fn run(today: i64, yesterday: Option<i64>, bounds: &str) -> Result<(bool, Vec<String>), String> {
    let (schema, batches) = rows(today);
    let mut bindings = ReferenceBindings::new();
    if let Some(count) = yesterday {
        let (_, reference) = rows(count);
        bindings.insert("yesterday", Box::new(reader_from_batches(reference)));
    }
    let document = ContractDocument::from_json(&contract(bounds)).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader_with_references(
        reader_from_batches(batches),
        &plan,
        &ExecutionOptions::default(),
        bindings,
    )
    .map_err(|error| error.to_string())?;
    Ok((
        report.valid,
        report.findings.iter().map(|f| f.message.clone()).collect(),
    ))
}

#[test]
fn a_volume_collapse_is_reported() {
    let (valid, messages) = run(50, Some(100), r#","min_ratio":-0.05"#).unwrap();
    assert!(!valid);
    assert!(messages[0].contains("below the minimum"), "{messages:?}");
    assert!(messages[0].contains("50 rows against 100"), "{messages:?}");
}

#[test]
fn an_uncontrolled_jump_is_reported() {
    let (valid, messages) = run(200, Some(100), r#","max_ratio":0.25"#).unwrap();
    assert!(!valid);
    assert!(messages[0].contains("above the maximum"), "{messages:?}");
}

#[test]
fn a_move_inside_the_band_passes() {
    let (valid, _) = run(110, Some(100), r#","min_ratio":-0.05,"max_ratio":0.25"#).unwrap();
    assert!(valid);
}

/// A comparison that never happened must not report as one that held.
#[test]
fn an_unbound_dataset_fails_before_the_subject_is_scanned() {
    let error = run(100, None, r#","max_ratio":0.25"#).unwrap_err();
    assert!(
        error.contains("needs a dataset bound to `yesterday`"),
        "{error}"
    );
}

/// Dividing by an empty yesterday would turn it into an infinite change or a pass.
#[test]
fn an_empty_reference_is_reported_rather_than_divided_by() {
    let (valid, messages) = run(10, Some(0), r#","max_ratio":0.25"#).unwrap();
    assert!(!valid);
    assert!(messages[0].contains("has no rows"), "{messages:?}");
}

#[test]
fn an_absent_rule_hashes_the_same_as_an_empty_one() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    let digest = |source: &str| {
        let document = ContractDocument::from_json(source).unwrap();
        CompiledContract::compile_document(&document, schema.as_ref())
            .unwrap()
            .compiled_plan_digest()
            .unwrap()
    };
    let absent = digest(r#"{"version":"proofframe.contract.v2","status":"active"}"#);
    let empty = digest(
        r#"{"version":"proofframe.contract.v2","status":"active",
            "dataset_rules":{"row_count_delta":[]}}"#,
    );
    assert_eq!(absent, empty);
    assert_ne!(absent, digest(&contract(r#","max_ratio":1"#)));
}
