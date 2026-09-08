mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn run(schema: Arc<Schema>, batch: RecordBatch, source: &str) -> (bool, Vec<String>) {
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    (
        report.valid,
        report
            .findings
            .iter()
            .map(|f| format!("{}@{:?}", f.rule, f.row))
            .collect(),
    )
}

fn text(values: Vec<&str>) -> (Arc<Schema>, RecordBatch) {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Utf8, true)]));
    let column: ArrayRef = Arc::new(StringArray::from(values));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    (schema, batch)
}

#[test]
fn a_value_outside_the_declared_length_is_reported() {
    let (schema, batch) = text(vec!["short", "just right", "far too long for this"]);
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
        "columns":{"v":{"min_length":6,"max_length":12}}}"#;
    let (valid, rules) = run(schema, batch, source);
    assert!(!valid);
    assert_eq!(rules, vec!["min_length@Some(0)", "max_length@Some(2)"]);
}

/// Counting bytes would make an eight-character rule mean different things in
/// different alphabets.
#[test]
fn length_counts_characters_rather_than_bytes() {
    let (schema, batch) = text(vec!["ÇÖĞÜŞİ"]);
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
        "columns":{"v":{"min_length":6,"max_length":6}}}"#;
    let (valid, _) = run(schema, batch, source);
    assert!(valid, "six characters, twelve bytes");
}

#[test]
fn a_length_is_the_only_rule_a_column_needs_to_be_checked() {
    let (schema, batch) = text(vec!["abcdefghij"]);
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
        "columns":{"v":{"max_length":3}}}"#;
    let (valid, rules) = run(schema, batch, source);
    assert!(!valid, "a column with only a length must still be scanned");
    assert_eq!(rules, vec!["max_length@Some(0)"]);
}

#[test]
fn a_minimum_longer_than_the_maximum_is_refused() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Utf8, true)]));
    let source = r#"{"version":"proofframe.contract.v2","status":"active",
        "columns":{"v":{"min_length":10,"max_length":2}}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let error = CompiledContract::compile_document(&document, schema.as_ref()).unwrap_err();
    assert!(
        error.to_string().contains("exceeds `max_length`"),
        "{error}"
    );
}

/// Nobody writing a contract knows that 2024-01-01 is day 19723.
#[test]
fn a_date_bound_can_be_written_as_a_date() {
    let schema = Arc::new(Schema::new(vec![Field::new("d", DataType::Date32, true)]));
    // 2023-06-01, 2024-01-01, 2024-06-01
    let column: ArrayRef = Arc::new(Date32Array::from(vec![19_509, 19_723, 19_875]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
        "columns":{"d":{"type":"date32","min":"2024-01-01"}}}"#;
    let (valid, rules) = run(schema, batch, source);
    assert!(!valid);
    assert_eq!(
        rules,
        vec!["min@Some(0)"],
        "only the 2023 row is before the bound"
    );
}

#[test]
fn a_day_count_still_parses_so_older_contracts_keep_working() {
    let schema = Arc::new(Schema::new(vec![Field::new("d", DataType::Date32, true)]));
    let column: ArrayRef = Arc::new(Date32Array::from(vec![19_509, 19_723]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
        "columns":{"d":{"type":"date32","min":19723}}}"#;
    let (valid, rules) = run(schema, batch, source);
    assert!(!valid);
    assert_eq!(rules, vec!["min@Some(0)"]);
}

#[test]
fn an_ambiguous_date_spelling_is_refused_rather_than_guessed() {
    let schema = Arc::new(Schema::new(vec![Field::new("d", DataType::Date32, true)]));
    for spelling in ["2024-1-1", "2024-01", "01/01/2024"] {
        let source = format!(
            r#"{{"version":"proofframe.contract.v2","status":"active",
                "columns":{{"d":{{"type":"date32","min":"{spelling}"}}}}}}"#
        );
        let document = ContractDocument::from_json(&source).unwrap();
        assert!(
            CompiledContract::compile_document(&document, schema.as_ref()).is_err(),
            "`{spelling}` must not be guessed at"
        );
    }
}
