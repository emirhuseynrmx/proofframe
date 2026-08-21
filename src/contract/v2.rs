use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::{BoundAst, ContractVersion, NaNPolicyAst};
use crate::{ErrorCode, ProofFrameError};

const DEFAULT_MAX_FINDINGS: usize = 100;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveTypeAst {
    Boolean,
    Int8,
    Int16,
    Int32,
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Float32,
    Float64,
    Date32,
    Date64,
    Utf8,
    LargeUtf8,
    Utf8View,
    Binary,
    LargeBinary,
    BinaryView,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum ParameterizedTypeAst {
    Decimal128 {
        precision: u8,
        scale: i8,
    },
    Timestamp {
        unit: TimeUnitAst,
        #[serde(default)]
        timezone: Option<String>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TypeAst {
    Primitive(PrimitiveTypeAst),
    Parameterized(ParameterizedTypeAst),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnitAst {
    S,
    Ms,
    Us,
    Ns,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleAstV2 {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub not_null: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(rename = "type")]
    pub expected_type: Option<TypeAst>,
    pub min: Option<BoundAst>,
    pub max: Option<BoundAst>,
    pub nan: Option<NaNPolicyAst>,
    pub pattern: Option<String>,
    pub allowed: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum OperandAst {
    Column(ColumnOperandAst),
    Literal(LiteralOperandAst),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnOperandAst {
    pub column: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiteralOperandAst {
    pub literal: Value,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOpAst {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NullPolicyAst {
    #[default]
    Skip,
    Fail,
    Equal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareAst {
    pub left: OperandAst,
    pub op: CompareOpAst,
    pub right: OperandAst,
    #[serde(default)]
    pub nulls: NullPolicyAst,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionAst {
    pub column: String,
    #[serde(default)]
    pub not_null: bool,
    pub min: Option<BoundAst>,
    pub max: Option<BoundAst>,
    pub nan: Option<NaNPolicyAst>,
    pub pattern: Option<String>,
    pub allowed: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowRuleAst {
    pub name: String,
    pub compare: Option<CompareAst>,
    pub when: Option<CompareAst>,
    #[serde(rename = "assert")]
    pub assertion: Option<AssertionAst>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountRangeAst {
    pub exact: Option<u64>,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatioRangeAst {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositeNullPolicyAst {
    #[default]
    Equal,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeUniqueAst {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub nulls: CompositeNullPolicyAst,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRulesAst {
    pub row_count: Option<CountRangeAst>,
    #[serde(default)]
    pub composite_unique: Vec<CompositeUniqueAst>,
    #[serde(default)]
    pub null_ratio: BTreeMap<String, RatioRangeAst>,
    #[serde(default)]
    pub distinct_count: BTreeMap<String, CountRangeAst>,
    #[serde(default)]
    pub distinct_ratio: BTreeMap<String, RatioRangeAst>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractAstV2 {
    pub version: ContractVersion,
    #[serde(default)]
    pub columns: BTreeMap<String, RuleAstV2>,
    #[serde(default)]
    pub row_rules: Vec<RowRuleAst>,
    #[serde(default)]
    pub dataset_rules: DatasetRulesAst,
    #[serde(default = "default_max_findings")]
    pub max_findings: usize,
}

pub(crate) fn parse(value: Value) -> Result<ContractAstV2, ProofFrameError> {
    validate_known_fields(&value)?;
    let contract: ContractAstV2 = serde_json::from_value(value).map_err(|error| {
        ProofFrameError::contract(
            ErrorCode::ContractInvalidJson,
            format!("Invalid V2 contract value: {error}"),
            None,
        )
    })?;
    if contract.version != ContractVersion::V2 {
        return Err(invalid(
            "V2 contract has an incompatible version",
            "$.version",
        ));
    }
    validate_semantics(&contract)?;
    Ok(contract)
}

fn default_max_findings() -> usize {
    DEFAULT_MAX_FINDINGS
}

fn validate_semantics(contract: &ContractAstV2) -> Result<(), ProofFrameError> {
    let mut names = BTreeSet::new();
    for (index, rule) in contract.row_rules.iter().enumerate() {
        if rule.name.is_empty() || !names.insert(rule.name.as_str()) {
            return Err(ProofFrameError::contract(
                ErrorCode::ContractDuplicateRule,
                format!("Row rule name `{}` is empty or duplicated", rule.name),
                Some(format!("$.row_rules[{index}].name")),
            ));
        }
        let direct = rule.compare.is_some();
        let conditional = rule.when.is_some() && rule.assertion.is_some();
        if direct == conditional {
            return Err(invalid(
                "A row rule must contain either compare or both when and assert",
                &format!("$.row_rules[{index}]"),
            ));
        }
    }
    for (column, range) in &contract.dataset_rules.null_ratio {
        validate_ratio(range, &format!("$.dataset_rules.null_ratio.{column}"))?;
    }
    for (column, range) in &contract.dataset_rules.distinct_ratio {
        validate_ratio(range, &format!("$.dataset_rules.distinct_ratio.{column}"))?;
    }
    for (index, rule) in contract.dataset_rules.composite_unique.iter().enumerate() {
        if rule.columns.len() < 2 {
            return Err(invalid(
                "Composite uniqueness requires at least two columns",
                &format!("$.dataset_rules.composite_unique[{index}].columns"),
            ));
        }
    }
    Ok(())
}

fn validate_ratio(range: &RatioRangeAst, path: &str) -> Result<(), ProofFrameError> {
    for (name, value) in [("min", range.min), ("max", range.max)] {
        if value.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err(ProofFrameError::contract(
                ErrorCode::ContractInvalidRatio,
                "Ratio bounds must be finite values between zero and one",
                Some(format!("{path}.{name}")),
            ));
        }
    }
    if range.min.zip(range.max).is_some_and(|(min, max)| min > max) {
        return Err(ProofFrameError::contract(
            ErrorCode::ContractInvalidRatio,
            "Ratio minimum exceeds maximum",
            Some(path.to_string()),
        ));
    }
    Ok(())
}

fn invalid(message: &str, path: &str) -> ProofFrameError {
    ProofFrameError::contract(
        ErrorCode::ContractInvalidJson,
        message,
        Some(path.to_string()),
    )
}

fn validate_known_fields(value: &Value) -> Result<(), ProofFrameError> {
    let root = object(value, "$")?;
    reject_unknown(
        root,
        &[
            "columns",
            "dataset_rules",
            "max_findings",
            "row_rules",
            "version",
        ],
        "$",
    )?;
    if let Some(columns) = root.get("columns") {
        for (name, rules) in object(columns, "$.columns")? {
            reject_unknown(
                object(rules, &format!("$.columns.{name}"))?,
                &[
                    "allowed", "max", "min", "nan", "not_null", "pattern", "required", "type",
                    "unique",
                ],
                &format!("$.columns.{name}"),
            )?;
        }
    }
    if let Some(row_rules) = root.get("row_rules") {
        let rules = row_rules
            .as_array()
            .ok_or_else(|| invalid("row_rules must be an array", "$.row_rules"))?;
        for (index, rule) in rules.iter().enumerate() {
            let path = format!("$.row_rules[{index}]");
            let rule = object(rule, &path)?;
            reject_unknown(rule, &["assert", "compare", "name", "when"], &path)?;
            for field in ["compare", "when"] {
                if let Some(compare) = rule.get(field) {
                    reject_unknown(
                        object(compare, &format!("{path}.{field}"))?,
                        &["left", "nulls", "op", "right"],
                        &format!("{path}.{field}"),
                    )?;
                }
            }
            if let Some(assertion) = rule.get("assert") {
                reject_unknown(
                    object(assertion, &format!("{path}.assert"))?,
                    &[
                        "allowed", "column", "max", "min", "nan", "not_null", "pattern",
                    ],
                    &format!("{path}.assert"),
                )?;
            }
        }
    }
    Ok(())
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, ProofFrameError> {
    value
        .as_object()
        .ok_or_else(|| invalid("Expected a JSON object", path))
}

fn reject_unknown(
    object: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> Result<(), ProofFrameError> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(ProofFrameError::contract(
            ErrorCode::ContractUnknownField,
            format!("Unknown contract field `{field}`"),
            Some(format!("{path}.{field}")),
        ));
    }
    Ok(())
}
