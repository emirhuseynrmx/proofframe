mod support;

use std::num::NonZeroUsize;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    CompiledContract, ContractDocument, ErrorCode, ExecutionOptions, PartitionReader,
    check_partition_readers, check_partition_readers_with_evidence,
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
    let make_batch = |low: &[i64], high: &[i64]| {
        RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from_iter_values(low.iter().copied())) as ArrayRef,
                Arc::new(Int64Array::from_iter_values(high.iter().copied())) as ArrayRef,
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
                partition(make_batch(&[1, 4], &[2, 3])),
                partition(make_batch(&[7, 5], &[6, 8])),
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
    let make_batch = |orders: &[i64], lines: &[i64]| {
        RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from_iter_values(orders.iter().copied())) as ArrayRef,
                Arc::new(Int64Array::from_iter_values(lines.iter().copied())) as ArrayRef,
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
            partition(make_batch(&[1, 1], &[1, 2])),
            partition(make_batch(&[1], &[1])),
        ],
        &plan,
        &ExecutionOptions {
            threads: Some(NonZeroUsize::new(4).unwrap()),
            ..ExecutionOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.violation_count, 1);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| { finding.rule == "composite_unique" && finding.row == Some(2) })
    );
}

#[test]
fn partition_manifest_is_emitted_from_the_same_global_validation_traversal() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let make_batch = |values: &[i64]| {
        RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from_iter_values(values.iter().copied())) as ArrayRef],
        )
        .unwrap()
    };
    let plan = compile(
        r#"{"version":"proofframe.contract.v2","columns":{"id":{"not_null":true}}}"#,
        schema.as_ref(),
    );
    let source_digest = format!("pf-contract-v2:{}", "2".repeat(64));

    let (report, manifest) = check_partition_readers_with_evidence(
        vec![
            partition(make_batch(&[1, 2])),
            partition(make_batch(&[3, 4, 5])),
        ],
        &plan,
        &source_digest,
        &ExecutionOptions {
            threads: Some(NonZeroUsize::new(2).unwrap()),
            ..ExecutionOptions::default()
        },
    )
    .unwrap();

    assert!(report.valid);
    assert_eq!(report.rows, 5);
    assert_eq!(manifest.partitions.len(), 2);
    assert_eq!(manifest.partitions[0].rows, 2);
    assert_eq!(manifest.partitions[1].rows, 3);
    assert_eq!(manifest.partitions[0].index, 0);
    assert_eq!(manifest.partitions[1].index, 1);
    manifest.validate().unwrap();
}
