mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{ErrorCode, LeakageOptions, ResourceLimits, detect_leakage_with_options};

use support::reader_from_batches;

fn options(memory: u64, temp: u64) -> LeakageOptions {
    LeakageOptions {
        resources: ResourceLimits {
            max_memory_bytes: memory,
            max_temp_bytes: temp,
            max_output_records: 0,
            max_samples: 4,
        },
        max_samples: 4,
        ..LeakageOptions::default()
    }
}

#[test]
fn keyed_identity_uses_logical_tuple_order_not_physical_column_index() {
    let left = RecordBatch::try_from_iter(vec![
        ("id", Arc::new(Int64Array::from(vec![7_i64])) as ArrayRef),
        (
            "feature",
            Arc::new(StringArray::from(vec!["left"])) as ArrayRef,
        ),
    ])
    .unwrap();
    let right = RecordBatch::try_from_iter(vec![
        (
            "feature",
            Arc::new(StringArray::from(vec!["right"])) as ArrayRef,
        ),
        ("id", Arc::new(Int64Array::from(vec![7_i64])) as ArrayRef),
    ])
    .unwrap();

    let report = detect_leakage_with_options(
        reader_from_batches(vec![left]),
        reader_from_batches(vec![right]),
        &["id".to_string()],
        &options(32 * 1024, 1024 * 1024),
    )
    .unwrap();

    assert_eq!(report.overlap_count, 1);
    assert_eq!(report.sample_fingerprints.len(), 1);
}

#[test]
fn full_row_identity_rejects_equal_names_with_different_arrow_types() {
    let integers = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
        vec![Arc::new(Int64Array::from(vec![1_i64])) as ArrayRef],
    )
    .unwrap();
    let strings = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("id", DataType::Utf8, false)])),
        vec![Arc::new(StringArray::from(vec!["1"])) as ArrayRef],
    )
    .unwrap();

    let error = detect_leakage_with_options(
        reader_from_batches(vec![integers]),
        reader_from_batches(vec![strings]),
        &[],
        &options(32 * 1024, 1024 * 1024),
    )
    .unwrap_err();

    assert_eq!(error.code(), ErrorCode::SchemaMismatch);
}

#[test]
fn duplicate_rows_do_not_inflate_distinct_overlap() {
    let train = RecordBatch::try_from_iter(vec![(
        "id",
        Arc::new(Int64Array::from(vec![1_i64, 1, 2])) as ArrayRef,
    )])
    .unwrap();
    let test = RecordBatch::try_from_iter(vec![(
        "id",
        Arc::new(Int64Array::from(vec![1_i64, 1, 3])) as ArrayRef,
    )])
    .unwrap();

    let report = detect_leakage_with_options(
        reader_from_batches(vec![train]),
        reader_from_batches(vec![test]),
        &["id".to_string()],
        &options(32 * 1024, 1024 * 1024),
    )
    .unwrap();

    assert_eq!(report.overlap_count, 1);
    assert_eq!(report.train_overlap_rate, 0.5);
    assert_eq!(report.test_overlap_rate, 0.5);
}

#[test]
fn leakage_spill_honors_the_temp_budget() {
    let train = RecordBatch::try_from_iter(vec![(
        "id",
        Arc::new(Int64Array::from_iter_values(0..10_000_i64)) as ArrayRef,
    )])
    .unwrap();
    let test = train.clone();

    let error = detect_leakage_with_options(
        reader_from_batches(vec![train]),
        reader_from_batches(vec![test]),
        &["id".to_string()],
        &options(256, 0),
    )
    .unwrap_err();

    assert_eq!(error.code(), ErrorCode::ResourceLimit);
}
