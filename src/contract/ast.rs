use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{ErrorCode, ProofFrameError};

const DEFAULT_MAX_FINDINGS: usize = 100;
const ROOT_FIELDS: &[&str] = &["columns", "max_findings", "version"];
const RULE_FIELDS: &[&str] = &[
    "allowed", "max", "min", "nan", "not_null", "pattern", "required", "unique",
];

/// Version of the serialized contract language.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
pub enum ContractVersion {
    /// Initial strict contract language used by ProofFrame 0.5.
    #[serde(rename = "proofframe.contract.v1")]
    V1,
}

/// Source-level floating-point NaN policy.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NaNPolicyAst {
    /// Treat NaN as a validation failure when numeric rules inspect the value.
    #[default]
    Reject,
    /// Permit NaN while applying numeric bounds only to ordered values.
    Allow,
}

/// Exact source literal for a numeric, decimal, or timestamp bound.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum BoundAst {
    /// A JSON number, preserved by `serde_json::Number` without conversion to `f64`.
    Number(serde_json::Number),
    /// A decimal string, signed timestamp ticks, or an offset-qualified ISO-8601 timestamp.
    Text(String),
}

impl BoundAst {
    /// Return the exact source spelling used for semantic type conversion.
    #[must_use]
    pub fn as_text(&self) -> &str {
        match self {
            Self::Number(number) => number.as_str(),
            Self::Text(text) => text,
        }
    }
}

/// Syntax-level rule set for one named column.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleAst {
    /// Require the column to exist.
    #[serde(default)]
    pub required: bool,
    /// Reject null cells.
    #[serde(default)]
    pub not_null: bool,
    /// Require exact uniqueness.
    #[serde(default)]
    pub unique: bool,
    /// Inclusive lower bound.
    pub min: Option<BoundAst>,
    /// Inclusive upper bound.
    pub max: Option<BoundAst>,
    /// NaN handling for floating-point columns.
    pub nan: Option<NaNPolicyAst>,
    /// Regular expression applied to textual values.
    pub pattern: Option<String>,
    /// Exact textual allowlist.
    pub allowed: Option<BTreeSet<String>>,
}

/// Strict, versioned syntax tree parsed from a ProofFrame contract document.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractAst {
    /// Contract language version.
    pub version: ContractVersion,
    /// Column rules keyed by logical column name.
    #[serde(default)]
    pub columns: BTreeMap<String, RuleAst>,
    /// Maximum number of row-level findings retained in memory.
    #[serde(default = "default_max_findings")]
    pub max_findings: usize,
}

impl ContractAst {
    /// Parse a contract without silently accepting unknown fields.
    pub fn from_json(source: &str) -> Result<Self, ProofFrameError> {
        let value: Value = serde_json::from_str(source).map_err(|error| {
            ProofFrameError::contract(
                ErrorCode::ContractInvalidJson,
                format!("Invalid contract JSON: {error}"),
                None,
            )
        })?;

        validate_known_fields(&value)?;
        serde_json::from_value(value).map_err(|error| {
            ProofFrameError::contract(
                ErrorCode::ContractInvalidJson,
                format!("Invalid contract value: {error}"),
                None,
            )
        })
    }
}

fn default_max_findings() -> usize {
    DEFAULT_MAX_FINDINGS
}

fn validate_known_fields(value: &Value) -> Result<(), ProofFrameError> {
    let root = value.as_object().ok_or_else(|| {
        ProofFrameError::contract(
            ErrorCode::ContractInvalidJson,
            "A contract document must be a JSON object",
            None,
        )
    })?;
    reject_unknown(root, ROOT_FIELDS, "$".to_string())?;

    let Some(columns) = root.get("columns") else {
        return Ok(());
    };
    let columns = columns.as_object().ok_or_else(|| {
        ProofFrameError::contract(
            ErrorCode::ContractInvalidJson,
            "Contract field `columns` must be a JSON object",
            Some("$.columns".to_string()),
        )
    })?;

    for (column, rule) in columns {
        let column_path = append_path("$.columns", column);
        let rule = rule.as_object().ok_or_else(|| {
            ProofFrameError::contract(
                ErrorCode::ContractInvalidJson,
                format!("Rules for column `{column}` must be a JSON object"),
                Some(column_path.clone()),
            )
        })?;
        reject_unknown(rule, RULE_FIELDS, column_path)?;
    }
    Ok(())
}

fn reject_unknown(
    object: &Map<String, Value>,
    allowed: &[&str],
    parent_path: String,
) -> Result<(), ProofFrameError> {
    if let Some(field) = object.keys().find(|field| {
        allowed
            .binary_search_by(|candidate| candidate.cmp(&field.as_str()))
            .is_err()
    }) {
        return Err(ProofFrameError::contract(
            ErrorCode::ContractUnknownField,
            format!("Unknown contract field `{field}`"),
            Some(append_path(&parent_path, field)),
        ));
    }
    Ok(())
}

pub(crate) fn column_path(column: &str) -> String {
    append_path("$.columns", column)
}

pub(crate) fn append_path(parent: &str, segment: &str) -> String {
    if is_identifier(segment) {
        format!("{parent}.{segment}")
    } else {
        let encoded = serde_json::to_string(segment).expect("serializing a string cannot fail");
        format!("{parent}[{encoded}]")
    }
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
