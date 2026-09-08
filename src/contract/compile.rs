use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use ahash::RandomState;
use arrow::datatypes::{DataType, FieldRef, Schema, TimeUnit};
use regex::Regex;

use super::ast::{append_path, column_path};
use super::bounds::parse_bound;
use super::{
    CompareOpAst, ComparePlan, CompositeNullPolicyAst, ConditionalUniquePlan, ContractAst,
    ContractAstV2, ContractDocument, ContractVersion, CountRangeAst, DatasetPlan, ExclusiveModeAst,
    GapDetectionPlan, MonotonicDirectionAst, MonotonicNullPolicyAst, MonotonicityPlan,
    MutuallyExclusivePlan, NaNPolicyAst, NullPolicyAst, OperandPlan, ParameterizedTypeAst,
    PrimitiveTypeAst, ReferenceNullPolicyAst, ReferencePlan, RowCountDeltaPlan, RowPlan,
    RowPlanKind, RuleAst, RuleAstV2, ScalarValuePlan, StatisticKind, StatisticPlan, SumBounds,
    SumPlan, TimeUnitAst, TypeAst, TypedBound,
};
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
    Utf8View,
    Binary,
    LargeBinary,
    BinaryView,
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
            DataType::Utf8View => Self::Utf8View,
            DataType::Binary => Self::Binary,
            DataType::LargeBinary => Self::LargeBinary,
            DataType::BinaryView => Self::BinaryView,
            DataType::List(_)
            | DataType::LargeList(_)
            | DataType::FixedSizeList(_, _)
            | DataType::Struct(_)
            | DataType::Map(_, _) => Self::Nested,
            _ => Self::NullOnly,
        }
    }

    pub(crate) fn from_data_type_for_plan(data_type: &DataType) -> Self {
        Self::from_data_type(data_type)
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
        matches!(self, Self::Utf8 | Self::LargeUtf8 | Self::Utf8View)
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
    pub(crate) validate_nan: bool,
    pub(crate) pattern: Option<Regex>,
    pub(crate) allowed: Option<Arc<HashSet<Box<str>, RandomState>>>,
    pub(crate) min_length: Option<usize>,
    pub(crate) max_length: Option<usize>,
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
    pub const fn validates_nan(&self) -> bool {
        self.validate_nan
    }

    #[must_use]
    pub fn pattern(&self) -> Option<&Regex> {
        self.pattern.as_ref()
    }

    #[must_use]
    pub fn allowed(&self) -> Option<&HashSet<Box<str>, RandomState>> {
        self.allowed.as_deref()
    }

    #[must_use]
    pub const fn min_length(&self) -> Option<usize> {
        self.min_length
    }

    #[must_use]
    pub const fn max_length(&self) -> Option<usize> {
        self.max_length
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
    schema: Schema,
    columns: Vec<ColumnPlan>,
    row_plans: Vec<RowPlan>,
    dataset_plan: DatasetPlan,
    version: ContractVersion,
    max_findings: usize,
}

impl CompiledContract {
    /// Resolve and type-check a syntax tree before any record batch is scanned.
    pub fn compile(contract: &ContractAst, schema: &Schema) -> Result<Self, ProofFrameError> {
        for (name, rules) in &contract.columns {
            if schema.index_of(name).is_ok() {
                continue;
            }
            if rules.required {
                return Err(ProofFrameError::contract(
                    ErrorCode::MissingColumn,
                    format!("Required contract column `{name}` is absent from the Arrow schema"),
                    Some(column_path(name)),
                ));
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
            let rules = compile_rules(
                field.name(),
                field.data_type(),
                &kernel,
                source_rules,
                (None, None),
            )?;
            columns.push(ColumnPlan {
                column_index,
                field: field.clone(),
                kernel,
                rules,
            });
        }

        Ok(Self {
            schema: schema.clone(),
            columns,
            row_plans: Vec::new(),
            dataset_plan: DatasetPlan::default(),
            version: ContractVersion::V1,
            max_findings: contract.max_findings,
        })
    }

    /// Compile either frozen V1 syntax or the relational V2 contract language.
    pub fn compile_document(
        document: &ContractDocument,
        schema: &Schema,
    ) -> Result<Self, ProofFrameError> {
        document.ensure_active()?;
        match document {
            ContractDocument::V1(contract) => Self::compile(contract, schema),
            ContractDocument::V2(contract) => Self::compile_v2(contract, schema),
        }
    }

    fn compile_v2(contract: &ContractAstV2, schema: &Schema) -> Result<Self, ProofFrameError> {
        for (name, rules) in &contract.columns {
            let Ok(column_index) = schema.index_of(name) else {
                if rules.required || has_v2_value_rules(rules) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::MissingColumn,
                        format!("Contract column `{name}` is absent from the Arrow schema"),
                        Some(column_path(name)),
                    ));
                }
                continue;
            };
            if let Some(expected) = rules.expected_type.as_ref() {
                let actual = schema.field(column_index).data_type();
                if !expected_type_matches(expected, actual) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractTypeMismatch,
                        format!("Column `{name}` requires `{expected:?}`, found `{actual}`"),
                        Some(append_path(&column_path(name), "type")),
                    ));
                }
            }
        }

        let mut columns = Vec::with_capacity(contract.columns.len());
        for (column_index, field) in schema.fields().iter().enumerate() {
            let Some(source) = contract.columns.get(field.name()) else {
                continue;
            };
            let source_rules = v2_rules_as_v1(source);
            // A column whose only rule is a length still has a rule; the V1 view of
            // it does not carry one, so it is asked for separately.
            let declares_length = source.min_length.is_some() || source.max_length.is_some();
            if !has_runtime_rules(&source_rules) && !declares_length {
                continue;
            }
            let kernel = KernelKind::from_data_type(field.data_type());
            let rules = compile_rules(
                field.name(),
                field.data_type(),
                &kernel,
                &source_rules,
                (source.min_length, source.max_length),
            )?;
            columns.push(ColumnPlan {
                column_index,
                field: field.clone(),
                kernel,
                rules,
            });
        }

        let row_plans = contract
            .row_rules
            .iter()
            .enumerate()
            .map(|(index, rule)| RowPlan::compile(rule, schema, index))
            .collect::<Result<Vec<_>, _>>()?;
        let dataset_plan = DatasetPlan::compile(contract, schema)?;
        Ok(Self {
            schema: schema.clone(),
            columns,
            row_plans,
            dataset_plan,
            version: ContractVersion::V2,
            max_findings: contract.max_findings,
        })
    }

    #[must_use]
    pub fn columns(&self) -> &[ColumnPlan] {
        &self.columns
    }

    #[must_use]
    pub const fn schema(&self) -> &Schema {
        &self.schema
    }

    #[must_use]
    pub fn row_plans(&self) -> &[RowPlan] {
        &self.row_plans
    }

    #[must_use]
    pub const fn dataset_plan(&self) -> &DatasetPlan {
        &self.dataset_plan
    }

    #[must_use]
    pub const fn max_findings(&self) -> usize {
        self.max_findings
    }

    /// Domain-separated digest of the Arrow schema used during compilation.
    pub fn schema_digest(&self) -> Result<String, ProofFrameError> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proofframe:schema:v1\0");
        hasher.update(&crate::encoding::canonical_schema_digest(&self.schema)?);
        Ok(tagged_digest("pf-schema-v1:", hasher.finalize()))
    }

    /// Domain-separated digest of the schema-resolved execution plan.
    pub fn compiled_plan_digest(&self) -> Result<String, ProofFrameError> {
        if self.version == ContractVersion::V2 {
            return self.compiled_plan_digest_v2();
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proofframe:compiled-plan:v1\0");
        hasher.update(&crate::encoding::canonical_schema_digest(&self.schema)?);
        hasher.update(&(self.max_findings as u64).to_le_bytes());
        for column in &self.columns {
            hasher.update(&(column.column_index as u64).to_le_bytes());
            hash_part(&mut hasher, column.field.name().as_bytes());
            hash_kernel(&mut hasher, &column.kernel);
            let rules = &column.rules;
            hasher.update(&[
                u8::from(rules.required),
                u8::from(rules.not_null),
                u8::from(rules.unique),
                u8::from(rules.validate_nan),
                match rules.nan {
                    NaNPolicy::Reject => 0,
                    NaNPolicy::Allow => 1,
                },
            ]);
            hash_optional_bound(&mut hasher, rules.min.as_ref());
            hash_optional_bound(&mut hasher, rules.max.as_ref());
            hash_optional_part(
                &mut hasher,
                rules
                    .pattern
                    .as_ref()
                    .map(|value| value.as_str().as_bytes()),
            );
            if let Some(allowed) = rules.allowed.as_deref() {
                hasher.update(&[1]);
                let mut values = allowed.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
                values.sort_unstable();
                hasher.update(&(values.len() as u64).to_le_bytes());
                for value in values {
                    hash_part(&mut hasher, value.as_bytes());
                }
            } else {
                hasher.update(&[0]);
            }
        }
        Ok(tagged_digest("pf-plan-v1:", hasher.finalize()))
    }

    fn compiled_plan_digest_v2(&self) -> Result<String, ProofFrameError> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proofframe:compiled-plan:v2\0");
        hasher.update(&crate::encoding::canonical_schema_digest(&self.schema)?);
        hasher.update(&(self.max_findings as u64).to_le_bytes());
        for column in &self.columns {
            hasher.update(&(column.column_index as u64).to_le_bytes());
            hash_part(&mut hasher, column.field.name().as_bytes());
            hash_kernel(&mut hasher, &column.kernel);
            hash_compiled_rules(&mut hasher, &column.rules);
        }
        hasher.update(&(self.row_plans.len() as u64).to_le_bytes());
        for row in &self.row_plans {
            hash_row_plan(&mut hasher, row);
        }
        hash_dataset_plan(&mut hasher, &self.dataset_plan);
        Ok(tagged_digest("pf-plan-v2:", hasher.finalize()))
    }
}

fn hash_row_plan(hasher: &mut blake3::Hasher, plan: &RowPlan) {
    hash_part(hasher, plan.name().as_bytes());
    match plan.kind() {
        RowPlanKind::Compare(compare) => {
            hasher.update(&[0]);
            hash_compare_plan(hasher, compare);
        }
        RowPlanKind::Conditional {
            predicate,
            assertion_column,
            assertion_field,
            assertion,
        } => {
            hasher.update(&[1]);
            hash_compare_plan(hasher, predicate);
            hasher.update(&(*assertion_column as u64).to_le_bytes());
            hash_part(hasher, assertion_field.name().as_bytes());
            hash_compiled_rules(hasher, assertion);
        }
    }
}

fn hash_compare_plan(hasher: &mut blake3::Hasher, plan: &ComparePlan) {
    hash_operand_plan(hasher, plan.left());
    hasher.update(&[compare_op_tag(plan.op()), null_policy_tag(plan.nulls())]);
    hash_operand_plan(hasher, plan.right());
}

fn hash_operand_plan(hasher: &mut blake3::Hasher, operand: &OperandPlan) {
    match operand {
        OperandPlan::Column {
            column_index,
            field,
            kernel,
        } => {
            hasher.update(&[0]);
            hasher.update(&(*column_index as u64).to_le_bytes());
            hash_part(hasher, field.name().as_bytes());
            hash_kernel(hasher, kernel);
        }
        OperandPlan::Literal(value) => {
            hasher.update(&[1]);
            match value {
                ScalarValuePlan::Boolean(value) => {
                    hasher.update(&[0, u8::from(*value)]);
                }
                ScalarValuePlan::I64(value) => {
                    hasher.update(&[1]);
                    hasher.update(&value.to_le_bytes());
                }
                ScalarValuePlan::U64(value) => {
                    hasher.update(&[2]);
                    hasher.update(&value.to_le_bytes());
                }
                ScalarValuePlan::F64(value) => {
                    hasher.update(&[3]);
                    hasher.update(&value.to_bits().to_le_bytes());
                }
                ScalarValuePlan::Text(value) => {
                    hasher.update(&[4]);
                    hash_part(hasher, value.as_bytes());
                }
            }
        }
    }
}

fn hash_dataset_plan(hasher: &mut blake3::Hasher, plan: &DatasetPlan) {
    hash_optional_count_range(hasher, plan.row_count());
    hasher.update(&(plan.null_ratios().len() as u64).to_le_bytes());
    for ratio in plan.null_ratios() {
        hasher.update(&(ratio.column_index() as u64).to_le_bytes());
        hash_part(hasher, ratio.column().as_bytes());
        hash_optional_f64(hasher, ratio.range().min);
        hash_optional_f64(hasher, ratio.range().max);
        hash_kernel(hasher, ratio.kernel());
    }
    hasher.update(&(plan.distinct_counts().len() as u64).to_le_bytes());
    for count in plan.distinct_counts() {
        hasher.update(&(count.column_index() as u64).to_le_bytes());
        hash_part(hasher, count.column().as_bytes());
        hash_count_range(hasher, count.range());
        hash_kernel(hasher, count.kernel());
    }
    hasher.update(&(plan.distinct_ratios().len() as u64).to_le_bytes());
    for ratio in plan.distinct_ratios() {
        hasher.update(&(ratio.column_index() as u64).to_le_bytes());
        hash_part(hasher, ratio.column().as_bytes());
        hash_optional_f64(hasher, ratio.range().min);
        hash_optional_f64(hasher, ratio.range().max);
        hash_kernel(hasher, ratio.kernel());
    }
    hasher.update(&(plan.composite_unique().len() as u64).to_le_bytes());
    for composite in plan.composite_unique() {
        hash_part(hasher, composite.name().as_bytes());
        hasher.update(&(composite.columns().len() as u64).to_le_bytes());
        for column in composite.columns() {
            hasher.update(&(*column as u64).to_le_bytes());
        }
        hasher.update(&[match composite.nulls() {
            CompositeNullPolicyAst::Equal => 0,
            CompositeNullPolicyAst::Reject => 1,
        }]);
    }
    hash_reference_plans(hasher, plan.references());
    hash_monotonicity_plans(hasher, plan.monotonicity());
    hash_gap_plans(hasher, plan.gap_detection());
    hash_exclusive_plans(hasher, plan.mutually_exclusive());
    hash_sum_plans(hasher, plan.sums());
    hash_statistic_plans(hasher, plan.statistics());
    hash_conditional_unique_plans(hasher, plan.conditional_unique());
    hash_row_count_delta_plans(hasher, plan.row_count_delta());
}

const ROW_DELTA_DOMAIN: &[u8] = b"proofframe:compiled-plan:row-count-delta:v1\0";

const CONDITIONAL_DOMAIN: &[u8] = b"proofframe:compiled-plan:conditional-unique:v1\0";

const LENGTH_DOMAIN: &[u8] = b"proofframe:compiled-plan:length:v1\0";

const STATISTIC_DOMAIN: &[u8] = b"proofframe:compiled-plan:statistic:v1\0";

const SUM_DOMAIN: &[u8] = b"proofframe:compiled-plan:sum:v1\0";

const EXCLUSIVE_DOMAIN: &[u8] = b"proofframe:compiled-plan:mutually-exclusive:v1\0";

const GAP_DOMAIN: &[u8] = b"proofframe:compiled-plan:gap-detection:v1\0";

/// Append ordering rules only when the plan carries them.
///
/// Same reason as references: a contract written before this rule existed must keep
/// the `pf-plan-v2` digest its receipts already record.
fn hash_monotonicity_plans(hasher: &mut blake3::Hasher, rules: &[MonotonicityPlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(b"proofframe:compiled-plan:monotonicity:v1\0");
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hasher.update(&(rule.column_index() as u64).to_le_bytes());
        hash_part(hasher, rule.column().as_bytes());
        hash_kernel(hasher, rule.kernel());
        hasher.update(&[match rule.direction() {
            MonotonicDirectionAst::Increasing => 0,
            MonotonicDirectionAst::StrictlyIncreasing => 1,
            MonotonicDirectionAst::Decreasing => 2,
            MonotonicDirectionAst::StrictlyDecreasing => 3,
        }]);
        hasher.update(&[match rule.nulls() {
            MonotonicNullPolicyAst::Skip => 0,
            MonotonicNullPolicyAst::Reject => 1,
        }]);
    }
}

/// Append referential rules only when the plan carries them.
///
/// A plan without references and a plan with an empty reference list execute
/// identically, so collapsing them is not a collision. Appending an unconditional
/// zero count would instead be a silent break: every contract written before
/// references existed would change its `pf-plan-v2` digest and stop matching the
/// receipts already issued for it.
fn hash_reference_plans(hasher: &mut blake3::Hasher, references: &[ReferencePlan]) {
    if references.is_empty() {
        return;
    }
    hasher.update(b"proofframe:compiled-plan:references:v1\0");
    hasher.update(&(references.len() as u64).to_le_bytes());
    for reference in references {
        hash_part(hasher, reference.name().as_bytes());
        hash_part(hasher, reference.reference().as_bytes());
        hasher.update(&(reference.columns().len() as u64).to_le_bytes());
        for (column, field) in reference.columns().iter().zip(reference.fields()) {
            hasher.update(&(*column as u64).to_le_bytes());
            hash_part(hasher, field.name().as_bytes());
            hash_kernel(
                hasher,
                &KernelKind::from_data_type_for_plan(field.data_type()),
            );
        }
        for column in reference.reference_columns() {
            hash_part(hasher, column.as_bytes());
        }
        hasher.update(&[match reference.nulls() {
            ReferenceNullPolicyAst::Skip => 0,
            ReferenceNullPolicyAst::Reject => 1,
        }]);
    }
}

/// Append step rules only when the plan carries them, for the same reason as the
/// rules above: a contract written before they existed keeps its plan identity.
fn hash_gap_plans(hasher: &mut blake3::Hasher, rules: &[GapDetectionPlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(GAP_DOMAIN);
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hasher.update(&(rule.column_index() as u64).to_le_bytes());
        hash_part(hasher, rule.column().as_bytes());
        hash_kernel(hasher, rule.kernel());
        hasher.update(&rule.expected_step().to_bits().to_le_bytes());
        hasher.update(&rule.tolerance().to_bits().to_le_bytes());
        hasher.update(&rule.max_gaps().to_le_bytes());
        hasher.update(&[match rule.nulls() {
            MonotonicNullPolicyAst::Skip => 0,
            MonotonicNullPolicyAst::Reject => 1,
        }]);
    }
}

/// Append exclusivity rules only when the plan carries them.
fn hash_exclusive_plans(hasher: &mut blake3::Hasher, rules: &[MutuallyExclusivePlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(EXCLUSIVE_DOMAIN);
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hasher.update(&(rule.columns().len() as u64).to_le_bytes());
        for column in rule.columns() {
            hasher.update(&(*column as u64).to_le_bytes());
        }
        hasher.update(&[match rule.mode() {
            ExclusiveModeAst::AtMostOne => 0,
            ExclusiveModeAst::ExactlyOne => 1,
        }]);
    }
}

/// Append total rules only when the plan carries them.
fn hash_sum_plans(hasher: &mut blake3::Hasher, rules: &[SumPlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(SUM_DOMAIN);
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hasher.update(&(rule.column_index() as u64).to_le_bytes());
        hash_part(hasher, rule.column().as_bytes());
        hash_kernel(hasher, rule.kernel());
        match rule.bounds() {
            SumBounds::Integer { min, max } => {
                hasher.update(&[0]);
                hash_optional_i128(hasher, *min);
                hash_optional_i128(hasher, *max);
            }
            SumBounds::Float { min, max } => {
                hasher.update(&[1]);
                hash_optional_f64(hasher, *min);
                hash_optional_f64(hasher, *max);
            }
        }
    }
}

fn hash_optional_i128(hasher: &mut blake3::Hasher, value: Option<i128>) {
    match value {
        None => hasher.update(&[0]),
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_le_bytes())
        }
    };
}

/// Append statistic rules only when the plan carries them.
fn hash_statistic_plans(hasher: &mut blake3::Hasher, rules: &[StatisticPlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(STATISTIC_DOMAIN);
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hasher.update(&(rule.column_index() as u64).to_le_bytes());
        hash_part(hasher, rule.column().as_bytes());
        hash_kernel(hasher, rule.kernel());
        hasher.update(&[match rule.kind() {
            StatisticKind::Mean => 0,
            StatisticKind::StdDev => 1,
        }]);
        hash_optional_f64(hasher, rule.min());
        hash_optional_f64(hasher, rule.max());
    }
}

/// Append a declared length only when there is one, so a contract written before
/// lengths existed keeps the plan identity its receipts record.
fn hash_optional_length(hasher: &mut blake3::Hasher, value: Option<usize>) {
    match value {
        None => hasher.update(&[0]),
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&(value as u64).to_le_bytes())
        }
    };
}

/// Append conditional uniqueness only when the plan carries it.
fn hash_conditional_unique_plans(hasher: &mut blake3::Hasher, rules: &[ConditionalUniquePlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(CONDITIONAL_DOMAIN);
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hasher.update(&(rule.columns().len() as u64).to_le_bytes());
        for column in rule.columns() {
            hasher.update(&(*column as u64).to_le_bytes());
        }
        hasher.update(&[match rule.nulls() {
            CompositeNullPolicyAst::Equal => 0,
            CompositeNullPolicyAst::Reject => 1,
        }]);
        hash_compare_plan(hasher, rule.predicate());
    }
}

/// Append row-count comparisons only when the plan carries them.
fn hash_row_count_delta_plans(hasher: &mut blake3::Hasher, rules: &[RowCountDeltaPlan]) {
    if rules.is_empty() {
        return;
    }
    hasher.update(ROW_DELTA_DOMAIN);
    hasher.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        hash_part(hasher, rule.name().as_bytes());
        hash_part(hasher, rule.reference().as_bytes());
        hash_optional_f64(hasher, rule.min_ratio());
        hash_optional_f64(hasher, rule.max_ratio());
    }
}

fn hash_optional_count_range(hasher: &mut blake3::Hasher, range: Option<&CountRangeAst>) {
    if let Some(range) = range {
        hasher.update(&[1]);
        hash_count_range(hasher, range);
    } else {
        hasher.update(&[0]);
    }
}

fn hash_count_range(hasher: &mut blake3::Hasher, range: &CountRangeAst) {
    hash_optional_u64(hasher, range.exact);
    hash_optional_u64(hasher, range.min);
    hash_optional_u64(hasher, range.max);
}

fn hash_optional_u64(hasher: &mut blake3::Hasher, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_f64(hasher: &mut blake3::Hasher, value: Option<f64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_bits().to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

const fn compare_op_tag(op: CompareOpAst) -> u8 {
    match op {
        CompareOpAst::Eq => 0,
        CompareOpAst::Ne => 1,
        CompareOpAst::Lt => 2,
        CompareOpAst::Lte => 3,
        CompareOpAst::Gt => 4,
        CompareOpAst::Gte => 5,
    }
}

const fn null_policy_tag(policy: NullPolicyAst) -> u8 {
    match policy {
        NullPolicyAst::Skip => 0,
        NullPolicyAst::Fail => 1,
        NullPolicyAst::Equal => 2,
    }
}

fn hash_compiled_rules(hasher: &mut blake3::Hasher, rules: &CompiledRules) {
    hasher.update(&[
        u8::from(rules.required),
        u8::from(rules.not_null),
        u8::from(rules.unique),
        u8::from(rules.validate_nan),
        match rules.nan {
            NaNPolicy::Reject => 0,
            NaNPolicy::Allow => 1,
        },
    ]);
    hash_optional_bound(hasher, rules.min.as_ref());
    hash_optional_bound(hasher, rules.max.as_ref());
    hash_optional_part(
        hasher,
        rules
            .pattern
            .as_ref()
            .map(|value| value.as_str().as_bytes()),
    );
    if let Some(allowed) = rules.allowed.as_deref() {
        hasher.update(&[1]);
        let mut values = allowed.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
        values.sort_unstable();
        hasher.update(&(values.len() as u64).to_le_bytes());
        for value in values {
            hash_part(hasher, value.as_bytes());
        }
    } else {
        hasher.update(&[0]);
    }
    // Absent lengths append nothing, so a contract written before this rule existed
    // keeps the plan identity its receipts record.
    if rules.min_length.is_some() || rules.max_length.is_some() {
        hasher.update(LENGTH_DOMAIN);
        hash_optional_length(hasher, rules.min_length);
        hash_optional_length(hasher, rules.max_length);
    }
}

fn has_v2_value_rules(rules: &RuleAstV2) -> bool {
    rules.not_null
        || rules.unique
        || rules.min_length.is_some()
        || rules.max_length.is_some()
        || rules.expected_type.is_some()
        || rules.min.is_some()
        || rules.max.is_some()
        || rules.nan.is_some()
        || rules.pattern.is_some()
        || rules.allowed.is_some()
}

fn v2_rules_as_v1(rules: &RuleAstV2) -> RuleAst {
    RuleAst {
        required: rules.required,
        not_null: rules.not_null,
        unique: rules.unique,
        min: rules.min.clone(),
        max: rules.max.clone(),
        nan: rules.nan,
        pattern: rules.pattern.clone(),
        allowed: rules.allowed.clone(),
    }
}

fn expected_type_matches(expected: &TypeAst, actual: &DataType) -> bool {
    match expected {
        TypeAst::Primitive(expected) => matches_primitive_type(*expected, actual),
        TypeAst::Parameterized(ParameterizedTypeAst::Decimal128 { precision, scale }) => {
            matches!(actual, DataType::Decimal128(actual_precision, actual_scale) if actual_precision == precision && actual_scale == scale)
        }
        TypeAst::Parameterized(ParameterizedTypeAst::Timestamp { unit, timezone }) => {
            matches!(actual, DataType::Timestamp(actual_unit, actual_timezone)
                if time_unit_matches(*unit, *actual_unit)
                    && actual_timezone.as_deref() == timezone.as_deref())
        }
    }
}

fn matches_primitive_type(expected: PrimitiveTypeAst, actual: &DataType) -> bool {
    matches!(
        (expected, actual),
        (PrimitiveTypeAst::Boolean, DataType::Boolean)
            | (PrimitiveTypeAst::Int8, DataType::Int8)
            | (PrimitiveTypeAst::Int16, DataType::Int16)
            | (PrimitiveTypeAst::Int32, DataType::Int32)
            | (PrimitiveTypeAst::Int64, DataType::Int64)
            | (PrimitiveTypeAst::Uint8, DataType::UInt8)
            | (PrimitiveTypeAst::Uint16, DataType::UInt16)
            | (PrimitiveTypeAst::Uint32, DataType::UInt32)
            | (PrimitiveTypeAst::Uint64, DataType::UInt64)
            | (PrimitiveTypeAst::Float32, DataType::Float32)
            | (PrimitiveTypeAst::Float64, DataType::Float64)
            | (PrimitiveTypeAst::Date32, DataType::Date32)
            | (PrimitiveTypeAst::Date64, DataType::Date64)
            | (PrimitiveTypeAst::Utf8, DataType::Utf8)
            | (PrimitiveTypeAst::LargeUtf8, DataType::LargeUtf8)
            | (PrimitiveTypeAst::Utf8View, DataType::Utf8View)
            | (PrimitiveTypeAst::Binary, DataType::Binary)
            | (PrimitiveTypeAst::LargeBinary, DataType::LargeBinary)
            | (PrimitiveTypeAst::BinaryView, DataType::BinaryView)
    )
}

fn time_unit_matches(expected: TimeUnitAst, actual: TimeUnit) -> bool {
    matches!(
        (expected, actual),
        (TimeUnitAst::S, TimeUnit::Second)
            | (TimeUnitAst::Ms, TimeUnit::Millisecond)
            | (TimeUnitAst::Us, TimeUnit::Microsecond)
            | (TimeUnitAst::Ns, TimeUnit::Nanosecond)
    )
}

fn tagged_digest(prefix: &str, digest: blake3::Hash) -> String {
    let hex = digest.to_hex();
    let mut output = String::with_capacity(prefix.len() + hex.len());
    output.push_str(prefix);
    output.push_str(hex.as_str());
    output
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_optional_part(hasher: &mut blake3::Hasher, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            hasher.update(&[1]);
            hash_part(hasher, bytes);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_kernel(hasher: &mut blake3::Hasher, kernel: &KernelKind) {
    hasher.update(&[kernel_tag(kernel)]);
    hash_kernel_payload(hasher, kernel);
}

fn scalar_kernel_tag(kernel: &KernelKind) -> Option<u8> {
    Some(match kernel {
        KernelKind::Boolean => 0,
        KernelKind::I8 => 1,
        KernelKind::I16 => 2,
        KernelKind::I32 => 3,
        KernelKind::I64 => 4,
        KernelKind::U8 => 5,
        KernelKind::U16 => 6,
        KernelKind::U32 => 7,
        KernelKind::U64 => 8,
        KernelKind::F32 => 9,
        KernelKind::F64 => 10,
        _ => return None,
    })
}

fn temporal_kernel_tag(kernel: &KernelKind) -> Option<u8> {
    match kernel {
        KernelKind::Date32 => Some(11),
        KernelKind::Date64 => Some(12),
        KernelKind::Decimal128 { .. } => Some(13),
        KernelKind::Timestamp(_) => Some(14),
        _ => None,
    }
}

fn variable_kernel_tag(kernel: &KernelKind) -> u8 {
    match kernel {
        KernelKind::Utf8 => 15,
        KernelKind::LargeUtf8 => 16,
        KernelKind::Binary => 17,
        KernelKind::LargeBinary => 18,
        KernelKind::Nested => 19,
        KernelKind::NullOnly => 20,
        KernelKind::Utf8View => 21,
        KernelKind::BinaryView => 22,
        _ => unreachable!("scalar and temporal kernels are handled first"),
    }
}

fn kernel_tag(kernel: &KernelKind) -> u8 {
    if let Some(tag) = scalar_kernel_tag(kernel) {
        return tag;
    }
    if let Some(tag) = temporal_kernel_tag(kernel) {
        return tag;
    }
    variable_kernel_tag(kernel)
}

fn hash_kernel_payload(hasher: &mut blake3::Hasher, kernel: &KernelKind) {
    match kernel {
        KernelKind::Decimal128 { precision, scale } => {
            hasher.update(&[*precision, *scale as u8]);
        }
        KernelKind::Timestamp(unit) => hash_part(hasher, format!("{unit:?}").as_bytes()),
        _ => {}
    }
}

fn hash_optional_bound(hasher: &mut blake3::Hasher, bound: Option<&TypedBound>) {
    let Some(bound) = bound else {
        hasher.update(&[0]);
        return;
    };
    hasher.update(&[1]);
    match bound {
        TypedBound::I64(value) => {
            hasher.update(&[0]);
            hasher.update(&value.to_le_bytes());
        }
        TypedBound::U64(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_le_bytes());
        }
        TypedBound::F32(value) => {
            hasher.update(&[2]);
            hasher.update(&value.to_bits().to_le_bytes());
        }
        TypedBound::F64(value) => {
            hasher.update(&[3]);
            hasher.update(&value.to_bits().to_le_bytes());
        }
        TypedBound::Decimal128 { value, scale } => {
            hasher.update(&[4, *scale as u8]);
            hasher.update(&value.to_le_bytes());
        }
        TypedBound::Timestamp { value, unit } => {
            hasher.update(&[5]);
            hasher.update(&value.to_le_bytes());
            hash_part(hasher, format!("{unit:?}").as_bytes());
        }
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

/// Compile one column's rules.
///
/// `length` is passed in rather than read off `source` because V1 contracts are
/// frozen: the value rules are shared, and only V2 may declare a length.
pub(super) fn compile_rules(
    column: &str,
    data_type: &DataType,
    kernel: &KernelKind,
    source: &RuleAst,
    length: (Option<usize>, Option<usize>),
) -> Result<CompiledRules, ProofFrameError> {
    let (min_length, max_length) = length;
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

    if min_length
        .zip(max_length)
        .is_some_and(|(low, high)| low > high)
    {
        return Err(ProofFrameError::contract(
            ErrorCode::ContractInvalidBound,
            format!("`min_length` exceeds `max_length` for column `{column}`"),
            Some(append_path(&base_path, "min_length")),
        ));
    }

    Ok(CompiledRules {
        required: source.required,
        not_null: source.not_null,
        unique: source.unique,
        min,
        max,
        nan: source.nan.into(),
        validate_nan: source.nan.is_some() || source.min.is_some() || source.max.is_some(),
        pattern,
        allowed,
        min_length,
        max_length,
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
