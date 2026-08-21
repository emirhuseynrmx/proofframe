mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::error::ArrowError;
use arrow::record_batch::{RecordBatch, RecordBatchIterator};
use proofframe::{
    CancellationToken, ColumnContract, CompiledContract, Contract, ContractAst, ContractDocument,
    ErrorCode, ExecutionOptions, ResourceLimits, execute_reader, validate_fast_reader,
};

use support::reader_from_batches;

fn compile(columns: &str, schema: &Schema) -> CompiledContract {
    let source =
        format!(r#"{{"version":"proofframe.contract.v1","columns":{columns},"max_findings":16}}"#);
    let ast = ContractAst::from_json(&source).unwrap();
    CompiledContract::compile(&ast, schema).unwrap()
}

fn compile_v2(source: &str, schema: &Schema) -> CompiledContract {
    let document = ContractDocument::from_json(source).unwrap();
    CompiledContract::compile_document(&document, schema).unwrap()
}

#[test]
fn exact_u64_range_reports_the_value_above_f64_precision() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::UInt64, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(UInt64Array::from(vec![
            9_007_199_254_740_993,
            9_007_199_254_740_994,
        ])) as ArrayRef],
    )
    .unwrap();
    let plan = compile(r#"{"id":{"min":9007199254740994}}"#, schema.as_ref());

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert!(!report.valid);
    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].rule, "min");
    assert_eq!(report.findings[0].column, "id");
    assert_eq!(report.findings[0].row, Some(0));
}

#[test]
fn compiled_kernels_preserve_rule_and_row_semantics() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("signed", DataType::Int64, false),
        Field::new("score", DataType::Float64, true),
        Field::new("name", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![i64::MIN, 0, i64::MAX])) as ArrayRef,
            Arc::new(Float64Array::from(vec![Some(0.5), Some(f64::NAN), None])) as ArrayRef,
            Arc::new(StringArray::from(vec!["ok", "BAD", "other"])) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile(
        r#"{
            "signed":{"min":-9223372036854775808,"max":9223372036854775807},
            "score":{"not_null":true,"min":0,"max":1,"nan":"reject"},
            "name":{"pattern":"^[a-z]+$","allowed":["ok","other"]}
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();
    let keys = report
        .findings
        .iter()
        .map(|finding| (finding.rule, finding.column.as_str(), finding.row))
        .collect::<Vec<_>>();

    assert_eq!(
        keys,
        vec![
            ("nan", "score", Some(1)),
            ("not_null", "score", Some(2)),
            ("pattern", "name", Some(1)),
            ("allowed", "name", Some(1)),
        ]
    );
}

#[test]
fn resource_sample_limit_bounds_retained_validation_findings() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from(vec![None, None, None, None, None])) as ArrayRef],
    )
    .unwrap();
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"not_null":true}},"max_findings":1000000}"#,
    )
    .unwrap();
    let plan = CompiledContract::compile(&ast, schema.as_ref()).unwrap();
    let options = ExecutionOptions {
        resources: ResourceLimits {
            max_samples: 3,
            ..ResourceLimits::default()
        },
        ..ExecutionOptions::default()
    };

    let report = execute_reader(reader_from_batches(vec![batch]), &plan, &options).unwrap();

    assert_eq!(report.violation_count, 5);
    assert_eq!(report.findings.len(), 3);
    assert!(report.truncated);
    assert!(report.metrics.peak_memory_bytes > 0);
}

#[test]
fn schema_changing_reader_fails_closed_without_indexing_the_batch() {
    let advertised = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let changed = Arc::new(Schema::new(vec![Field::new(
        "other",
        DataType::Utf8,
        false,
    )]));
    let batch = RecordBatch::try_new(
        changed,
        vec![Arc::new(StringArray::from(vec!["not-an-integer"])) as ArrayRef],
    )
    .unwrap();
    let reader = RecordBatchIterator::new(
        vec![Ok::<RecordBatch, ArrowError>(batch)].into_iter(),
        advertised.clone(),
    );
    let plan = compile(r#"{"id":{"not_null":true}}"#, advertised.as_ref());

    let error = execute_reader(reader, &plan, &ExecutionOptions::default())
        .expect_err("a stream batch must match the schema used to compile the plan");

    assert_eq!(error.code(), ErrorCode::SchemaMismatch);
}

#[test]
fn legacy_fast_api_uses_the_compiled_nan_policy() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "score",
        DataType::Float64,
        false,
    )]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Float64Array::from(vec![f64::NAN])) as ArrayRef],
    )
    .unwrap();
    let contract = Contract {
        columns: [(
            String::from("score"),
            ColumnContract {
                min: Some(0.0),
                ..ColumnContract::default()
            },
        )]
        .into_iter()
        .collect(),
        max_findings: 16,
    };

    let report = validate_fast_reader(reader_from_batches(vec![batch]), &contract).unwrap();

    assert!(!report.valid);
    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].rule, "nan");
    assert_eq!(report.findings[0].row, Some(0));
}

#[test]
fn execution_checks_cancellation_before_scanning_a_batch() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from(vec![1_i64])) as ArrayRef],
    )
    .unwrap();
    let plan = compile(r#"{"id":{"not_null":true}}"#, schema.as_ref());
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions {
            cancellation,
            ..ExecutionOptions::default()
        },
    )
    .unwrap_err();

    assert_eq!(error.code(), ErrorCode::Cancelled);
}

#[test]
fn relational_comparison_respects_null_policy_and_global_rows() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("start", DataType::Int64, true),
        Field::new("end", DataType::Int64, true),
    ]));
    let first = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![Some(1), Some(3)])) as ArrayRef,
            Arc::new(Int64Array::from(vec![Some(2), Some(2)])) as ArrayRef,
        ],
    )
    .unwrap();
    let second = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![None])) as ArrayRef,
            Arc::new(Int64Array::from(vec![Some(4)])) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile_v2(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{
                "name":"valid_window",
                "compare":{
                    "left":{"column":"start"},"op":"lte",
                    "right":{"column":"end"},"nulls":"fail"
                }
            }]
        }"#,
        schema.as_ref(),
    );

    let report = execute_reader(
        reader_from_batches(vec![first, second]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap();

    assert_eq!(report.violation_count, 2);
    assert_eq!(
        report
            .findings
            .iter()
            .map(|finding| finding.row)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
    assert!(
        report
            .findings
            .iter()
            .all(|finding| finding.rule == "compare")
    );
    assert!(
        report
            .findings
            .iter()
            .all(|finding| finding.message.contains("valid_window"))
    );
}

#[test]
fn conditional_rule_evaluates_native_utf8_and_asserts_not_null() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("status", DataType::Utf8, false),
        Field::new("shipped_at", DataType::Int64, true),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["shipped", "pending"])) as ArrayRef,
            Arc::new(Int64Array::from(vec![None, None])) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile_v2(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{
                "name":"shipped_has_timestamp",
                "when":{"left":{"column":"status"},"op":"eq","right":{"literal":"shipped"}},
                "assert":{"column":"shipped_at","not_null":true}
            }]
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
    assert_eq!(report.findings[0].row, Some(0));
    assert_eq!(report.findings[0].rule, "conditional");
    assert!(report.findings[0].message.contains("shipped_has_timestamp"));
}
