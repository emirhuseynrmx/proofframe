mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::fingerprint_reader;

use support::reader_from_batches;

fn v1_golden_batch() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 2, 3])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])) as ArrayRef,
        ],
    )
    .expect("golden Arrow batch must be valid")
}

#[test]
fn v1_golden_vector_is_frozen() {
    assert_eq!(
        fingerprint_reader(reader_from_batches(vec![v1_golden_batch()])).unwrap(),
        "pf-fp-v1:4dc74e666725f040dea7e788827d0411e59c19a72b55f2ce27f22ed9a00afb42"
    );
}

#[test]
fn v1_remains_invariant_to_real_batch_segmentation() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let whole = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from(vec![1_i64, 2, 3, 4])) as ArrayRef],
    )
    .unwrap();
    let first = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef],
    )
    .unwrap();
    let second = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from(vec![3_i64, 4])) as ArrayRef],
    )
    .unwrap();

    let direct = fingerprint_reader(reader_from_batches(vec![whole])).unwrap();
    let segmented = fingerprint_reader(reader_from_batches(vec![first, second])).unwrap();

    assert_eq!(direct, segmented);
}
