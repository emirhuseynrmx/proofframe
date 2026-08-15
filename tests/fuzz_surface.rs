#![cfg(feature = "fuzzing")]

use proofframe::{ErrorCode, fuzz_partition_bytes};

#[test]
fn arbitrary_partition_bytes_fail_closed_without_panicking() {
    let error = fuzz_partition_bytes(b"not a partition").expect_err("garbage must be rejected");
    assert!(matches!(
        error.code(),
        ErrorCode::Io | ErrorCode::CorruptPartition
    ));
}

#[test]
fn partition_fuzz_surface_caps_input_before_copying() {
    let oversized = vec![0_u8; (1024 * 1024) + 1];
    let error = fuzz_partition_bytes(&oversized).expect_err("oversized input must be rejected");
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
}
