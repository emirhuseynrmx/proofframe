use proofframe::ContractDocument;

#[test]
fn invalid_type_names_identify_column_and_teach_native_spelling() {
    let error = ContractDocument::from_json(
        r#"{"version":"proofframe.contract.v2","columns":{"customer":{"type":"string"}}}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("customer") && error.contains("utf8"),
        "{error}"
    );
    assert!(!error.contains("untagged enum"), "{error}");
}

#[test]
fn parameterized_type_explains_missing_required_parameter() {
    let error = ContractDocument::from_json(r#"{"version":"proofframe.contract.v2","columns":{"price":{"type":{"name":"decimal128","scale":2}}}}"#).unwrap_err().to_string();
    assert!(
        error.contains("price") && error.contains("precision"),
        "{error}"
    );
}

#[test]
fn version_error_shows_the_actual_supported_envelope() {
    let error = ContractDocument::from_json(r#"{"version":2,"columns":{}}"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("proofframe.contract.v2"), "{error}");
}

#[test]
fn status_on_v1_points_to_v2_instead_of_only_blaming_status() {
    let error = ContractDocument::from_json(
        r#"{"version":"proofframe.contract.v1","status":"active","columns":{}}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("version") && error.contains("proofframe.contract.v2"),
        "{error}"
    );
}
