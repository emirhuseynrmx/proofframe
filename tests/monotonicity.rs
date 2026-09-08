mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn batches(values: Vec<Option<i64>>) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "clock",
        DataType::Int64,
        true,
    )]));
    let column: ArrayRef = Arc::new(Int64Array::from(values));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    (schema, vec![batch])
}

fn run(values: Vec<Option<i64>>, contract: &str) -> (bool, u64, Vec<String>) {
    let (schema, data) = batches(values);
    let document = ContractDocument::from_json(contract).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(data),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    let rules = report
        .findings
        .iter()
        .map(|finding| format!("{}@{}", finding.rule, finding.row.expect("row")))
        .collect();
    (report.valid, report.violation_count, rules)
}

fn contract(direction: &str, nulls: &str) -> String {
    format!(
        r#"{{"version":"proofframe.contract.v2","status":"active","max_findings":10,
            "dataset_rules":{{"monotonicity":[
              {{"name":"clock","column":"clock","direction":"{direction}","nulls":"{nulls}"}}]}}}}"#
    )
}

#[test]
fn a_sequence_that_goes_backwards_is_reported_at_the_row_that_broke_it() {
    let values = vec![Some(10), Some(20), Some(15), Some(30)];
    let (valid, count, rules) = run(values, &contract("increasing", "skip"));
    assert!(!valid);
    assert_eq!(count, 1);
    assert_eq!(rules, vec!["monotonicity@2"]);
}

#[test]
fn equal_values_pass_increasing_and_fail_strictly_increasing() {
    let values = vec![Some(1), Some(1), Some(2)];
    let (valid, _, _) = run(values.clone(), &contract("increasing", "skip"));
    assert!(valid, "a repeated value does not decrease");

    let (valid, count, rules) = run(values, &contract("strictly_increasing", "skip"));
    assert!(!valid);
    assert_eq!(count, 1);
    assert_eq!(rules, vec!["monotonicity@1"]);
}

#[test]
fn decreasing_reads_the_other_direction() {
    let (valid, _, _) = run(
        vec![Some(9), Some(5), Some(5)],
        &contract("decreasing", "skip"),
    );
    assert!(valid);
    let (valid, _, _) = run(
        vec![Some(9), Some(5), Some(6)],
        &contract("decreasing", "skip"),
    );
    assert!(!valid);
}

/// A null is not a position in the order, so the values around it are compared to
/// each other rather than through it.
#[test]
fn a_skipped_null_compares_the_values_on_either_side_of_it() {
    let (valid, _, _) = run(
        vec![Some(1), None, Some(2)],
        &contract("strictly_increasing", "skip"),
    );
    assert!(valid);

    let (valid, count, rules) = run(
        vec![Some(5), None, Some(4)],
        &contract("strictly_increasing", "skip"),
    );
    assert!(!valid, "the null must not hide the step backwards");
    assert_eq!(count, 1);
    assert_eq!(rules, vec!["monotonicity@2"]);
}

#[test]
fn a_rejected_null_is_reported_where_it_appears() {
    let (valid, count, rules) = run(
        vec![Some(1), None, Some(2)],
        &contract("increasing", "reject"),
    );
    assert!(!valid);
    assert_eq!(count, 1);
    assert_eq!(rules, vec!["monotonicity@1"]);
}

#[test]
fn text_columns_order_lexicographically() {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, true)]));
    let column: ArrayRef = Arc::new(StringArray::from(vec![Some("a"), Some("b"), Some("aa")]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
        "dataset_rules":{"monotonicity":[
          {"name":"code","column":"code","direction":"increasing"}]}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    assert!(!report.valid, "`aa` sorts before `b`");
    assert_eq!(report.violation_count, 1);
}

/// A contract that carries no ordering rule must keep the plan identity its
/// receipts already record.
///
/// The digest cannot be compared against a build that no longer exists, so it is
/// pinned the way the rule itself is written: an empty list and an absent list
/// execute identically, so they must hash identically. Anything else would mean the
/// language grew and every old receipt stopped matching.
#[test]
fn an_absent_ordering_rule_hashes_the_same_as_an_empty_one() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "clock",
        DataType::Int64,
        true,
    )]));
    let digest = |source: &str| {
        let document = ContractDocument::from_json(source).unwrap();
        CompiledContract::compile_document(&document, schema.as_ref())
            .unwrap()
            .compiled_plan_digest()
            .unwrap()
    };

    let absent = digest(
        r#"{"version":"proofframe.contract.v2","status":"active",
            "columns":{"clock":{"type":"int64"}}}"#,
    );
    let empty = digest(
        r#"{"version":"proofframe.contract.v2","status":"active",
            "columns":{"clock":{"type":"int64"}},
            "dataset_rules":{"monotonicity":[]}}"#,
    );
    assert_eq!(absent, empty);

    let present = digest(
        r#"{"version":"proofframe.contract.v2","status":"active",
            "columns":{"clock":{"type":"int64"}},
            "dataset_rules":{"monotonicity":[
              {"name":"c","column":"clock","direction":"increasing"}]}}"#,
    );
    assert_ne!(absent, present, "a real rule must change the plan identity");
}
