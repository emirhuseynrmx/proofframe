mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn check(schema: Arc<Schema>, batches: Vec<RecordBatch>, source: &str) -> (bool, Vec<String>) {
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(batches),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    let messages = report
        .findings
        .iter()
        .map(|finding| finding.message.clone())
        .collect();
    (report.valid, messages)
}

fn integers(values: Vec<i64>, chunk: usize) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "amount",
        DataType::Int64,
        true,
    )]));
    let batches = values
        .chunks(chunk)
        .map(|slice| {
            let column: ArrayRef = Arc::new(Int64Array::from(slice.to_vec()));
            RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap()
        })
        .collect();
    (schema, batches)
}

fn floats(values: Vec<f64>, chunk: usize) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "amount",
        DataType::Float64,
        true,
    )]));
    let batches = values
        .chunks(chunk)
        .map(|slice| {
            let column: ArrayRef = Arc::new(Float64Array::from(slice.to_vec()));
            RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap()
        })
        .collect();
    (schema, batches)
}

fn contract(bounds: &str) -> String {
    format!(
        r#"{{"version":"proofframe.contract.v2","status":"active","max_findings":10,
            "dataset_rules":{{"sum":[
              {{"name":"turnover","column":"amount"{bounds}}}]}}}}"#
    )
}

#[test]
fn a_total_outside_its_bounds_is_reported_with_the_total_named() {
    let (schema, batches) = integers(vec![10, 20, 30], 2);
    let (valid, messages) = check(schema, batches, &contract(r#","max":50"#));
    assert!(!valid);
    assert!(messages[0].contains("totals 60"), "{messages:?}");
    assert!(messages[0].contains("above the maximum 50"), "{messages:?}");
}

#[test]
fn a_total_inside_its_bounds_passes() {
    let (schema, batches) = integers(vec![10, 20, 30], 1);
    let (valid, _) = check(schema, batches, &contract(r#","min":0,"max":60"#));
    assert!(valid);
}

/// `arrow::compute::sum` answers `i64::MAX + 1` with a negative number. A rule that
/// exists to catch a total growing too large must not be built on that.
#[test]
fn a_total_past_i64_is_counted_rather_than_wrapped() {
    let (schema, batches) = integers(vec![i64::MAX, i64::MAX], 1);
    let (valid, messages) = check(schema, batches, &contract(r#","max":100"#));
    assert!(!valid, "two maxima are not a small negative number");
    assert!(
        messages[0].contains("18446744073709551614"),
        "the exact total must survive: {messages:?}"
    );
}

/// A bound larger than 2^53 is exactly what a finance contract states, so it must
/// not be rounded on its way in.
#[test]
fn an_integer_bound_past_two_to_the_fifty_third_is_kept_exactly() {
    let (schema, batches) = integers(vec![9_007_199_254_740_993], 1);
    let (valid, _) = check(schema, batches, &contract(r#","max":9007199254740993"#));
    assert!(valid, "the total equals the bound exactly");

    let (schema, batches) = integers(vec![9_007_199_254_740_993], 1);
    let (valid, _) = check(schema, batches, &contract(r#","max":9007199254740992"#));
    assert!(!valid, "one less must fail");
}

/// Batch size is a reader setting. The verdict must describe the data instead.
#[test]
fn a_float_total_does_not_change_with_the_batch_size() {
    let values: Vec<f64> = (0..100_000).map(|i| 0.1 + f64::from(i) * 1e-9).collect();
    let mut totals = Vec::new();
    for chunk in [100_000, 7_919, 1_024, 97] {
        let (schema, batches) = floats(values.clone(), chunk);
        let document = ContractDocument::from_json(&contract("")).unwrap();
        let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
        let report = execute_reader(
            reader_from_batches(batches),
            &plan,
            &ExecutionOptions::default(),
        )
        .unwrap();
        assert!(report.valid);
        // Re-run with a bound just under the true total to read the total back out.
        let (schema, batches) = floats(values.clone(), chunk);
        let (_, messages) = check(schema, batches, &contract(r#","max":0"#));
        totals.push(messages[0].clone());
    }
    assert!(
        totals.windows(2).all(|pair| pair[0] == pair[1]),
        "the same values gave different totals: {totals:?}"
    );
}

#[test]
fn nulls_are_not_counted_as_zero_or_anything_else() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "amount",
        DataType::Int64,
        true,
    )]));
    let column: ArrayRef = Arc::new(Int64Array::from(vec![Some(10), None, Some(20)]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap();
    let (valid, messages) = check(schema, vec![batch], &contract(r#","max":29"#));
    assert!(!valid);
    assert!(messages[0].contains("totals 30"), "{messages:?}");
}

#[test]
fn a_non_numeric_column_is_refused_at_compile_time() {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, true)]));
    let source = r#"{"version":"proofframe.contract.v2","status":"active",
        "dataset_rules":{"sum":[{"name":"s","column":"code","max":1}]}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let error = CompiledContract::compile_document(&document, schema.as_ref()).unwrap_err();
    assert!(
        error.to_string().contains("needs a numeric column"),
        "{error}"
    );
}

#[test]
fn an_absent_total_rule_hashes_the_same_as_an_empty_one() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "amount",
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
    let absent = digest(r#"{"version":"proofframe.contract.v2","status":"active"}"#);
    let empty = digest(
        r#"{"version":"proofframe.contract.v2","status":"active","dataset_rules":{"sum":[]}}"#,
    );
    assert_eq!(absent, empty);
    assert_ne!(absent, digest(&contract(r#","max":1"#)));
}
