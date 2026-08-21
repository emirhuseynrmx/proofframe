use serde::{Deserialize, Serialize};

use super::{validate_tagged_digest, validate_tagged_digest_any};
use crate::{Fingerprint, FingerprintVersion, ProofFrameError, ResourceLimits};

const MAX_MANIFEST_JSON_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum PartitionManifestSchema {
    #[serde(rename = "proofframe.partition-manifest.v1")]
    V1,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionEvidenceV1 {
    pub index: u64,
    pub schema_digest: String,
    pub contract_source_digest: String,
    pub compiled_plan_digest: String,
    pub rows: u64,
    pub fingerprint_version: FingerprintVersion,
    pub fingerprint_digest: [u8; 32],
    pub result_digest: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionManifestV1 {
    pub schema: PartitionManifestSchema,
    pub partitions: Vec<PartitionEvidenceV1>,
    pub global_result_digest: String,
    pub resources: ResourceLimits,
    pub root_digest: String,
}

#[derive(Serialize)]
struct ManifestBody<'a> {
    schema: PartitionManifestSchema,
    partitions: &'a [PartitionEvidenceV1],
    global_result_digest: &'a str,
    resources: ResourceLimits,
}

impl PartitionManifestV1 {
    pub fn from_json(source: &str) -> Result<Self, ProofFrameError> {
        if source.len() > MAX_MANIFEST_JSON_BYTES {
            return Err(ProofFrameError::ResourceLimit {
                resource: "partition manifest JSON",
                requested: source.len() as u64,
                used: 0,
                limit: MAX_MANIFEST_JSON_BYTES as u64,
            });
        }
        let manifest: Self = serde_json::from_str(source)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn new(
        partitions: Vec<PartitionEvidenceV1>,
        global_result_digest: String,
        resources: ResourceLimits,
    ) -> Result<Self, ProofFrameError> {
        let mut manifest = Self {
            schema: PartitionManifestSchema::V1,
            partitions,
            global_result_digest,
            resources,
            root_digest: String::new(),
        };
        manifest.validate_body()?;
        manifest.root_digest = manifest.compute_root_digest()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ProofFrameError> {
        self.validate_body()?;
        validate_tagged_digest(&self.root_digest, "pf-partition-root-v1:", "partition root")?;
        if self.root_digest != self.compute_root_digest()? {
            return Err(invalid(
                "Partition manifest root digest does not match its body",
            ));
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<[u8; 32], ProofFrameError> {
        self.validate()?;
        self.digest_unchecked()
    }

    pub(crate) fn digest_unchecked(&self) -> Result<[u8; 32], ProofFrameError> {
        let computed = self.compute_root_digest()?;
        let hex = computed
            .strip_prefix("pf-partition-root-v1:")
            .expect("computed root has the required prefix");
        let mut digest = [0_u8; 32];
        for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
            digest[index] = (decode_hex(chunk[0])? << 4) | decode_hex(chunk[1])?;
        }
        Ok(digest)
    }

    fn validate_body(&self) -> Result<(), ProofFrameError> {
        if self.schema != PartitionManifestSchema::V1 {
            return Err(invalid("Unsupported partition manifest schema"));
        }
        if self.partitions.is_empty() {
            return Err(invalid(
                "Partition manifest must contain at least one partition",
            ));
        }
        validate_tagged_digest(
            &self.global_result_digest,
            "pf-result-v1:",
            "global partition result",
        )?;
        let first = &self.partitions[0];
        for (position, partition) in self.partitions.iter().enumerate() {
            if partition.index != position as u64 {
                return Err(invalid(
                    "Partition indices must be contiguous, unique, and logically ordered",
                ));
            }
            if partition.fingerprint_version != FingerprintVersion::V2 {
                return Err(invalid("Partition evidence requires V2 fingerprints"));
            }
            validate_tagged_digest(
                &partition.schema_digest,
                "pf-schema-v1:",
                "partition schema",
            )?;
            validate_tagged_digest_any(
                &partition.contract_source_digest,
                &["pf-contract-v1:", "pf-contract-v2:"],
                "partition contract source",
            )?;
            validate_tagged_digest_any(
                &partition.compiled_plan_digest,
                &["pf-plan-v1:", "pf-plan-v2:"],
                "partition compiled plan",
            )?;
            validate_tagged_digest(
                &partition.result_digest,
                "pf-partition-result-v1:",
                "partition result",
            )?;
            if partition.schema_digest != first.schema_digest
                || partition.contract_source_digest != first.contract_source_digest
                || partition.compiled_plan_digest != first.compiled_plan_digest
            {
                return Err(invalid(
                    "Every partition must bind the same schema, contract, and compiled plan",
                ));
            }
        }
        Ok(())
    }

    fn compute_root_digest(&self) -> Result<String, ProofFrameError> {
        let body = ManifestBody {
            schema: self.schema,
            partitions: &self.partitions,
            global_result_digest: &self.global_result_digest,
            resources: self.resources,
        };
        let canonical = serde_json_canonicalizer::to_vec(&body)
            .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proofframe:partition-manifest:v1\0");
        hasher.update(&canonical);
        Ok(format!(
            "pf-partition-root-v1:{}",
            hasher.finalize().to_hex()
        ))
    }
}

fn decode_hex(byte: u8) -> Result<u8, ProofFrameError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(invalid("Partition root contains invalid hexadecimal")),
    }
}

fn invalid(message: impl Into<String>) -> ProofFrameError {
    ProofFrameError::InvalidReceipt(message.into())
}

pub(crate) fn partition_result_digest(
    index: u64,
    fingerprint: &Fingerprint,
    schema_digest: &str,
    contract_source_digest: &str,
    compiled_plan_digest: &str,
) -> Result<String, ProofFrameError> {
    let value = serde_json::json!({
        "index": index,
        "rows": fingerprint.rows(),
        "fingerprint_version": fingerprint.version(),
        "fingerprint_digest": fingerprint.digest(),
        "schema_digest": schema_digest,
        "contract_source_digest": contract_source_digest,
        "compiled_plan_digest": compiled_plan_digest,
    });
    let canonical = serde_json_canonicalizer::to_vec(&value)
        .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proofframe:partition-result:v1\0");
    hasher.update(&canonical);
    Ok(format!(
        "pf-partition-result-v1:{}",
        hasher.finalize().to_hex()
    ))
}
