use proofframe::{ContractAst, ErrorCode};

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
