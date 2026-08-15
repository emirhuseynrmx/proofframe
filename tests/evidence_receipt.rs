use ed25519_dalek::SigningKey;
use proofframe::evidence::{
    DatasetEvidence, EngineEvidence, EvidenceSchema, EvidenceV2, ExecutionEvidence, ResultEvidence,
};
use proofframe::receipt::{
    TrustPolicy, generate_keypair_json, sign_json, sign_v2, verify_json_with_policy, verify_v2,
};
use proofframe::{FingerprintVersion, ResourceLimits};

fn fixture() -> EvidenceV2 {
    EvidenceV2 {
        schema: EvidenceSchema::V2,
        dataset: DatasetEvidence {
            fingerprint_version: FingerprintVersion::V2,
            fingerprint_digest: [3; 32],
            rows: 42,
        },
        contract_digest: [5; 32],
        engine: EngineEvidence {
            name: "proofframe".to_string(),
            version: "0.5.0".to_string(),
        },
        execution: ExecutionEvidence {
            operation: "validate".to_string(),
            resources: ResourceLimits::default(),
        },
        result: ResultEvidence {
            valid: true,
            violation_count: 0,
            output_records: 0,
        },
    }
}

#[test]
fn evidence_digest_binds_every_claimed_execution_dimension() {
    let base = fixture();
    let base_digest = base.digest().unwrap();
    let mut variants = Vec::new();

    let mut changed = base.clone();
    changed.dataset.fingerprint_version = FingerprintVersion::V1;
    variants.push(changed);
    let mut changed = base.clone();
    changed.contract_digest = [7; 32];
    variants.push(changed);
    let mut changed = base.clone();
    changed.execution.resources.max_memory_bytes = 64 * 1024 * 1024;
    variants.push(changed);
    let mut changed = base.clone();
    changed.result.violation_count = 2;
    variants.push(changed);
    let mut changed = base.clone();
    changed.engine.version = "0.5.1".to_string();
    variants.push(changed);

    for changed in variants {
        assert_ne!(base_digest, changed.digest().unwrap());
    }
}

#[test]
fn evidence_and_receipt_schemas_reject_unknown_fields() {
    let mut unknown = serde_json::to_value(fixture()).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unknown".into(), 1.into());
    assert!(serde_json::from_value::<EvidenceV2>(unknown).is_err());

    let mut nested = serde_json::to_value(fixture()).unwrap();
    nested["engine"]["build_host"] = "untrusted".into();
    assert!(serde_json::from_value::<EvidenceV2>(nested).is_err());
}

#[test]
fn signature_validity_and_signer_trust_are_separate_results() {
    let key_a = SigningKey::from_bytes(&[11; 32]);
    let key_b = SigningKey::from_bytes(&[12; 32]);
    let signed = sign_v2(fixture(), &key_a).unwrap();

    let accepted = verify_v2(&signed, &TrustPolicy::ExpectedKey(key_a.verifying_key())).unwrap();
    assert!(accepted.signature_valid);
    assert!(accepted.report_hash_matches);
    assert!(accepted.signer_trusted);
    assert!(accepted.valid);
    assert!(!accepted.legacy);

    let rejected = verify_v2(&signed, &TrustPolicy::ExpectedKey(key_b.verifying_key())).unwrap();
    assert!(rejected.signature_valid);
    assert!(!rejected.signer_trusted);
    assert!(!rejected.valid);
}

#[test]
fn tampering_breaks_both_the_evidence_hash_and_signature() {
    let key = SigningKey::from_bytes(&[21; 32]);
    let mut signed = sign_v2(fixture(), &key).unwrap();
    signed.unsigned.evidence.result.violation_count = 9;

    let verification = verify_v2(&signed, &TrustPolicy::SignatureOnly).unwrap();
    assert!(!verification.report_hash_matches);
    assert!(!verification.signature_valid);
    assert!(!verification.valid);
}

#[test]
fn legacy_v1_receipts_remain_explicitly_readable() {
    let keys: serde_json::Value = serde_json::from_str(&generate_keypair_json().unwrap()).unwrap();
    let receipt = sign_json(
        r#"{"valid":true,"rows":3}"#,
        keys["private_key"].as_str().unwrap(),
    )
    .unwrap();

    let verification = verify_json_with_policy(&receipt, &TrustPolicy::SignatureOnly).unwrap();

    assert!(verification.valid);
    assert!(verification.legacy);
    assert!(verification.signer_trusted);
}
