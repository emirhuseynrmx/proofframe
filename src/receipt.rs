//! Ed25519-signed proof receipts over canonical JSON reports.
//!
//! A receipt binds a report to an accountable signer using RFC 8785 JSON
//! canonicalization and a BLAKE3 report hash. Verification is fail-closed:
//! schema support, report hash, and signature must all hold.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProofFrameError;
use crate::evidence::EvidenceV2;

mod trust;
pub use trust::{TrustPolicy, TrustStore};

/// Ed25519 signing keypair encoded as URL-safe base64.
#[derive(Serialize)]
pub struct Keypair {
    /// Signature algorithm identifier (`Ed25519`).
    pub algorithm: &'static str,
    /// Base64 private signing key; store it in a secret manager.
    pub private_key: String,
    /// Base64 public verifying key.
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnsignedReceipt {
    schema: String,
    algorithm: String,
    engine_version: String,
    issued_at_unix_ms: u64,
    report_hash: String,
    report: Value,
    public_key: String,
}

#[derive(Serialize, Deserialize)]
struct SignedReceipt {
    #[serde(flatten)]
    unsigned: UnsignedReceipt,
    signature: String,
}

/// Result of verifying a signed receipt; every field must hold for `valid`.
#[derive(Serialize)]
pub struct Verification {
    /// `true` only when schema, report hash, and signature all pass.
    pub valid: bool,
    /// `true` when the Ed25519 signature verifies against the public key.
    pub signature_valid: bool,
    /// `true` when the recomputed report hash matches the receipt.
    pub report_hash_matches: bool,
    /// `true` when the receipt schema and algorithm are supported.
    pub schema_supported: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReceiptSchema {
    #[serde(rename = "proofframe.receipt.v2")]
    V2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsignedReceiptV2 {
    pub schema: ReceiptSchema,
    pub algorithm: String,
    pub engine_version: String,
    pub issued_at_unix_ms: u64,
    pub evidence_hash: [u8; 32],
    pub evidence: EvidenceV2,
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReceiptV2 {
    pub schema: ReceiptSchema,
    pub unsigned: UnsignedReceiptV2,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReceiptVerification {
    pub valid: bool,
    pub signature_valid: bool,
    pub report_hash_matches: bool,
    pub schema_supported: bool,
    pub signer_trusted: bool,
    pub legacy: bool,
}

fn canonical(value: &impl Serialize) -> Result<Vec<u8>, ProofFrameError> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))
}

fn validate_i_json(value: &Value) -> Result<(), ProofFrameError> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    let out_of_range = || {
        ProofFrameError::InvalidReceipt(
            "Receipt integers must be within the I-JSON safe range".to_string(),
        )
    };
    match value {
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                if value.unsigned_abs() > MAX_SAFE_INTEGER {
                    return Err(out_of_range());
                }
            } else if number
                .as_u64()
                .is_some_and(|value| value > MAX_SAFE_INTEGER)
            {
                return Err(out_of_range());
            }
        }
        Value::Array(values) => {
            for item in values {
                validate_i_json(item)?;
            }
        }
        Value::Object(values) => {
            for item in values.values() {
                validate_i_json(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn decode_exact<const N: usize>(encoded: &str, label: &str) -> Result<[u8; N], ProofFrameError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|error| ProofFrameError::InvalidReceipt(format!("Invalid {label}: {error}")))?;
    bytes
        .try_into()
        .map_err(|_| ProofFrameError::InvalidReceipt(format!("Invalid {label} length")))
}

fn now_unix_ms() -> Result<u64, ProofFrameError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| {
            ProofFrameError::InvalidReceipt(
                "System timestamp is outside the supported range".to_string(),
            )
        })
}

/// Generate an Ed25519 [`Keypair`] and return it as a JSON string.
pub fn generate_keypair_json() -> Result<String, ProofFrameError> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?;
    let signing = SigningKey::from_bytes(&seed);
    let output = Keypair {
        algorithm: "Ed25519",
        private_key: URL_SAFE_NO_PAD.encode(signing.to_bytes()),
        public_key: URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
    };
    Ok(serde_json::to_string(&output)?)
}

/// Sign a JSON report and return a canonical, Ed25519-signed receipt string.
///
/// Integers outside the I-JSON safe range are rejected to keep JSON number
/// canonicalization unambiguous.
pub fn sign_json(report_json: &str, private_key: &str) -> Result<String, ProofFrameError> {
    let report: Value = serde_json::from_str(report_json)?;
    validate_i_json(&report)?;
    let signing = SigningKey::from_bytes(&decode_exact(private_key, "private key")?);
    let report_hash = blake3::hash(&canonical(&report)?).to_hex().to_string();
    let issued_at_unix_ms = now_unix_ms()?;
    let unsigned = UnsignedReceipt {
        schema: "proofframe.receipt.v1".to_string(),
        algorithm: "Ed25519".to_string(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        issued_at_unix_ms,
        report_hash,
        report,
        public_key: URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
    };
    let signature = signing.sign(&canonical(&unsigned)?);
    Ok(serde_json::to_string(&SignedReceipt {
        unsigned,
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    })?)
}

/// Sign a strict V2 evidence envelope with an externally managed key.
pub fn sign_v2(
    evidence: EvidenceV2,
    signing: &SigningKey,
) -> Result<SignedReceiptV2, ProofFrameError> {
    let unsigned = UnsignedReceiptV2 {
        schema: ReceiptSchema::V2,
        algorithm: "Ed25519".to_string(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        issued_at_unix_ms: now_unix_ms()?,
        evidence_hash: evidence.digest()?,
        evidence,
        public_key: URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
    };
    let signature = signing.sign(&v2_message(&unsigned)?);
    Ok(SignedReceiptV2 {
        schema: ReceiptSchema::V2,
        unsigned,
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    })
}

/// Deserialize and sign a strict V2 evidence envelope.
pub fn sign_v2_json(evidence_json: &str, private_key: &str) -> Result<String, ProofFrameError> {
    let evidence: EvidenceV2 = serde_json::from_str(evidence_json)?;
    let signing = SigningKey::from_bytes(&decode_exact(private_key, "private key")?);
    Ok(serde_json::to_string(&sign_v2(evidence, &signing)?)?)
}

/// Verify V2 or legacy V1 JSON with an optional expected signer key.
pub fn verify_json_with_expected_key(
    receipt_json: &str,
    expected_public_key: Option<&str>,
) -> Result<ReceiptVerification, ProofFrameError> {
    let trust = match expected_public_key {
        Some(key) => TrustPolicy::ExpectedKey(decode_public_key(key)?),
        None => TrustPolicy::SignatureOnly,
    };
    verify_json_with_policy(receipt_json, &trust)
}

/// Verify cryptographic integrity independently from the caller's signer-trust policy.
pub fn verify_v2(
    receipt: &SignedReceiptV2,
    trust: &TrustPolicy,
) -> Result<ReceiptVerification, ProofFrameError> {
    let evidence_semantics_valid = receipt.unsigned.evidence.validate().is_ok();
    let schema_supported = receipt.schema == ReceiptSchema::V2
        && receipt.unsigned.schema == ReceiptSchema::V2
        && receipt.unsigned.algorithm == "Ed25519"
        && evidence_semantics_valid;
    let expected_hash = receipt.unsigned.evidence.digest_unchecked()?;
    let report_hash_matches = expected_hash == receipt.unsigned.evidence_hash;
    let public = decode_public_key(&receipt.unsigned.public_key)?;
    let signature = Signature::from_bytes(&decode_exact(&receipt.signature, "signature")?);
    let signature_valid = public
        .verify_strict(&v2_message(&receipt.unsigned)?, &signature)
        .is_ok();
    let signer_trusted = trust.accepts(&public);
    Ok(ReceiptVerification {
        valid: schema_supported && report_hash_matches && signature_valid && signer_trusted,
        signature_valid,
        report_hash_matches,
        schema_supported,
        signer_trusted,
        legacy: false,
    })
}

/// Verify either strict V2 or the isolated V1 compatibility schema.
pub fn verify_json_with_policy(
    receipt_json: &str,
    trust: &TrustPolicy,
) -> Result<ReceiptVerification, ProofFrameError> {
    let value: Value = serde_json::from_str(receipt_json)?;
    match value.get("schema").and_then(Value::as_str) {
        Some("proofframe.receipt.v2") => {
            let receipt: SignedReceiptV2 = serde_json::from_value(value)?;
            verify_v2(&receipt, trust)
        }
        Some("proofframe.receipt.v1") => {
            let receipt: SignedReceipt = serde_json::from_value(value)?;
            let public = decode_public_key(&receipt.unsigned.public_key)?;
            let verification = verify_json(receipt_json)?;
            let signer_trusted = trust.accepts(&public);
            Ok(ReceiptVerification {
                valid: verification.valid && signer_trusted,
                signature_valid: verification.signature_valid,
                report_hash_matches: verification.report_hash_matches,
                schema_supported: verification.schema_supported,
                signer_trusted,
                legacy: true,
            })
        }
        _ => Ok(ReceiptVerification {
            valid: false,
            signature_valid: false,
            report_hash_matches: false,
            schema_supported: false,
            signer_trusted: false,
            legacy: false,
        }),
    }
}

fn decode_public_key(encoded: &str) -> Result<VerifyingKey, ProofFrameError> {
    VerifyingKey::from_bytes(&decode_exact(encoded, "public key")?)
        .map_err(|error| ProofFrameError::InvalidReceipt(format!("Invalid public key: {error}")))
}

fn v2_message(unsigned: &UnsignedReceiptV2) -> Result<Vec<u8>, ProofFrameError> {
    let canonical = canonical(unsigned)?;
    let mut message = Vec::with_capacity(27 + canonical.len());
    message.extend_from_slice(b"proofframe:receipt:v2\0");
    message.extend_from_slice(&canonical);
    Ok(message)
}

/// Verify a signed receipt's schema, report hash, and Ed25519 signature.
pub fn verify_json(receipt_json: &str) -> Result<Verification, ProofFrameError> {
    let receipt: SignedReceipt = serde_json::from_str(receipt_json)?;
    validate_i_json(&receipt.unsigned.report)?;
    let schema_supported = receipt.unsigned.schema == "proofframe.receipt.v1"
        && receipt.unsigned.algorithm == "Ed25519";
    let expected_hash = blake3::hash(&canonical(&receipt.unsigned.report)?)
        .to_hex()
        .to_string();
    let report_hash_matches = expected_hash == receipt.unsigned.report_hash;
    let public =
        VerifyingKey::from_bytes(&decode_exact(&receipt.unsigned.public_key, "public key")?)
            .map_err(|error| {
                ProofFrameError::InvalidReceipt(format!("Invalid public key: {error}"))
            })?;
    let signature = Signature::from_bytes(&decode_exact(&receipt.signature, "signature")?);
    let signature_valid = public
        .verify_strict(&canonical(&receipt.unsigned)?, &signature)
        .is_ok();
    Ok(Verification {
        valid: schema_supported && report_hash_matches && signature_valid,
        signature_valid,
        report_hash_matches,
        schema_supported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn signed_receipts_verify_and_tampering_fails(n in -9_007_199_254_740_990_i64..9_007_199_254_740_990_i64) {
            let keys: Value = serde_json::from_str(&generate_keypair_json().unwrap()).unwrap();
            let private = keys["private_key"].as_str().unwrap();
            let receipt = sign_json(&format!(r#"{{"value":{n}}}"#), private).unwrap();
            prop_assert!(verify_json(&receipt).unwrap().valid);

            let mut tampered: Value = serde_json::from_str(&receipt).unwrap();
            tampered["report"]["value"] = Value::from(n.wrapping_add(1));
            prop_assert!(!verify_json(&tampered.to_string()).unwrap().valid);
        }
    }

    #[test]
    fn rejects_integers_outside_i_json_safe_range() {
        let keys: Value = serde_json::from_str(&generate_keypair_json().unwrap()).unwrap();
        let private = keys["private_key"].as_str().unwrap();
        assert!(sign_json(r#"{"value":9007199254740992}"#, private).is_err());
    }
}
