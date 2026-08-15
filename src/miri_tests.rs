use crate::{ContractAst, ErrorCode, ResourceAccount, ResourceLimits};

#[test]
fn strict_contract_parser_rejects_unknown_fields() {
    let error = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{},"mystery":true}"#,
    )
    .expect_err("unknown fields must fail closed");
    assert_eq!(error.code(), ErrorCode::ContractUnknownField);
}

#[test]
fn hierarchical_reservations_roll_back_after_child_failure() {
    let root = ResourceAccount::root(ResourceLimits {
        max_memory_bytes: 16,
        max_temp_bytes: 16,
        max_output_records: 1,
        max_samples: 1,
    });
    let child = root.child(8, 8);
    let reservation = child
        .try_reserve_memory(8)
        .expect("reservation at the exact cap succeeds");
    assert!(child.try_reserve_memory(1).is_err());
    assert_eq!(root.memory_used(), 8);
    assert_eq!(child.memory_used(), 8);
    drop(reservation);
    assert_eq!(root.memory_used(), 0);
    assert_eq!(child.memory_used(), 0);
}
