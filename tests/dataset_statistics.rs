mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn floats(values: Vec<Option<f64>>, chunk: usize) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Float64, true)]));
    let batches = values
        .chunks(chunk.max(1))
        .map(|slice| {
            let column: ArrayRef = Arc::new(Float64Array::from(slice.to_vec()));
            RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap()
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

fn contract(kind: &str, bounds: &str) -> String {
    format!(
        r#"{{"version":"proofframe.contract.v2","status":"active","max_findings":10,
            "dataset_rules":{{"{kind}":[{{"name":"r","column":"v"{bounds}}}]}}}}"#
    )
}

#[test]
fn a_mean_outside_its_bounds_is_reported_with_the_value_and_the_count() {
    let (schema, batches) = floats(vec![Some(1.0), Some(2.0), Some(3.0)], 2);
    let (valid, messages) = check(schema, batches, &contract("mean", r#","max":1.5"#));
    assert!(!valid);
    assert!(messages[0].contains("mean 2"), "{messages:?}");
    assert!(messages[0].contains("over 3 value(s)"), "{messages:?}");
}

#[test]
fn a_mean_inside_its_bounds_passes() {
    let (schema, batches) = floats(vec![Some(1.0), Some(2.0), Some(3.0)], 1);
    let (valid, _) = check(
        schema,
        batches,
        &contract("mean", r#","min":1.9,"max":2.1"#),
    );
    assert!(valid);
}

#[test]
fn the_standard_deviation_is_the_population_one_over_the_values_seen() {
    // 2, 4, 4, 4, 5, 5, 7, 9 has a population standard deviation of exactly 2.
    let values = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]
        .into_iter()
        .map(Some)
        .collect();
    let (schema, batches) = floats(values, 3);
    let (valid, _) = check(
        schema,
        batches,
        &contract("std_dev", r#","min":1.999,"max":2.001"#),
    );
    assert!(valid);
}

/// Welford is a sequential recurrence, so the batching cannot move the answer.
#[test]
fn neither_statistic_changes_with_the_batch_size() {
    let values: Vec<Option<f64>> = (0..10_000)
        .map(|i| Some(1e9 + f64::from(i) * 0.25))
        .collect();
    for kind in ["mean", "std_dev"] {
        let mut seen = Vec::new();
        for chunk in [10_000, 997, 64, 7] {
            let (schema, batches) = floats(values.clone(), chunk);
            // Bounds that cannot hold, so the message carries the computed value.
            let (_, messages) = check(schema, batches, &contract(kind, r#","max":-1"#));
            seen.push(messages[0].clone());
        }
        assert!(
            seen.windows(2).all(|pair| pair[0] == pair[1]),
            "{kind} moved with the batch size: {seen:?}"
        );
    }
}

/// A mean of nothing is not zero, and zero would pass bounds it never earned.
#[test]
fn a_column_with_no_values_reports_that_rather_than_zero() {
    let (schema, batches) = floats(vec![None, None], 2);
    let (valid, messages) = check(schema, batches, &contract("mean", r#","min":0,"max":0"#));
    assert!(!valid, "an empty column must not satisfy a bound");
    assert!(messages[0].contains("had no value"), "{messages:?}");
}

#[test]
fn integers_are_summarised_too() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, true)]));
    let column: ArrayRef = Arc::new(Int64Array::from(vec![10i64, 20, 30]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    let (valid, messages) = check(schema, vec![batch], &contract("mean", r#","max":19"#));
    assert!(!valid);
    assert!(messages[0].contains("mean 20"), "{messages:?}");
}

#[test]
fn a_non_numeric_column_is_refused_at_compile_time() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Utf8, true)]));
    let document = ContractDocument::from_json(&contract("mean", r#","max":1"#)).unwrap();
    let error = CompiledContract::compile_document(&document, schema.as_ref()).unwrap_err();
    assert!(
        error.to_string().contains("needs a numeric column"),
        "{error}"
    );
}

#[test]
fn absent_statistic_rules_hash_the_same_as_empty_ones() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Float64, true)]));
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
            "dataset_rules":{"mean":[],"std_dev":[]}}"#,
    );
    assert_eq!(absent, empty);
    assert_ne!(absent, digest(&contract("mean", r#","max":1"#)));
    assert_ne!(
        digest(&contract("mean", r#","max":1"#)),
        digest(&contract("std_dev", r#","max":1"#)),
        "the two statistics must not share a plan identity"
    );
}
