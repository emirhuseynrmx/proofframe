mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    DiffOptions, DiffOutput, ErrorCode, ResourceLimits, diff_readers, diff_readers_with_options,
};
use tempfile::TempDir;

use support::reader_from_batches;

fn batch(rows: usize, suffix: &str) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let ids = (0..rows as i64).collect::<Vec<_>>();
    let values = (0..rows)
        .map(|row| format!("row-{row}-{suffix}"))
        .collect::<Vec<_>>();
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(ids)) as ArrayRef,
            Arc::new(StringArray::from(values)) as ArrayRef,
        ],
    )
    .unwrap()
}

fn options(samples: usize, output: Option<DiffOutput>) -> DiffOptions {
    DiffOptions {
        resources: ResourceLimits {
            max_memory_bytes: 64 * 1024 * 1024,
            max_temp_bytes: 256 * 1024 * 1024,
            max_output_records: 20_000,
            max_samples: samples,
        },
        max_samples: samples,
        output,
        ..DiffOptions::default()
    }
}

#[test]
fn report_samples_are_bounded_while_jsonl_receives_every_change() {
    let directory = TempDir::new().unwrap();
    let output = directory.path().join("full-diff.jsonl");
    let report = diff_readers_with_options(
        reader_from_batches(vec![batch(10_000, "before")]),
        reader_from_batches(vec![batch(10_000, "after")]),
        &["id".to_string()],
        &options(3, Some(DiffOutput::JsonLines(output.clone()))),
    )
    .unwrap();

    assert_eq!(report.changed_count, 10_000);
    assert_eq!(report.changed.len(), 3);
    assert!(report.truncated);
    assert_eq!(report.metrics.output_records, 10_000);
    assert_eq!(
        std::fs::read_to_string(output).unwrap().lines().count(),
        10_000
    );
}

#[test]
fn failed_output_limit_never_publishes_a_partial_target() {
    let directory = TempDir::new().unwrap();
    let output = directory.path().join("must-not-exist.jsonl");
    let mut options = options(1, Some(DiffOutput::JsonLines(output.clone())));
    options.resources.max_output_records = 2;

    let error = diff_readers_with_options(
        reader_from_batches(vec![batch(3, "before")]),
        reader_from_batches(vec![batch(3, "after")]),
        &["id".to_string()],
        &options,
    )
    .unwrap_err();

    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(!output.exists());
}

#[test]
fn duplicate_keys_and_schema_mismatches_still_fail_closed() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let duplicate = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 1])) as ArrayRef,
            Arc::new(StringArray::from(vec!["a", "b"])) as ArrayRef,
        ],
    )
    .unwrap();
    let error = diff_readers(
        reader_from_batches(vec![duplicate]),
        reader_from_batches(vec![batch(1, "after")]),
        &["id".to_string()],
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::DuplicateKey);

    let incompatible = RecordBatch::try_from_iter(vec![(
        "id",
        Arc::new(StringArray::from(vec!["0"])) as ArrayRef,
    )])
    .unwrap();
    let error = diff_readers(
        reader_from_batches(vec![batch(1, "before")]),
        reader_from_batches(vec![incompatible]),
        &["id".to_string()],
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::SchemaMismatch);
}

#[test]
fn partition_temp_usage_is_charged_before_writing() {
    let mut options = options(1, None);
    options.resources.max_temp_bytes = 128;
    let error = diff_readers_with_options(
        reader_from_batches(vec![batch(100, "before")]),
        reader_from_batches(vec![batch(100, "after")]),
        &["id".to_string()],
        &options,
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
}

#[test]
fn arrow_ipc_sink_is_atomic_and_readable() {
    let directory = TempDir::new().unwrap();
    let output = directory.path().join("diff.arrow");
    let report = diff_readers_with_options(
        reader_from_batches(vec![batch(2, "before")]),
        reader_from_batches(vec![batch(2, "after")]),
        &["id".to_string()],
        &options(2, Some(DiffOutput::ArrowIpc(output.clone()))),
    )
    .unwrap();

    let reader =
        arrow::ipc::reader::FileReader::try_new(std::fs::File::open(output).unwrap(), None)
            .unwrap();
    let rows = reader.map(|batch| batch.unwrap().num_rows()).sum::<usize>();
    assert_eq!(rows, 2);
    assert_eq!(report.metrics.output_records, 2);
}
