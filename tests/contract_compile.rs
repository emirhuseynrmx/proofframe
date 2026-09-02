use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use proofframe::{
    CompiledContract, ContractAst, ContractDocument, ContractVersion, ErrorCode, KernelKind,
    NaNPolicy, TypedBound,
};

fn contract_with_columns(columns: &str) -> ContractAst {
    ContractAst::from_json(&format!(
        r#"{{"version":"proofframe.contract.v1","columns":{columns}}}"#
    ))
    .expect("test contract syntax must be valid")
}

#[test]
fn unknown_contract_fields_fail_closed() {
    let error = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{},"max_findngs":4}"#,
    )
    .expect_err("an unknown root field must not be ignored");

    assert_eq!(error.code(), ErrorCode::ContractUnknownField);
    assert_eq!(error.path(), Some("$.max_findngs"));
}

#[test]
fn unknown_rule_fields_fail_closed_with_the_column_path() {
    let error = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"requried":true}}}"#,
    )
    .expect_err("an unknown rule field must not be ignored");

    assert_eq!(error.code(), ErrorCode::ContractUnknownField);
    assert_eq!(error.path(), Some("$.columns.id.requried"));
}

#[test]
fn bound_literals_preserve_integer_text_above_f64_precision() {
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"min":9007199254740993}}}"#,
    )
    .expect("the exact JSON integer is a valid syntax literal");

    assert_eq!(
        ast.columns["id"].min.as_ref().map(|bound| bound.as_text()),
        Some("9007199254740993")
    );
}

#[test]
fn malformed_json_has_a_distinct_stable_error_code() {
    let error = ContractAst::from_json("{").expect_err("truncated JSON must fail");

    assert_eq!(error.code(), ErrorCode::ContractInvalidJson);
    assert_eq!(error.path(), None);
}

#[test]
fn unsupported_rule_type_pair_is_a_compile_error() {
    let schema = Schema::new(vec![Field::new("payload", DataType::Binary, false)]);
    let ast = contract_with_columns(r#"{"payload":{"min":1}}"#);

    let error = CompiledContract::compile(&ast, &schema)
        .expect_err("binary columns do not have numeric ordering semantics");

    assert_eq!(error.code(), ErrorCode::ContractTypeMismatch);
    assert_eq!(error.path(), Some("$.columns.payload.min"));
}

#[test]
fn unsigned_bound_above_f64_precision_remains_exact() {
    let schema = Schema::new(vec![Field::new("id", DataType::UInt64, false)]);
    let ast = contract_with_columns(r#"{"id":{"min":9007199254740993}}"#);

    let plan = CompiledContract::compile(&ast, &schema).expect("u64 bound must compile");
    let column = &plan.columns()[0];

    assert_eq!(column.column_index(), 0);
    assert_eq!(column.kernel(), &KernelKind::U64);
    assert_eq!(
        column.rules().min(),
        Some(&TypedBound::U64(9_007_199_254_740_993))
    );
}

#[test]
fn signed_integer_extrema_compile_without_float_conversion() {
    let schema = Schema::new(vec![Field::new("id", DataType::Int64, false)]);
    let ast = contract_with_columns(&format!(
        r#"{{"id":{{"min":{},"max":{}}}}}"#,
        i64::MIN,
        i64::MAX
    ));

    let plan = CompiledContract::compile(&ast, &schema).expect("i64 extrema must compile");
    let rules = plan.columns()[0].rules();

    assert_eq!(rules.min(), Some(&TypedBound::I64(i64::MIN)));
    assert_eq!(rules.max(), Some(&TypedBound::I64(i64::MAX)));
}

#[test]
fn decimal_bound_requires_the_declared_scale() {
    let schema = Schema::new(vec![Field::new(
        "amount",
        DataType::Decimal128(20, 4),
        false,
    )]);
    let valid = contract_with_columns(r#"{"amount":{"min":"-12.3400","max":"99.9999"}}"#);
    let plan = CompiledContract::compile(&valid, &schema).expect("scaled decimals must compile");

    assert_eq!(
        plan.columns()[0].rules().min(),
        Some(&TypedBound::Decimal128 {
            value: -123_400,
            scale: 4,
        })
    );

    let invalid = contract_with_columns(r#"{"amount":{"min":"1.23456"}}"#);
    let error = CompiledContract::compile(&invalid, &schema)
        .expect_err("extra non-zero fractional digits must not be rounded");
    assert_eq!(error.code(), ErrorCode::ContractInvalidBound);
    assert_eq!(error.path(), Some("$.columns.amount.min"));
}

#[test]
fn timestamp_bounds_retain_the_arrow_unit() {
    let schema = Schema::new(vec![Field::new(
        "created_at",
        DataType::Timestamp(TimeUnit::Nanosecond, None),
        false,
    )]);
    let ast = contract_with_columns(
        r#"{"created_at":{"min":1700000000000000000,"max":1700000000000000001}}"#,
    );

    let plan = CompiledContract::compile(&ast, &schema).expect("timestamp ticks must compile");

    assert_eq!(
        plan.columns()[0].rules().min(),
        Some(&TypedBound::Timestamp {
            value: 1_700_000_000_000_000_000,
            unit: TimeUnit::Nanosecond,
        })
    );
}

#[test]
fn iso_8601_timestamp_bounds_normalize_offsets_to_exact_arrow_ticks() {
    let schema = Schema::new(vec![Field::new(
        "created_at",
        DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        false,
    )]);
    let ast = contract_with_columns(
        r#"{"created_at":{"min":"2023-11-14T22:13:20.123456Z","max":"2023-11-14T23:13:20.123456+01:00"}}"#,
    );

    let plan =
        CompiledContract::compile(&ast, &schema).expect("RFC 3339 ISO-8601 bounds must compile");
    let expected = TypedBound::Timestamp {
        value: 1_700_000_000_123_456,
        unit: TimeUnit::Microsecond,
    };

    assert_eq!(plan.columns()[0].rules().min(), Some(&expected));
    assert_eq!(plan.columns()[0].rules().max(), Some(&expected));
}

#[test]
fn iso_8601_timestamp_bounds_reject_precision_below_the_arrow_unit() {
    let schema = Schema::new(vec![Field::new(
        "created_at",
        DataType::Timestamp(TimeUnit::Millisecond, None),
        false,
    )]);
    let ast = contract_with_columns(r#"{"created_at":{"min":"2023-11-14T22:13:20.123456Z"}}"#);

    let error = CompiledContract::compile(&ast, &schema)
        .expect_err("sub-millisecond bounds must not be silently rounded");

    assert_eq!(error.code(), ErrorCode::ContractInvalidBound);
    assert_eq!(error.path(), Some("$.columns.created_at.min"));
}

#[test]
fn invalid_regex_and_reversed_bounds_fail_during_compilation() {
    let text_schema = Schema::new(vec![Field::new("name", DataType::Utf8, false)]);
    let regex_ast = contract_with_columns(r#"{"name":{"pattern":"["}}"#);
    let regex_error = CompiledContract::compile(&regex_ast, &text_schema)
        .expect_err("an invalid regex must not reach a scan");
    assert_eq!(regex_error.code(), ErrorCode::ContractInvalidBound);
    assert_eq!(regex_error.path(), Some("$.columns.name.pattern"));

    let number_schema = Schema::new(vec![Field::new("value", DataType::Int64, false)]);
    let bounds_ast = contract_with_columns(r#"{"value":{"min":10,"max":1}}"#);
    let bounds_error = CompiledContract::compile(&bounds_ast, &number_schema)
        .expect_err("min greater than max must not reach a scan");
    assert_eq!(bounds_error.code(), ErrorCode::ContractInvalidBound);
    assert_eq!(bounds_error.path(), Some("$.columns.value"));
}

#[test]
fn required_missing_columns_fail_compilation_deterministically() {
    let schema = Schema::empty();
    let ast = contract_with_columns(r#"{"z":{"required":true},"a":{"required":true}}"#);

    let error = CompiledContract::compile(&ast, &schema)
        .expect_err("a missing required column must fail before a row is scanned");

    assert_eq!(error.code(), ErrorCode::MissingColumn);
    assert_eq!(error.path(), Some("$.columns.a"));
}

#[test]
fn source_plan_and_schema_digests_are_separate_versioned_contracts() {
    let schema = Schema::new(vec![Field::new("id", DataType::Int64, false)]);
    let source =
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"min":0}},"max_findings":8}"#;
    let ast = ContractAst::from_json(source).unwrap();
    let plan = CompiledContract::compile(&ast, &schema).unwrap();

    let source_digest = proofframe::evidence::contract_source_digest(source).unwrap();
    assert!(source_digest.starts_with("pf-contract-v1:"));
    assert!(
        plan.compiled_plan_digest()
            .unwrap()
            .starts_with("pf-plan-v1:")
    );
    assert_eq!(
        plan.compiled_plan_digest().unwrap(),
        "pf-plan-v1:3a243dfa197e9fa4858c58f334333c0b4ed48bef82aeaa3751912d1092d74c2c"
    );
    assert!(plan.schema_digest().unwrap().starts_with("pf-schema-v1:"));
    assert_ne!(
        source_digest[15..],
        plan.compiled_plan_digest().unwrap()[11..]
    );
    assert_ne!(
        plan.compiled_plan_digest().unwrap()[11..],
        plan.schema_digest().unwrap()[13..]
    );
}

#[test]
fn contract_source_digest_preserves_integer_literals_outside_f64_precision() {
    let source =
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"max":18446744073709551615}}}"#;

    let digest = proofframe::evidence::contract_source_digest(source)
        .expect("valid exact integer contracts must have a canonical digest");

    assert!(digest.starts_with("pf-contract-v1:"));
}

#[test]
fn value_rules_on_an_absent_optional_column_fail_closed() {
    let schema = Schema::empty();
    let ast = contract_with_columns(r#"{"score":{"min":0}}"#);

    let error = CompiledContract::compile(&ast, &schema)
        .expect_err("a value rule must not be silently skipped");

    assert_eq!(error.code(), ErrorCode::MissingColumn);
    assert_eq!(error.path(), Some("$.columns.score"));
}

#[test]
fn float_nan_policy_is_explicit_in_the_compiled_plan() {
    let schema = Schema::new(vec![Field::new("score", DataType::Float64, false)]);
    let default_ast = contract_with_columns(r#"{"score":{"min":0}}"#);
    let allow_ast = contract_with_columns(r#"{"score":{"min":0,"nan":"allow"}}"#);

    let default_plan = CompiledContract::compile(&default_ast, &schema).unwrap();
    let allow_plan = CompiledContract::compile(&allow_ast, &schema).unwrap();

    assert_eq!(default_plan.columns()[0].rules().nan(), NaNPolicy::Reject);
    assert_eq!(allow_plan.columns()[0].rules().nan(), NaNPolicy::Allow);
}

#[test]
fn integer_bounds_must_fit_the_physical_arrow_domain() {
    let cases = [
        (DataType::Int8, r#"{"value":{"min":-129}}"#, "min"),
        (DataType::Int16, r#"{"value":{"max":32768}}"#, "max"),
        (DataType::UInt8, r#"{"value":{"max":256}}"#, "max"),
        (DataType::UInt16, r#"{"value":{"min":65536}}"#, "min"),
        (DataType::Date32, r#"{"value":{"min":2147483648}}"#, "min"),
    ];

    for (data_type, columns, rule) in cases {
        let schema = Schema::new(vec![Field::new("value", data_type, false)]);
        let ast = contract_with_columns(columns);
        let error = CompiledContract::compile(&ast, &schema)
            .expect_err("an out-of-domain bound must fail during compilation");
        let expected_path = format!("$.columns.value.{rule}");

        assert_eq!(error.code(), ErrorCode::ContractInvalidBound);
        assert_eq!(error.path(), Some(expected_path.as_str()));
    }
}

#[test]
fn contract_v2_parses_relational_and_dataset_rules_strictly() {
    let document = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{
                "order_id":{"required":true,"not_null":true,"type":"int64"}
            },
            "row_rules":[{
                "name":"valid_window",
                "compare":{
                    "left":{"column":"start_at"},
                    "op":"lte",
                    "right":{"column":"end_at"},
                    "nulls":"skip"
                }
            }],
            "dataset_rules":{
                "row_count":{"min":1},
                "null_ratio":{"order_id":{"max":0.01}},
                "composite_unique":[{
                    "name":"order_line_key",
                    "columns":["order_id","line_id"],
                    "nulls":"equal"
                }]
            }
        }"#,
    )
    .expect("a valid V2 contract must parse");

    assert_eq!(document.version(), ContractVersion::V2);
    let v2 = document
        .as_v2()
        .expect("the dispatcher must retain the V2 AST");
    assert_eq!(v2.row_rules.len(), 1);
    assert_eq!(v2.dataset_rules.composite_unique.len(), 1);
}

#[test]
fn contract_v2_rejects_unknown_fields_with_an_exact_path() {
    let error = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{"name":"broken","comprae":{}}]
        }"#,
    )
    .expect_err("unknown V2 fields must fail closed");

    assert_eq!(error.code(), ErrorCode::ContractUnknownField);
    assert_eq!(error.path(), Some("$.row_rules[0].comprae"));
}

#[test]
fn contract_v2_rejects_duplicate_rule_names() {
    let error = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[
                {"name":"same","compare":{"left":{"column":"a"},"op":"eq","right":{"column":"b"}}},
                {"name":"same","compare":{"left":{"column":"a"},"op":"ne","right":{"column":"b"}}}
            ]
        }"#,
    )
    .expect_err("rule names identify findings and must be unique");

    assert_eq!(error.code(), ErrorCode::ContractDuplicateRule);
    assert_eq!(error.path(), Some("$.row_rules[1].name"));
}

#[test]
fn contract_v2_rejects_ratios_outside_the_unit_interval() {
    let error = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "dataset_rules":{"null_ratio":{"email":{"max":1.01}}}
        }"#,
    )
    .expect_err("ratios above one must fail before compilation");

    assert_eq!(error.code(), ErrorCode::ContractInvalidRatio);
    assert_eq!(error.path(), Some("$.dataset_rules.null_ratio.email.max"));
}

#[test]
fn contract_v2_compiles_relational_and_dataset_plans_against_arrow_schema() {
    let schema = Schema::new(vec![
        Field::new("order_id", DataType::Int64, false),
        Field::new("line_id", DataType::Int64, false),
        Field::new("start_at", DataType::Int64, true),
        Field::new("end_at", DataType::Int64, true),
    ]);
    let document = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{"order_id":{"required":true,"type":"int64"}},
            "row_rules":[{
                "name":"valid_window",
                "compare":{"left":{"column":"start_at"},"op":"lte","right":{"column":"end_at"}}
            }],
            "dataset_rules":{
                "row_count":{"min":1},
                "composite_unique":[{"name":"line_key","columns":["order_id","line_id"]}]
            }
        }"#,
    )
    .unwrap();

    let plan = CompiledContract::compile_document(&document, &schema)
        .expect("valid V2 rules must compile before scanning");

    assert_eq!(plan.row_plans().len(), 1);
    assert_eq!(plan.row_plans()[0].name(), "valid_window");
    assert_eq!(plan.dataset_plan().composite_unique().len(), 1);
    assert!(
        plan.compiled_plan_digest()
            .unwrap()
            .starts_with("pf-plan-v2:")
    );
}

#[test]
fn contract_v2_exact_type_assertions_fail_before_scanning() {
    let schema = Schema::new(vec![Field::new("order_id", DataType::UInt64, false)]);
    let document = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{"order_id":{"type":"int64"}}
        }"#,
    )
    .unwrap();

    let error = CompiledContract::compile_document(&document, &schema)
        .expect_err("V2 type assertions never cast the Arrow input");

    assert_eq!(error.code(), ErrorCode::ContractTypeMismatch);
    assert_eq!(error.path(), Some("$.columns.order_id.type"));
}

#[test]
fn compile_document_preserves_the_frozen_v1_plan_digest() {
    let schema = Schema::new(vec![Field::new("id", DataType::Int64, false)]);
    let source =
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"min":0}},"max_findings":8}"#;
    let document = ContractDocument::from_json(source).unwrap();
    let plan = CompiledContract::compile_document(&document, &schema).unwrap();

    assert_eq!(
        plan.compiled_plan_digest().unwrap(),
        "pf-plan-v1:3a243dfa197e9fa4858c58f334333c0b4ed48bef82aeaa3751912d1092d74c2c"
    );
}

#[test]
fn draft_v2_contract_is_rejected_at_the_shared_compile_boundary() {
    let schema = Schema::new(vec![Field::new("id", DataType::Int64, false)]);
    let document = ContractDocument::from_json(
        r#"{"version":"proofframe.contract.v2","status":"draft","columns":{}}"#,
    )
    .expect("draft metadata must parse so the compiler can reject execution");

    let error = CompiledContract::compile_document(&document, &schema)
        .expect_err("draft contracts must never compile into executable plans");

    assert_eq!(error.code().as_str(), "PF_DRAFT_CONTRACT");
}

#[test]
fn contract_v2_rejects_relational_literals_outside_the_arrow_physical_type() {
    let schema = Schema::new(vec![Field::new("tiny", DataType::Int8, false)]);
    let document = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{
                "name":"tiny_limit",
                "compare":{"left":{"column":"tiny"},"op":"lte","right":{"literal":128}}
            }]
        }"#,
    )
    .unwrap();

    let error = CompiledContract::compile_document(&document, &schema)
        .expect_err("an Int8 literal must never be truncated at execution time");

    assert_eq!(error.code(), ErrorCode::ContractTypeMismatch);
    assert_eq!(error.path(), Some("$.row_rules[0].compare.right.literal"));
}

#[test]
fn contract_v2_rejects_ordering_for_unordered_relational_types() {
    let schema = Schema::new(vec![Field::new("enabled", DataType::Boolean, false)]);
    let document = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{
                "name":"invalid_boolean_order",
                "compare":{"left":{"column":"enabled"},"op":"lt","right":{"literal":true}}
            }]
        }"#,
    )
    .unwrap();

    let error = CompiledContract::compile_document(&document, &schema)
        .expect_err("boolean contracts support equality, not invented ordering semantics");

    assert_eq!(error.code(), ErrorCode::ContractTypeMismatch);
    assert_eq!(error.path(), Some("$.row_rules[0].compare.op"));
}
