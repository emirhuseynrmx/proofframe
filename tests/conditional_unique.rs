mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn data(ids: Vec<i64>, deleted: Vec<bool>, chunk: usize) -> (Arc<Schema>, Vec<RecordBatch>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("is_deleted", DataType::Boolean, true),
    ]));
    let rows: Vec<(i64, bool)> = ids.into_iter().zip(deleted).collect();
    let batches = rows
        .chunks(chunk.max(1))
        .map(|slice| {
            let id: ArrayRef = Arc::new(Int64Array::from(
                slice.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ));
            let flag: ArrayRef = Arc::new(BooleanArray::from(
                slice.iter().map(|(_, d)| *d).collect::<Vec<_>>(),
            ));
            RecordBatch::try_new(Arc::clone(&schema), vec![id, flag]).unwrap()
        })
        .collect();
    (schema, batches)
}

const CONTRACT: &str = r#"{"version":"proofframe.contract.v2","status":"active","max_findings":10,
    "dataset_rules":{"conditional_unique":[{
      "name":"live_ids","columns":["id"],
      "when":{"left":{"column":"is_deleted"},"op":"eq","right":{"literal":false}}}]}}"#;

fn run(schema: Arc<Schema>, batches: Vec<RecordBatch>) -> (bool, u64) {
    let document = ContractDocument::from_json(CONTRACT).unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();
    let report = execute_reader(
        reader_from_batches(batches),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    (report.valid, report.violation_count)
}

/// "Unique among the records that are not deleted" is a different claim from
/// "unique", and stating it as the second one fails on every tombstone.
#[test]
fn a_duplicate_among_deleted_rows_is_not_a_violation() {
    let (schema, batches) = data(vec![1, 1, 2], vec![true, true, false], 3);
    let (valid, count) = run(schema, batches);
    assert!(valid, "both duplicates are deleted");
    assert_eq!(count, 0);
}

#[test]
fn a_duplicate_among_selected_rows_is_a_violation() {
    let (schema, batches) = data(vec![1, 1, 2], vec![false, false, true], 3);
    let (valid, count) = run(schema, batches);
    assert!(!valid);
    assert_eq!(count, 1);
}

#[test]
fn a_key_repeated_across_the_condition_is_not_a_duplicate() {
    // The same id appears once live and once deleted: only one is selected.
    let (schema, batches) = data(vec![7, 7], vec![false, true], 2);
    let (valid, count) = run(schema, batches);
    assert!(valid);
    assert_eq!(count, 0);
}

#[test]
fn the_verdict_does_not_depend_on_the_batch_size() {
    let ids = vec![1, 2, 1, 3, 2];
    let deleted = vec![false, false, false, true, true];
    let whole = run(
        data(ids.clone(), deleted.clone(), 5).0,
        data(ids.clone(), deleted.clone(), 5).1,
    );
    for chunk in [1, 2, 3, 4] {
        let (schema, batches) = data(ids.clone(), deleted.clone(), chunk);
        assert_eq!(run(schema, batches), whole, "chunk {chunk}");
    }
}

#[test]
fn an_absent_rule_hashes_the_same_as_an_empty_one() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("is_deleted", DataType::Boolean, true),
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
            "dataset_rules":{"conditional_unique":[]}}"#,
    );
    assert_eq!(absent, empty);
    assert_ne!(absent, digest(CONTRACT));
}
