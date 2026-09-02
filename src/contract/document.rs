use serde_json::Value;

use super::{ContractAst, ContractAstV2, ContractStatus, ContractVersion, v2};
use crate::{ErrorCode, ProofFrameError};

#[derive(Debug, Clone, PartialEq)]
pub enum ContractDocument {
    V1(ContractAst),
    V2(ContractAstV2),
}

impl ContractDocument {
    pub fn from_json(source: &str) -> Result<Self, ProofFrameError> {
        let value: Value = serde_json::from_str(source).map_err(|error| {
            ProofFrameError::contract(
                ErrorCode::ContractInvalidJson,
                format!("Invalid contract JSON: {error}"),
                None,
            )
        })?;
        let version = value
            .as_object()
            .and_then(|object| object.get("version"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProofFrameError::contract(
                    ErrorCode::ContractInvalidJson,
                    "Contract version must be a string",
                    Some("$.version".to_string()),
                )
            })?;
        match version {
            "proofframe.contract.v1" => ContractAst::from_json(source).map(Self::V1),
            "proofframe.contract.v2" => v2::parse(value).map(Self::V2),
            _ => Err(ProofFrameError::contract(
                ErrorCode::ContractInvalidJson,
                format!("Unsupported contract version `{version}`"),
                Some("$.version".to_string()),
            )),
        }
    }

    #[must_use]
    pub const fn version(&self) -> ContractVersion {
        match self {
            Self::V1(_) => ContractVersion::V1,
            Self::V2(_) => ContractVersion::V2,
        }
    }

    #[must_use]
    pub const fn as_v2(&self) -> Option<&ContractAstV2> {
        match self {
            Self::V1(_) => None,
            Self::V2(contract) => Some(contract),
        }
    }

    /// Reject generated drafts before they can reach an execution plan.
    pub fn ensure_active(&self) -> Result<(), ProofFrameError> {
        if matches!(self, Self::V2(contract) if contract.status == ContractStatus::Draft) {
            return Err(ProofFrameError::contract(
                ErrorCode::ContractDraft,
                "Draft contracts cannot be executed; review the suggested rules and set `status` to `active`",
                Some("$.status".to_string()),
            ));
        }
        Ok(())
    }
}
