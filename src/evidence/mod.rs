//! Strict, versioned evidence envelopes binding data, execution and result claims.

mod partition;

pub(crate) use partition::partition_result_digest;
pub use partition::{PartitionEvidenceV1, PartitionManifestSchema, PartitionManifestV1};

use serde::{Deserialize, Serialize};

use serde_json::Value;

use crate::{
    ContractDocument, ContractVersion, FingerprintVersion, ProofFrameError, ResourceLimits,
};

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
    pub truncated: bool,
    pub result_digest: String,
    pub report_digest: String,
    pub findings_digest: String,
    pub metrics_digest: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceV2 {
    pub schema: EvidenceSchema,
    pub dataset: DatasetEvidence,
    pub contract_source_digest: String,
    pub compiled_plan_digest: String,
    pub schema_digest: String,
    pub engine: EngineEvidence,
    pub execution: ExecutionEvidence,
    pub result: ResultEvidence,
}

/// Digest the validated, RFC 8785-canonical contract source independently from its compiled plan.
pub fn contract_source_digest(source: &str) -> Result<String, ProofFrameError> {
    let document = ContractDocument::from_json(source)?;
    let value: Value = serde_json::from_str(source)?;
    let canonical = serde_json_canonicalizer::to_vec(&value)
        .map_err(|error| ProofFrameError::InvalidContract(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    let (domain, prefix) = match document.version() {
        ContractVersion::V1 => (
            b"proofframe:contract-source:v1\0".as_slice(),
            "pf-contract-v1:",
        ),
        ContractVersion::V2 => (
            b"proofframe:contract-source:v2\0".as_slice(),
            "pf-contract-v2:",
        ),
    };
    hasher.update(domain);
    hasher.update(&canonical);
    Ok(format!("{prefix}{}", hasher.finalize().to_hex()))
}

/// Bind a canonical validation report to one versioned result identity.
pub fn validation_result_digest(report: &Value) -> Result<String, ProofFrameError> {
    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_evidence("Validation report is missing `findings`"))?;
    let summary = serde_json::json!({
        "valid": required_report_field(report, "valid")?,
        "violation_count": required_report_field(report, "violation_count")?,
        "truncated": required_report_field(report, "truncated")?,
        "output_records": findings.len(),
    });
    tagged_canonical_digest(
        "pf-result-v1:",
        b"proofframe:validation-result:v1\0",
        &summary,
    )
}

/// Bind the complete canonical validation report, including execution metrics and digests.
pub fn validation_report_digest(report: &Value) -> Result<String, ProofFrameError> {
    tagged_canonical_digest(
        "pf-report-v1:",
        b"proofframe:validation-report:v1\0",
        report,
    )
}

/// Bind the ordered retained finding records independently from summary counts.
pub fn validation_findings_digest(report: &Value) -> Result<String, ProofFrameError> {
    let findings = required_report_field(report, "findings")?;
    tagged_canonical_digest(
        "pf-findings-v1:",
        b"proofframe:validation-findings:v1\0",
        findings,
    )
}

/// Bind resource and spill measurements independently from validation findings.
pub fn validation_metrics_digest(report: &Value) -> Result<String, ProofFrameError> {
    let metrics = required_report_field(report, "metrics")?;
    tagged_canonical_digest(
        "pf-metrics-v1:",
        b"proofframe:validation-metrics:v1\0",
        metrics,
    )
}

fn tagged_canonical_digest<T: Serialize>(
    prefix: &str,
    domain: &[u8],
    value: &T,
) -> Result<String, ProofFrameError> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&canonical);
    Ok(format!("{prefix}{}", hasher.finalize().to_hex()))
}

fn required_report_field<'a>(report: &'a Value, field: &str) -> Result<&'a Value, ProofFrameError> {
    report
        .get(field)
        .ok_or_else(|| invalid_evidence(format!("Validation report is missing `{field}`")))
}

impl EvidenceV2 {
    /// Reject structurally valid envelopes whose claims are semantically inconsistent.
    pub fn validate(&self) -> Result<(), ProofFrameError> {
        if self.dataset.fingerprint_version != FingerprintVersion::V2 {
            return Err(invalid_evidence(
                "V2 evidence requires the V2 dataset fingerprint",
            ));
        }
        validate_tagged_digest_any(
            &self.contract_source_digest,
            &["pf-contract-v1:", "pf-contract-v2:"],
            "contract source",
        )?;
        validate_tagged_digest_any(
            &self.compiled_plan_digest,
            &["pf-plan-v1:", "pf-plan-v2:"],
            "compiled plan",
        )?;
        validate_tagged_digest(&self.schema_digest, "pf-schema-v1:", "schema")?;
        validate_tagged_digest(
            &self.result.result_digest,
            "pf-result-v1:",
            "validation result",
        )?;
        validate_tagged_digest(
            &self.result.report_digest,
            "pf-report-v1:",
            "validation report",
        )?;
        validate_tagged_digest(
            &self.result.findings_digest,
            "pf-findings-v1:",
            "validation findings",
        )?;
        validate_tagged_digest(
            &self.result.metrics_digest,
            "pf-metrics-v1:",
            "validation metrics",
        )?;
        if self.engine.name != "proofframe" || self.engine.version.trim().is_empty() {
            return Err(invalid_evidence(
                "Evidence engine must be `proofframe` with a non-empty version",
            ));
        }
        if self.execution.operation != "check" {
            return Err(invalid_evidence(
                "V2 evidence supports only the `check` operation",
            ));
        }
        if self.result.valid != (self.result.violation_count == 0) {
            return Err(invalid_evidence(
                "Evidence validity must equal a zero violation count",
            ));
        }
        if self.result.output_records > self.result.violation_count {
            return Err(invalid_evidence(
                "Evidence output records exceed the violation count",
            ));
        }
        let max_samples = u64::try_from(self.execution.resources.max_samples).unwrap_or(u64::MAX);
        if self.result.output_records > max_samples {
            return Err(invalid_evidence(
                "Evidence output records exceed the configured sample limit",
            ));
        }
        if self.result.truncated != (self.result.output_records < self.result.violation_count) {
            return Err(invalid_evidence(
                "Evidence truncation does not match retained output records",
            ));
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<[u8; 32], ProofFrameError> {
        self.validate()?;
        self.digest_unchecked()
    }

    pub(crate) fn digest_unchecked(&self) -> Result<[u8; 32], ProofFrameError> {
        let canonical = serde_json_canonicalizer::to_vec(self)
            .map_err(|error| ProofFrameError::InvalidReceipt(error.to_string()))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proofframe:evidence:v2\0");
        hasher.update(&canonical);
        Ok(*hasher.finalize().as_bytes())
    }
}

fn validate_tagged_digest(value: &str, prefix: &str, label: &str) -> Result<(), ProofFrameError> {
    let Some(hex) = value.strip_prefix(prefix) else {
        return Err(invalid_evidence(format!(
            "Evidence {label} digest has the wrong prefix"
        )));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_evidence(format!(
            "Evidence {label} digest must contain 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn validate_tagged_digest_any(
    value: &str,
    prefixes: &[&str],
    label: &str,
) -> Result<(), ProofFrameError> {
    if prefixes.iter().any(|prefix| {
        value.strip_prefix(prefix).is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    }) {
        Ok(())
    } else {
        Err(invalid_evidence(format!(
            "Evidence {label} digest has an unsupported prefix or malformed digest"
        )))
    }
}

fn invalid_evidence(message: impl Into<String>) -> ProofFrameError {
    ProofFrameError::InvalidReceipt(message.into())
}
