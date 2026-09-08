mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("tckn", DataType::Utf8, true),
        Field::new("vkn", DataType::Utf8, true),
        Field::new("note", DataType::Int64, true),
    ]))
}

fn run(rows: Vec<(Option<&str>, Option<&str>)>, chunk: usize, mode: &str) -> (bool, u64, Vec<u64>) {
    let schema = schema();
    let batches = rows
        .chunks(chunk)
        .map(|slice| {
            let tckn: ArrayRef = Arc::new(StringArray::from(
                slice.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            ));
            let vkn: ArrayRef = Arc::new(StringArray::from(
                slice.iter().map(|(_, b)| *b).collect::<Vec<_>>(),
            ));
            // A column with no nulls at all carries no validity buffer; keeping one
            // here makes sure the rule is not quietly reading that absence.
            let note: ArrayRef = Arc::new(Int64Array::from(vec![1i64; slice.len()]));
            RecordBatch::try_new(Arc::clone(&schema), vec![tckn, vkn, note]).unwrap()
        })
        .collect::<Vec<_>>();
    let source = format!(
        r#"{{"version":"proofframe.contract.v2","status":"active","max_findings":20,
            "dataset_rules":{{"mutually_exclusive":[
              {{"name":"tax_id","columns":["tckn","vkn"],"mode":"{mode}"}}]}}}}"#
    );
    let document = ContractDocument::from_json(&source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(batches),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    let rows = report
        .findings
        .iter()
        .filter_map(|finding| finding.row)
        .collect();
    (report.valid, report.violation_count, rows)
}

#[test]
fn a_row_that_fills_both_columns_is_reported() {
    let rows = vec![
        (Some("111"), None),
        (None, Some("222")),
        (Some("333"), Some("444")),
        (None, None),
    ];
    let (valid, count, at) = run(rows, 4, "at_most_one");
    assert!(!valid);
    assert_eq!(count, 1);
    assert_eq!(at, vec![2]);
}

#[test]
fn at_most_one_accepts_an_empty_row_and_exactly_one_does_not() {
    let rows = vec![(Some("111"), None), (None, None)];
    let (valid, _, _) = run(rows.clone(), 2, "at_most_one");
    assert!(valid);

    let (valid, count, at) = run(rows, 2, "exactly_one");
    assert!(!valid);
    assert_eq!(count, 1);
    assert_eq!(at, vec![1], "the empty row is the one that fails");
}

/// The bitmap walks whole 64-bit words, so a violation that lands on a word edge is
/// the case most likely to be dropped by an off-by-one.
#[test]
fn violations_are_found_across_word_boundaries() {
    let mut rows = vec![(Some("x"), None); 200];
    for at in [0usize, 63, 64, 65, 127, 128, 199] {
        rows[at] = (Some("x"), Some("y"));
    }
    let (valid, count, found) = run(rows, 200, "at_most_one");
    assert!(!valid);
    assert_eq!(count, 7);
    assert_eq!(found, vec![0, 63, 64, 65, 127, 128, 199]);
}

/// A column with no nulls has no validity buffer in Arrow. Reading that absence as
/// "nothing is present" would invert the rule, so it is checked directly.
#[test]
fn a_column_without_nulls_counts_as_present_everywhere() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("always", DataType::Int64, false),
        Field::new("sometimes", DataType::Int64, true),
    ]));
    let always: ArrayRef = Arc::new(Int64Array::from(vec![1i64, 2, 3]));
    let sometimes: ArrayRef = Arc::new(Int64Array::from(vec![None, Some(9), None]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![always, sometimes]).unwrap();
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":20,
        "dataset_rules":{"mutually_exclusive":[
          {"name":"pair","columns":["always","sometimes"],"mode":"at_most_one"}]}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    assert_eq!(report.violation_count, 1, "row 1 fills both");
    assert_eq!(report.findings[0].row, Some(1));
}

#[test]
fn three_columns_need_two_present_before_it_is_a_violation() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int64, true),
        Field::new("b", DataType::Int64, true),
        Field::new("c", DataType::Int64, true),
    ]));
    let a: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), None, None, Some(1)]));
    let b: ArrayRef = Arc::new(Int64Array::from(vec![None, Some(1), None, Some(1)]));
    let c: ArrayRef = Arc::new(Int64Array::from(vec![None, None, Some(1), Some(1)]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![a, b, c]).unwrap();
    let source = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":20,
        "dataset_rules":{"mutually_exclusive":[
          {"name":"one_of_three","columns":["a","b","c"]}]}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].row, Some(3));
    assert!(
        report.findings[0].message.contains("a, b, c"),
        "{:?}",
        report.findings[0]
    );
}

#[test]
fn the_verdict_does_not_depend_on_batch_size() {
    let rows = vec![
        (Some("1"), None),
        (Some("2"), Some("2")),
        (None, None),
        (None, Some("4")),
    ];
    let whole = run(rows.clone(), 4, "at_most_one");
    for chunk in [1, 2, 3] {
        assert_eq!(
            run(rows.clone(), chunk, "at_most_one"),
            whole,
            "chunk {chunk}"
        );
    }
}

#[test]
fn a_rule_with_fewer_than_two_columns_is_refused() {
    let source = r#"{"version":"proofframe.contract.v2","status":"active",
        "dataset_rules":{"mutually_exclusive":[{"name":"x","columns":["tckn"]}]}}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let error = CompiledContract::compile_document(&document, schema().as_ref()).unwrap_err();
    assert!(
        error.to_string().contains("at least two columns"),
        "{error}"
    );
}
