mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, TimestampSecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

/// Split the values across batches so a rule cannot depend on where a batch ends.
fn run(values: Vec<Option<i64>>, chunk: usize, contract: &str) -> (bool, u64, Vec<String>) {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "clock",
        DataType::Timestamp(TimeUnit::Second, None),
        true,
    )]));
    let batches = values
        .chunks(chunk)
        .map(|slice| {
            let column: ArrayRef = Arc::new(TimestampSecondArray::from(slice.to_vec()));
            RecordBatch::try_new(Arc::clone(&schema), vec![column]).unwrap()
        })
        .collect::<Vec<_>>();
    let document = ContractDocument::from_json(contract).unwrap();
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
    (report.valid, report.violation_count, messages)
}

fn contract(step: &str, extra: &str) -> String {
    format!(
        r#"{{"version":"proofframe.contract.v2","status":"active","max_findings":10,
            "dataset_rules":{{"gap_detection":[
              {{"name":"one_min_bars","column":"clock","expected_step":{step}{extra}}}]}}}}"#
    )
}

#[test]
fn a_missing_minute_is_reported_once_with_its_unit_named() {
    let values = vec![Some(0), Some(60), Some(300), Some(360)];
    let (valid, count, messages) = run(values, 4, &contract("60", ""));
    assert!(!valid);
    assert_eq!(count, 1);
    assert!(
        messages[0].contains("1 step(s) longer than 60 seconds"),
        "{messages:?}"
    );
}

#[test]
fn an_evenly_spaced_series_passes() {
    let values = vec![Some(0), Some(60), Some(120), Some(180)];
    let (valid, count, _) = run(values, 2, &contract("60", ""));
    assert!(valid);
    assert_eq!(count, 0);
}

/// The verdict must describe the data, not the reader's batch size.
#[test]
fn the_result_does_not_depend_on_where_the_batches_end() {
    let values = vec![Some(0), Some(60), Some(300), Some(360), Some(900)];
    let whole = run(values.clone(), 5, &contract("60", ""));
    for chunk in [1, 2, 3, 4] {
        assert_eq!(
            run(values.clone(), chunk, &contract("60", "")),
            whole,
            "chunk {chunk}"
        );
    }
}

#[test]
fn tolerance_and_an_allowance_move_the_line_without_hiding_the_count() {
    let values = vec![Some(0), Some(61), Some(122)];
    let (valid, _, _) = run(values.clone(), 3, &contract("60", ""));
    assert!(!valid, "a one-second overshoot is still a gap");

    let (valid, _, _) = run(values.clone(), 3, &contract("60", r#","tolerance":1"#));
    assert!(valid, "tolerance absorbs it");

    let (valid, _, _) = run(values, 3, &contract("60", r#","max_gaps":2"#));
    assert!(valid, "two gaps are allowed");
}

/// A null has no position, so the step is measured across it rather than through it.
#[test]
fn a_skipped_null_does_not_invent_a_gap() {
    let values = vec![Some(0), None, Some(60)];
    let (valid, _, _) = run(values, 3, &contract("60", ""));
    assert!(valid);
}

#[test]
fn a_step_that_is_not_a_positive_number_is_refused_at_compile_time() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "clock",
        DataType::Timestamp(TimeUnit::Second, None),
        true,
    )]));
    for step in ["0", "-60"] {
        let source = contract(step, "");
        let document = ContractDocument::from_json(&source).unwrap();
        let error = CompiledContract::compile_document(&document, schema.as_ref())
            .expect_err("a step of {step} must be refused");
        assert!(error.to_string().contains("expected_step"), "{error}");
    }
}

#[test]
fn a_column_with_no_distance_is_refused_rather_than_silently_skipped() {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, true)]));
    let source = r#"{"version":"proofframe.contract.v2","status":"active",
        "dataset_rules":{"gap_detection":[
          {"name":"g","column":"code","expected_step":1}]}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let error = CompiledContract::compile_document(&document, schema.as_ref()).unwrap_err();
    assert!(
        error.to_string().contains("ordered numeric or temporal"),
        "{error}"
    );
}
