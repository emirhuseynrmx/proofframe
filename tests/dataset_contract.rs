mod support;

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Decimal128Array, Float64Array, Int64Array, StringArray,
    StringViewArray, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

use support::reader_from_batches;

fn compile(source: &str, schema: &Schema) -> CompiledContract {
    let document = ContractDocument::from_json(source).unwrap();
    CompiledContract::compile_document(&document, schema).unwrap()
}

#[test]
fn row_count_and_null_ratio_are_dataset_level_violations() {
    let schema = Arc::new(Schema::new(vec![Field::new("email", DataType::Utf8, true)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(StringArray::from(vec![Some("a@b.dev"), None, None])) as ArrayRef],
    )
    .unwrap();
    let plan = compile(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "dataset_rules":{
                "row_count":{"min":4},
                "null_ratio":{"email":{"max":0.5}}
            }
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert_eq!(report.violation_count, 2);
    assert_eq!(
        report
            .findings
            .iter()
            .map(|finding| finding.rule)
            .collect::<Vec<_>>(),
        vec!["row_count", "null_ratio"]
    );
}

#[test]
fn composite_unique_is_exact_across_record_batch_boundaries() {
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
            "dataset_rules":{
                "composite_unique":[{"name":"line_key","columns":["order_id","line_id"]}]
            }
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![
            make_batch(vec![1, 1], vec![1, 2]),
            make_batch(vec![1], vec![1]),
        ]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].rule, "composite_unique");
    assert_eq!(report.findings[0].row, Some(2));
    assert!(report.findings[0].message.contains("line_key"));
}

#[test]
fn distinct_count_and_ratio_use_exact_dataset_state() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "customer_id",
        DataType::Int64,
        false,
    )]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from_iter_values([1_i64, 1, 2, 3])) as ArrayRef],
    )
    .unwrap();
    let plan = compile(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "dataset_rules":{
                "distinct_count":{"customer_id":{"min":4}},
                "distinct_ratio":{"customer_id":{"min":1.0}}
            }
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert_eq!(report.violation_count, 2);
    assert_eq!(
        report
            .findings
            .iter()
            .map(|finding| finding.rule)
            .collect::<Vec<_>>(),
        vec!["distinct_count", "distinct_ratio"]
    );
}

#[test]
fn distinct_count_supports_every_fixed_width_scalar_family_exactly() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("enabled", DataType::Boolean, false),
        Field::new("unsigned_id", DataType::UInt64, false),
        Field::new("score", DataType::Float64, false),
        Field::new("amount", DataType::Decimal128(20, 2), false),
    ]));
    let amount = Decimal128Array::from_iter_values([10_001_i128, 10_001, 20_002])
        .with_precision_and_scale(20, 2)
        .unwrap();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(BooleanArray::from(vec![true, true, false])) as ArrayRef,
            Arc::new(UInt64Array::from_iter_values([
                9_007_199_254_740_993_u64,
                9_007_199_254_740_993,
                9_007_199_254_740_995,
            ])) as ArrayRef,
            Arc::new(Float64Array::from_iter_values([1.5, 1.5, 2.5])) as ArrayRef,
            Arc::new(amount) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "dataset_rules":{"distinct_count":{
                "enabled":{"min":3},
                "unsigned_id":{"min":3},
                "score":{"min":3},
                "amount":{"min":3}
            }}
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert_eq!(report.violation_count, 4);
    assert_eq!(
        report
            .findings
            .iter()
            .map(|finding| finding.column.as_str())
            .collect::<Vec<_>>(),
        vec!["enabled", "unsigned_id", "score", "amount"]
    );
}

#[test]
fn composite_unique_uses_type_tagged_canonical_scalar_keys() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("enabled", DataType::Boolean, false),
        Field::new("amount", DataType::Decimal128(20, 2), false),
        Field::new("label", DataType::Utf8View, false),
    ]));
    let amount = Decimal128Array::from_iter_values([10_001_i128, 20_002, 10_001])
        .with_precision_and_scale(20, 2)
        .unwrap();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(BooleanArray::from(vec![true, false, true])) as ArrayRef,
            Arc::new(amount) as ArrayRef,
            Arc::new(StringViewArray::from(vec!["alpha", "beta", "alpha"])) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "dataset_rules":{"composite_unique":[{
                "name":"typed_key","columns":["enabled","amount","label"]
            }]}
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].row, Some(2));
}
