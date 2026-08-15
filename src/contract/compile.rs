use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use ahash::RandomState;
use arrow::datatypes::{DataType, FieldRef, Schema, TimeUnit};
use regex::Regex;

use super::ast::{append_path, column_path};
use super::bounds::parse_bound;
use super::{ContractAst, NaNPolicyAst, RuleAst, TypedBound};
use crate::{ErrorCode, ProofFrameError};

/// Arrow-specialized kernel selected once during contract compilation.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum KernelKind {
    Boolean,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Date32,
    Date64,
    Decimal128 { precision: u8, scale: i8 },
    Timestamp(TimeUnit),
    Utf8,
    LargeUtf8,
    Binary,
    LargeBinary,
    Nested,
    NullOnly,
}

impl KernelKind {
    fn from_data_type(data_type: &DataType) -> Self {
        match data_type {
            DataType::Boolean => Self::Boolean,
            DataType::Int8 => Self::I8,
            DataType::Int16 => Self::I16,
            DataType::Int32 => Self::I32,
            DataType::Int64 => Self::I64,
            DataType::UInt8 => Self::U8,
            DataType::UInt16 => Self::U16,
            DataType::UInt32 => Self::U32,
            DataType::UInt64 => Self::U64,
            DataType::Float32 => Self::F32,
            DataType::Float64 => Self::F64,
            DataType::Date32 => Self::Date32,
            DataType::Date64 => Self::Date64,
            DataType::Decimal128(precision, scale) => Self::Decimal128 {
                precision: *precision,
                scale: *scale,
            },
            DataType::Timestamp(unit, _) => Self::Timestamp(*unit),
            DataType::Utf8 => Self::Utf8,
            DataType::LargeUtf8 => Self::LargeUtf8,
            DataType::Binary => Self::Binary,
            DataType::LargeBinary => Self::LargeBinary,
            DataType::List(_)
            | DataType::LargeList(_)
            | DataType::FixedSizeList(_, _)
            | DataType::Struct(_)
            | DataType::Map(_, _) => Self::Nested,
            _ => Self::NullOnly,
        }
    }

    fn supports_bounds(&self) -> bool {
        matches!(
            self,
            Self::I8
                | Self::I16
                | Self::I32
                | Self::I64
                | Self::U8
                | Self::U16
                | Self::U32
                | Self::U64
                | Self::F32
                | Self::F64
                | Self::Date32
                | Self::Date64
                | Self::Decimal128 { .. }
                | Self::Timestamp(_)
        )
    }

    fn supports_text_rules(&self) -> bool {
        matches!(self, Self::Utf8 | Self::LargeUtf8)
    }

    fn supports_unique(&self) -> bool {
        !matches!(self, Self::NullOnly)
    }
}

/// Runtime NaN behavior compiled for floating-point kernels.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NaNPolicy {
    Reject,
    Allow,
}

impl From<Option<NaNPolicyAst>> for NaNPolicy {
    fn from(value: Option<NaNPolicyAst>) -> Self {
        match value.unwrap_or_default() {
            NaNPolicyAst::Reject => Self::Reject,
            NaNPolicyAst::Allow => Self::Allow,
        }
    }
}

/// Semantically checked rules ready for typed execution.
#[derive(Debug, Clone)]
pub struct CompiledRules {
    pub(crate) required: bool,
    pub(crate) not_null: bool,
    pub(crate) unique: bool,
    pub(crate) min: Option<TypedBound>,
    pub(crate) max: Option<TypedBound>,
    pub(crate) nan: NaNPolicy,
    pub(crate) pattern: Option<Regex>,
    pub(crate) allowed: Option<Arc<HashSet<Box<str>, RandomState>>>,
}

impl CompiledRules {
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    #[must_use]
    pub const fn not_null(&self) -> bool {
        self.not_null
    }

    #[must_use]
    pub const fn unique(&self) -> bool {
        self.unique
    }

    #[must_use]
    pub fn min(&self) -> Option<&TypedBound> {
        self.min.as_ref()
    }

    #[must_use]
    pub fn max(&self) -> Option<&TypedBound> {
        self.max.as_ref()
    }

    #[must_use]
    pub const fn nan(&self) -> NaNPolicy {
        self.nan
    }

    #[must_use]
    pub fn pattern(&self) -> Option<&Regex> {
        self.pattern.as_ref()
    }

    #[must_use]
    pub fn allowed(&self) -> Option<&HashSet<Box<str>, RandomState>> {
        self.allowed.as_deref()
    }
}

/// One schema-resolved column in the execution intermediate representation.
#[derive(Debug, Clone)]
pub struct ColumnPlan {
    column_index: usize,
    field: FieldRef,
    kernel: KernelKind,
    rules: CompiledRules,
}

impl ColumnPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }

    #[must_use]
    pub fn field(&self) -> &FieldRef {
        &self.field
    }

    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }

    #[must_use]
    pub const fn rules(&self) -> &CompiledRules {
        &self.rules
    }
}

/// Contract execution IR compiled in Arrow schema order.
#[derive(Debug, Clone)]
pub struct CompiledContract {
    columns: Vec<ColumnPlan>,
    missing_required: Vec<String>,
    max_findings: usize,
}

impl CompiledContract {
    /// Resolve and type-check a syntax tree before any record batch is scanned.
    pub fn compile(contract: &ContractAst, schema: &Schema) -> Result<Self, ProofFrameError> {
        let mut missing_required = Vec::new();
        for (name, rules) in &contract.columns {
            if schema.index_of(name).is_ok() {
                continue;
            }
            if rules.required {
                missing_required.push(name.clone());
            } else if has_value_rules(rules) {
                return Err(ProofFrameError::contract(
                    ErrorCode::MissingColumn,
                    format!("Contract column `{name}` is absent from the Arrow schema"),
                    Some(column_path(name)),
                ));
            }
        }

        let mut columns = Vec::with_capacity(contract.columns.len());
        for (column_index, field) in schema.fields().iter().enumerate() {
            let Some(source_rules) = contract.columns.get(field.name()) else {
                continue;
            };
            if !has_runtime_rules(source_rules) {
                continue;
            }
            let kernel = KernelKind::from_data_type(field.data_type());
            let rules = compile_rules(field.name(), field.data_type(), &kernel, source_rules)?;
            columns.push(ColumnPlan {
                column_index,
                field: field.clone(),
                kernel,
                rules,
            });
        }

        Ok(Self {
            columns,
            missing_required,
            max_findings: contract.max_findings,
        })
    }

    #[must_use]
    pub fn columns(&self) -> &[ColumnPlan] {
        &self.columns
    }

    #[must_use]
    pub fn missing_required(&self) -> &[String] {
        &self.missing_required
    }

    #[must_use]
    pub const fn max_findings(&self) -> usize {
        self.max_findings
    }
}

fn has_value_rules(rules: &RuleAst) -> bool {
    rules.not_null
        || rules.unique
        || rules.min.is_some()
        || rules.max.is_some()
        || rules.nan.is_some()
        || rules.pattern.is_some()
        || rules.allowed.is_some()
}

fn has_runtime_rules(rules: &RuleAst) -> bool {
    has_value_rules(rules)
}

fn compile_rules(
    column: &str,
    data_type: &DataType,
    kernel: &KernelKind,
    source: &RuleAst,
) -> Result<CompiledRules, ProofFrameError> {
    let base_path = column_path(column);
    if (source.min.is_some() || source.max.is_some()) && !kernel.supports_bounds() {
        let field = if source.min.is_some() { "min" } else { "max" };
        return Err(type_mismatch(
            column,
            data_type,
            field,
            "numeric, decimal, date, or timestamp",
        ));
    }
    if source.pattern.is_some() && !kernel.supports_text_rules() {
        return Err(type_mismatch(column, data_type, "pattern", "UTF-8"));
    }
    if source.allowed.is_some() && !kernel.supports_text_rules() {
        return Err(type_mismatch(column, data_type, "allowed", "UTF-8"));
    }
    if source.nan.is_some() && !matches!(kernel, KernelKind::F32 | KernelKind::F64) {
        return Err(type_mismatch(column, data_type, "nan", "floating point"));
    }
    if source.unique && !kernel.supports_unique() {
        return Err(type_mismatch(
            column,
            data_type,
            "unique",
            "canonically encodable",
        ));
    }

    let min_path = append_path(&base_path, "min");
    let max_path = append_path(&base_path, "max");
    let min = source
        .min
        .as_ref()
        .map(|bound| parse_bound(bound, data_type, &min_path))
        .transpose()?;
    let max = source
        .max
        .as_ref()
        .map(|bound| parse_bound(bound, data_type, &max_path))
        .transpose()?;
    if min
        .as_ref()
        .zip(max.as_ref())
        .is_some_and(|(minimum, maximum)| minimum.compare(maximum) == Some(Ordering::Greater))
    {
        return Err(ProofFrameError::contract(
            ErrorCode::ContractInvalidBound,
            format!("Contract minimum exceeds maximum for column `{column}`"),
            Some(base_path.clone()),
        ));
    }

    let pattern = source
        .pattern
        .as_ref()
        .map(|pattern| {
            Regex::new(pattern).map_err(|error| {
                ProofFrameError::contract(
                    ErrorCode::ContractInvalidBound,
                    format!("Invalid regular expression for column `{column}`: {error}"),
                    Some(append_path(&base_path, "pattern")),
                )
            })
        })
        .transpose()?;
    let allowed = source.allowed.as_ref().map(|values| {
        let mut compiled = HashSet::with_capacity_and_hasher(values.len(), RandomState::new());
        compiled.extend(values.iter().map(|value| value.clone().into_boxed_str()));
        Arc::new(compiled)
    });

    Ok(CompiledRules {
        required: source.required,
        not_null: source.not_null,
        unique: source.unique,
        min,
        max,
        nan: source.nan.into(),
        pattern,
        allowed,
    })
}

fn type_mismatch(
    column: &str,
    data_type: &DataType,
    rule: &str,
    expected: &str,
) -> ProofFrameError {
    let path = append_path(&column_path(column), rule);
    ProofFrameError::contract(
        ErrorCode::ContractTypeMismatch,
        format!(
            "Rule `{rule}` on column `{column}` requires {expected} values, found `{data_type}`"
        ),
        Some(path),
    )
}
