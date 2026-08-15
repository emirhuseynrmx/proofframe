//! Strict, versioned evidence envelopes binding data, execution and result claims.

use serde::{Deserialize, Serialize};

use crate::{FingerprintVersion, ProofFrameError, ResourceLimits};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum EvidenceSchema {
    #[serde(rename = "proofframe.evidence.v2")]
    V2,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetEvidence {
    pub fingerprint_version: FingerprintVersion,
    pub fingerprint_digest: [u8; 32],
    pub rows: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineEvidence {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionEvidence {
    pub operation: String,
    pub resources: ResourceLimits,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultEvidence {
    pub valid: bool,
    pub violation_count: u64,
    pub output_records: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceV2 {
    pub schema: EvidenceSchema,
    pub dataset: DatasetEvidence,
    pub contract_digest: [u8; 32],
    pub engine: EngineEvidence,
    pub execution: ExecutionEvidence,
    pub result: ResultEvidence,
}

impl EvidenceV2 {
    pub fn digest(&self) -> Result<[u8; 32], ProofFrameError> {
        let canonical = serde_json_canonicalizer::to_vec(self)
            .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proofframe:evidence:v2\0");
        hasher.update(&canonical);
        Ok(*hasher.finalize().as_bytes())
    }
}
