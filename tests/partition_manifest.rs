use ed25519_dalek::SigningKey;
use proofframe::evidence::{PartitionEvidenceV1, PartitionManifestV1};
use proofframe::receipt::{
    TrustPolicy, sign_partition_manifest, verify_partition_manifest_receipt,
};
use proofframe::{ErrorCode, FingerprintVersion, ResourceLimits};

fn digest(prefix: &str, byte: char) -> String {
    format!("{prefix}{}", byte.to_string().repeat(64))
}

fn partition(index: u64, fingerprint_byte: u8) -> PartitionEvidenceV1 {
    PartitionEvidenceV1 {
        index,
        schema_digest: digest("pf-schema-v1:", '1'),
        contract_source_digest: digest("pf-contract-v2:", '2'),
        compiled_plan_digest: digest("pf-plan-v2:", '3'),
        rows: 10,
        fingerprint_version: FingerprintVersion::V2,
        fingerprint_digest: [fingerprint_byte; 32],
        result_digest: digest("pf-partition-result-v1:", '4'),
    }
}

#[test]
fn manifest_root_binds_partition_order_and_omission() {
    let manifest = PartitionManifestV1::new(
        vec![partition(0, 7), partition(1, 8)],
        digest("pf-result-v1:", '5'),
        ResourceLimits::default(),
    )
    .unwrap();

    manifest.validate().unwrap();
    assert!(manifest.root_digest.starts_with("pf-partition-root-v1:"));

    let mut reordered = manifest.clone();
    reordered.partitions.swap(0, 1);
    assert_eq!(
        reordered.validate().unwrap_err().code(),
        ErrorCode::ReceiptInvalid
    );

    let mut omitted = manifest.clone();
    omitted.partitions.pop();
    assert_eq!(
        omitted.validate().unwrap_err().code(),
        ErrorCode::ReceiptInvalid
    );
}

#[test]
fn manifest_rejects_duplicate_indices_and_mixed_contract_identity() {
    let mut duplicate = partition(1, 9);
    duplicate.compiled_plan_digest = digest("pf-plan-v2:", '6');
    let error = PartitionManifestV1::new(
        vec![partition(0, 7), duplicate],
        digest("pf-result-v1:", '5'),
        ResourceLimits::default(),
    )
    .expect_err("all partition evidence must bind one compiled plan");

    assert_eq!(error.code(), ErrorCode::ReceiptInvalid);

    let error = PartitionManifestV1::new(
        vec![partition(0, 7), partition(0, 8)],
        digest("pf-result-v1:", '5'),
        ResourceLimits::default(),
    )
    .expect_err("partition indices must be contiguous and unique");

    assert_eq!(error.code(), ErrorCode::ReceiptInvalid);
}

#[test]
fn signed_partition_manifest_rejects_body_tampering() {
    let manifest = PartitionManifestV1::new(
        vec![partition(0, 7), partition(1, 8)],
        digest("pf-result-v1:", '5'),
        ResourceLimits::default(),
    )
    .unwrap();
    let signing = SigningKey::from_bytes(&[42_u8; 32]);
    let receipt = sign_partition_manifest(manifest, &signing).unwrap();

    let verified =
        verify_partition_manifest_receipt(&receipt, &TrustPolicy::SignatureOnly).unwrap();
    assert!(verified.valid);

    let mut tampered = receipt;
    tampered.unsigned.manifest.partitions[0].rows += 1;
    let verification =
        verify_partition_manifest_receipt(&tampered, &TrustPolicy::SignatureOnly).unwrap();
    assert!(!verification.valid);
    assert!(!verification.report_hash_matches);
}

#[test]
fn manifest_json_decoder_is_strict_and_input_bounded() {
    let manifest = PartitionManifestV1::new(
        vec![partition(0, 7)],
        digest("pf-result-v1:", '5'),
        ResourceLimits::default(),
    )
    .unwrap();
    let encoded = serde_json::to_string(&manifest).unwrap();

    assert_eq!(PartitionManifestV1::from_json(&encoded).unwrap(), manifest);

    let oversized = " ".repeat(1024 * 1024 + 1);
    assert_eq!(
        PartitionManifestV1::from_json(&oversized)
            .unwrap_err()
            .code(),
        ErrorCode::ResourceLimit
    );
}
