mod support;

use std::num::NonZeroUsize;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    CompiledContract, ContractDocument, ErrorCode, ExecutionOptions, PartitionReader,
    check_partition_readers,
};

use support::reader_from_batches;

fn compile(source: &str, schema: &Schema) -> CompiledContract {
    let document = ContractDocument::from_json(source).unwrap();
    CompiledContract::compile_document(&document, schema).unwrap()
}

fn partition(batch: RecordBatch) -> PartitionReader {
    Box::new(reader_from_batches(vec![batch]))
}

#[test]
fn serial_and_parallel_partition_reports_are_byte_equivalent_and_ordered() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("low", DataType::Int64, false),
        Field::new("high", DataType::Int64, false),
    ]));
    let make_batch = |low: Vec<i64>, high: Vec<i64>| {
        RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(low)) as ArrayRef,
                Arc::new(Int64Array::from(high)) as ArrayRef,
            ],
        )
        .unwrap()
    };
    let plan = compile(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{
                "name":"ordered",
                "compare":{"left":{"column":"low"},"op":"lte","right":{"column":"high"}}
            }]
        }"#,
        schema.as_ref(),
    );
    let run = |threads| {
        check_partition_readers(
            vec![
                partition(make_batch(vec![1, 4], vec![2, 3])),
                partition(make_batch(vec![7, 5], vec![6, 8])),
            ],
            &plan,
            &ExecutionOptions {
                threads: Some(NonZeroUsize::new(threads).unwrap()),
                ..ExecutionOptions::default()
            },
        )
        .unwrap()
    };

    let serial = run(1);
    let parallel = run(2);

    assert_eq!(
        serde_json::to_value(&serial).unwrap(),
        serde_json::to_value(&parallel).unwrap()
    );
    assert_eq!(
        parallel
            .findings
            .iter()
            .map(|finding| finding.row)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
}

#[test]
fn partition_execution_fails_closed_on_a_schema_mismatch() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let wrong_schema = Arc::new(Schema::new(vec![Field::new(
        "other",
        DataType::Int64,
        false,
    )]));
    let plan = compile(
        r#"{"version":"proofframe.contract.v2","columns":{"id":{"not_null":true}}}"#,
        schema.as_ref(),
    );
    let wrong_batch = RecordBatch::try_new(
        wrong_schema,
        vec![Arc::new(Int64Array::from_iter_values([1])) as ArrayRef],
    )
    .unwrap();

    let error = check_partition_readers(
        vec![partition(wrong_batch)],
        &plan,
        &ExecutionOptions::default(),
    )
    .expect_err("partition schemas must match the compiled plan before scanning");

    assert_eq!(error.code(), ErrorCode::SchemaMismatch);
}

#[test]
fn exact_composite_uniqueness_remains_global_across_partitions() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("order_id", DataType::Int64, false),
        Field::new("line_id", DataType::Int64, false),
    ]));
    let make_batch = |orders: Vec<i64>, lines: Vec<i64>| {
        RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(orders)) as ArrayRef,
                Arc::new(Int64Array::from(lines)) as ArrayRef,
            ],
        )
        .unwrap()
    };
    let plan = compile(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "dataset_rules":{"composite_unique":[{
                "name":"line_key","columns":["order_id","line_id"]
            }]}
        }"#,
        schema.as_ref(),
    );

    let report = check_partition_readers(
        vec![
            partition(make_batch(vec![1, 1], vec![1, 2])),
            partition(make_batch(vec![1], vec![1])),
        ],
        &plan,
        &ExecutionOptions {
            threads: Some(NonZeroUsize::new(4).unwrap()),
            ..ExecutionOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].row, Some(2));
}
