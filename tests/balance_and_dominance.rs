mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn ledger(debit: Vec<f64>, credit: Vec<f64>, chunk: usize) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("debit", DataType::Float64, true),
        Field::new("credit", DataType::Float64, true),
    ]));
    let rows: Vec<(f64, f64)> = debit.into_iter().zip(credit).collect();
    let batches = rows
        .chunks(chunk.max(1))
        .map(|slice| {
            let d: ArrayRef = Arc::new(Float64Array::from(
                slice.iter().map(|(d, _)| *d).collect::<Vec<_>>(),
            ));
            let c: ArrayRef = Arc::new(Float64Array::from(
                slice.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
            ));
            RecordBatch::try_new(Arc::clone(&schema), vec![d, c]).unwrap()
        })
        .collect();
    (schema, batches)
}

fn check(schema: Arc<Schema>, batches: Vec<RecordBatch>, source: &str) -> (bool, Vec<String>) {
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(batches),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    (
        report.valid,
        report.findings.iter().map(|f| f.message.clone()).collect(),
    )
}

const BALANCE: &str = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
    "dataset_rules":{"balance_equal":[
      {"name":"books","left_column":"debit","right_column":"credit","tolerance":0.0001}]}}"#;

#[test]
fn a_ledger_that_balances_passes() {
    let (schema, batches) = ledger(vec![10.0, 20.5], vec![5.5, 25.0], 1);
    let (valid, _) = check(schema, batches, BALANCE);
    assert!(valid);
}

#[test]
fn a_ledger_that_does_not_balance_names_both_totals() {
    let (schema, batches) = ledger(vec![10.0, 20.0], vec![5.0, 20.0], 2);
    let (valid, messages) = check(schema, batches, BALANCE);
    assert!(!valid);
    assert!(messages[0].contains("`debit` totalling 30"), "{messages:?}");
    assert!(
        messages[0].contains("`credit` totalling 25"),
        "{messages:?}"
    );
}

/// Money stored as a float does not add up exactly, which is what the tolerance is
/// for; the totals themselves must still not move with the batch size.
#[test]
fn a_balance_verdict_does_not_change_with_the_batch_size() {
    let debit: Vec<f64> = (0..5_000).map(|i| 0.01 + f64::from(i) * 1e-6).collect();
    let credit = debit.clone();
    for chunk in [5_000, 331, 64, 7] {
        let (schema, batches) = ledger(debit.clone(), credit.clone(), chunk);
        let (valid, _) = check(schema, batches, BALANCE);
        assert!(valid, "chunk {chunk}");
    }
}

fn categories(values: Vec<&str>) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, true)]));
    let column: ArrayRef = Arc::new(StringArray::from(values));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    (schema, vec![batch])
}

const DOMINANT: &str = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
    "dataset_rules":{"max_dominant_value_ratio":[{"name":"skew","column":"code","max":0.7}]}}"#;

#[test]
fn one_value_holding_most_of_a_column_is_reported() {
    let (schema, batches) = categories(vec![
        "TR", "TR", "TR", "TR", "TR", "TR", "TR", "TR", "DE", "FR",
    ]);
    let (valid, messages) = check(schema, batches, DOMINANT);
    assert!(!valid);
    assert!(messages[0].contains("in 8 row(s)"), "{messages:?}");
    assert!(
        messages[0].contains("above the maximum 0.7"),
        "{messages:?}"
    );
}

#[test]
fn a_spread_column_passes() {
    let (schema, batches) = categories(vec!["TR", "DE", "FR", "IT"]);
    let (valid, _) = check(schema, batches, DOMINANT);
    assert!(valid);
}

/// A column that is mostly empty is exactly the case this rule is for, so nulls
/// count as the value the column held.
#[test]
fn nulls_count_towards_the_dominant_share() {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, true)]));
    let column: ArrayRef = Arc::new(StringArray::from(vec![None, None, None, Some("TR")]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    let (valid, messages) = check(schema, vec![batch], DOMINANT);
    assert!(!valid, "three of four rows are empty");
    assert!(messages[0].contains("in 3 row(s)"), "{messages:?}");
}

#[test]
fn a_ratio_outside_zero_to_one_is_refused() {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, true)]));
    for max in ["-0.1", "1.5"] {
        let source = format!(
            r#"{{"version":"proofframe.contract.v2","status":"active",
                "dataset_rules":{{"max_dominant_value_ratio":[
                  {{"name":"s","column":"code","max":{max}}}]}}}}"#
        );
        let document = ContractDocument::from_json(&source).unwrap();
        let error = CompiledContract::compile_document(&document, schema.as_ref()).unwrap_err();
        assert!(error.to_string().contains("between 0 and 1"), "{error}");
    }
}

#[test]
fn absent_rules_hash_the_same_as_empty_ones() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("debit", DataType::Float64, true),
        Field::new("credit", DataType::Float64, true),
    ]));
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
            "dataset_rules":{"balance_equal":[],"max_dominant_value_ratio":[]}}"#,
    );
    assert_eq!(absent, empty);
    assert_ne!(absent, digest(BALANCE));
}
