use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
pub struct Keypair {
    pub algorithm: &'static str,
    pub private_key: String,
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

#[derive(Serialize)]
pub struct Verification {
    pub valid: bool,
    pub signature_valid: bool,
    pub report_hash_matches: bool,
    pub schema_supported: bool,
}

fn canonical(value: &impl Serialize) -> Result<Vec<u8>, String> {
    serde_json_canonicalizer::to_vec(value).map_err(|error| error.to_string())
}

fn validate_i_json(value: &Value) -> Result<(), String> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    match value {
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                if value.unsigned_abs() > MAX_SAFE_INTEGER {
                    return Err("Receipt integers must be within the I-JSON safe range".to_string());
                }
            } else if number
                .as_u64()
                .is_some_and(|value| value > MAX_SAFE_INTEGER)
            {
                return Err("Receipt integers must be within the I-JSON safe range".to_string());
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

fn decode_exact<const N: usize>(encoded: &str, label: &str) -> Result<[u8; N], String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|error| format!("Invalid {label}: {error}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("Invalid {label} length"))
}

pub fn generate_keypair_json() -> Result<String, String> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|error| error.to_string())?;
    let signing = SigningKey::from_bytes(&seed);
    let output = Keypair {
        algorithm: "Ed25519",
        private_key: URL_SAFE_NO_PAD.encode(signing.to_bytes()),
        public_key: URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
    };
    serde_json::to_string(&output).map_err(|error| error.to_string())
}

pub fn sign_json(report_json: &str, private_key: &str) -> Result<String, String> {
    let report: Value = serde_json::from_str(report_json).map_err(|error| error.to_string())?;
    validate_i_json(&report)?;
    let signing = SigningKey::from_bytes(&decode_exact(private_key, "private key")?);
    let report_hash = blake3::hash(&canonical(&report)?).to_hex().to_string();
    let issued_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis()
        .try_into()
        .map_err(|_| "System timestamp is outside the supported range".to_string())?;
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
    serde_json::to_string(&SignedReceipt {
        unsigned,
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    })
    .map_err(|error| error.to_string())
}

pub fn verify_json(receipt_json: &str) -> Result<Verification, String> {
    let receipt: SignedReceipt =
        serde_json::from_str(receipt_json).map_err(|error| error.to_string())?;
    validate_i_json(&receipt.unsigned.report)?;
    let schema_supported = receipt.unsigned.schema == "proofframe.receipt.v1"
        && receipt.unsigned.algorithm == "Ed25519";
    let expected_hash = blake3::hash(&canonical(&receipt.unsigned.report)?)
        .to_hex()
        .to_string();
    let report_hash_matches = expected_hash == receipt.unsigned.report_hash;
    let public =
        VerifyingKey::from_bytes(&decode_exact(&receipt.unsigned.public_key, "public key")?)
            .map_err(|error| format!("Invalid public key: {error}"))?;
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
