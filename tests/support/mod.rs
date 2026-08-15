#![allow(dead_code)]

use arrow::error::ArrowError;
use arrow::record_batch::{RecordBatch, RecordBatchIterator, RecordBatchReader};

pub fn reader_from_batches(batches: Vec<RecordBatch>) -> impl RecordBatchReader {
    let schema = batches.first().expect("at least one batch").schema();
    RecordBatchIterator::new(
        batches.into_iter().map(Ok::<RecordBatch, ArrowError>),
        schema,
    )
}
