use arrow::datatypes::{FieldRef, Schema};

use super::{
    CompositeNullPolicyAst, ContractAstV2, CountRangeAst, ExclusiveModeAst, MonotonicDirectionAst,
    MonotonicNullPolicyAst, RatioRangeAst, ReferenceNullPolicyAst,
};
use crate::{ErrorCode, KernelKind, ProofFrameError};

/// One resolved referential integrity rule.
///
/// Only the local side resolves at compile time. The reference columns stay as names
/// because the reference dataset is supplied per execution, not per contract, and its
/// schema is therefore unknown until the caller binds it.
#[derive(Debug, Clone)]
pub struct ReferencePlan {
    name: Box<str>,
    pub(crate) columns: Vec<usize>,
    pub(crate) fields: Vec<FieldRef>,
    pub(crate) reference: Box<str>,
    pub(crate) reference_columns: Vec<Box<str>>,
    pub(crate) nulls: ReferenceNullPolicyAst,
}

impl ReferencePlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn columns(&self) -> &[usize] {
        &self.columns
    }
    #[must_use]
    pub fn fields(&self) -> &[FieldRef] {
        &self.fields
    }
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }
    #[must_use]
    pub fn reference_columns(&self) -> &[Box<str>] {
        &self.reference_columns
    }
    #[must_use]
    pub const fn nulls(&self) -> ReferenceNullPolicyAst {
        self.nulls
    }
}

#[derive(Debug, Clone)]
pub struct RatioPlan {
    pub(crate) column_index: usize,
    pub(crate) column: Box<str>,
    pub(crate) range: RatioRangeAst,
    pub(crate) kernel: KernelKind,
}

impl RatioPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }
    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }
    #[must_use]
    pub const fn range(&self) -> &RatioRangeAst {
        &self.range
    }
    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }
}

#[derive(Debug, Clone)]
pub struct CountPlan {
    pub(crate) column_index: usize,
    pub(crate) column: Box<str>,
    pub(crate) range: CountRangeAst,
    pub(crate) kernel: KernelKind,
}

impl CountPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }
    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }
    #[must_use]
    pub const fn range(&self) -> &CountRangeAst {
        &self.range
    }
    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }
}

#[derive(Debug, Clone)]
pub struct CompositeUniquePlan {
    name: Box<str>,
    pub(crate) columns: Vec<usize>,
    pub(crate) nulls: CompositeNullPolicyAst,
}

impl CompositeUniquePlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn columns(&self) -> &[usize] {
        &self.columns
    }
    #[must_use]
    pub const fn nulls(&self) -> CompositeNullPolicyAst {
        self.nulls
    }
}

/// Two numeric columns that must total the same amount.
#[derive(Debug, Clone, PartialEq)]
pub struct BalanceEqualPlan {
    pub(crate) name: Box<str>,
    pub(crate) left: usize,
    pub(crate) right: usize,
    pub(crate) left_column: Box<str>,
    pub(crate) right_column: Box<str>,
    pub(crate) left_kernel: KernelKind,
    pub(crate) right_kernel: KernelKind,
    pub(crate) tolerance: f64,
}

impl BalanceEqualPlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub const fn left(&self) -> usize {
        self.left
    }
    #[must_use]
    pub const fn right(&self) -> usize {
        self.right
    }
    #[must_use]
    pub fn left_column(&self) -> &str {
        &self.left_column
    }
    #[must_use]
    pub fn right_column(&self) -> &str {
        &self.right_column
    }
    #[must_use]
    pub const fn left_kernel(&self) -> &KernelKind {
        &self.left_kernel
    }
    #[must_use]
    pub const fn right_kernel(&self) -> &KernelKind {
        &self.right_kernel
    }
    #[must_use]
    pub const fn tolerance(&self) -> f64 {
        self.tolerance
    }
}

/// The largest share of a column one value may hold.
#[derive(Debug, Clone, PartialEq)]
pub struct DominantValuePlan {
    pub(crate) name: Box<str>,
    pub(crate) column: Box<str>,
    pub(crate) column_index: usize,
    pub(crate) max: f64,
}

impl DominantValuePlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }
    #[must_use]
    pub const fn max(&self) -> f64 {
        self.max
    }
}

/// How far this dataset's row count may move from a reference dataset's.
#[derive(Debug, Clone, PartialEq)]
pub struct RowCountDeltaPlan {
    pub(crate) name: Box<str>,
    pub(crate) reference: Box<str>,
    pub(crate) min_ratio: Option<f64>,
    pub(crate) max_ratio: Option<f64>,
}

impl RowCountDeltaPlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The logical name the caller must bind a dataset to.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    #[must_use]
    pub const fn min_ratio(&self) -> Option<f64> {
        self.min_ratio
    }

    #[must_use]
    pub const fn max_ratio(&self) -> Option<f64> {
        self.max_ratio
    }
}

/// Uniqueness among the rows a predicate selects.
#[derive(Debug, Clone)]
pub struct ConditionalUniquePlan {
    pub(crate) name: Box<str>,
    pub(crate) columns: Vec<usize>,
    pub(crate) nulls: CompositeNullPolicyAst,
    pub(crate) predicate: super::ComparePlan,
}

impl ConditionalUniquePlan {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn columns(&self) -> &[usize] {
        &self.columns
    }

    #[must_use]
    pub const fn nulls(&self) -> CompositeNullPolicyAst {
        self.nulls
    }

    #[must_use]
    pub const fn predicate(&self) -> &super::ComparePlan {
        &self.predicate
    }
}

/// Which summary a statistic rule bounds.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StatisticKind {
    Mean,
    /// Population standard deviation, over the values the column actually held.
    StdDev,
}

impl StatisticKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::StdDev => "std_dev",
        }
    }
}

/// A resolved statistic rule.
#[derive(Debug, Clone, PartialEq)]
pub struct StatisticPlan {
    pub(crate) column_index: usize,
    pub(crate) column: Box<str>,
    pub(crate) name: Box<str>,
    pub(crate) kernel: KernelKind,
    pub(crate) kind: StatisticKind,
    pub(crate) min: Option<f64>,
    pub(crate) max: Option<f64>,
}

impl StatisticPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }

    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }

    #[must_use]
    pub const fn kind(&self) -> StatisticKind {
        self.kind
    }

    #[must_use]
    pub const fn min(&self) -> Option<f64> {
        self.min
    }

    #[must_use]
    pub const fn max(&self) -> Option<f64> {
        self.max
    }
}

/// Whether a column's total is counted exactly or with floating-point compensation,
/// and the bounds it must stay inside.
#[derive(Debug, Clone, PartialEq)]
pub enum SumBounds {
    Integer {
        min: Option<i128>,
        max: Option<i128>,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
}

/// A resolved total rule.
#[derive(Debug, Clone, PartialEq)]
pub struct SumPlan {
    pub(crate) column_index: usize,
    pub(crate) column: Box<str>,
    pub(crate) name: Box<str>,
    pub(crate) kernel: KernelKind,
    pub(crate) bounds: SumBounds,
}

impl SumPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }

    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }

    #[must_use]
    pub const fn bounds(&self) -> &SumBounds {
        &self.bounds
    }
}

/// A resolved exclusivity rule: which columns, and how many of them may be filled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutuallyExclusivePlan {
    pub(crate) columns: Vec<usize>,
    pub(crate) names: Vec<Box<str>>,
    pub(crate) name: Box<str>,
    pub(crate) mode: ExclusiveModeAst,
}

impl MutuallyExclusivePlan {
    #[must_use]
    pub fn columns(&self) -> &[usize] {
        &self.columns
    }

    #[must_use]
    pub fn names(&self) -> &[Box<str>] {
        &self.names
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn mode(&self) -> ExclusiveModeAst {
        self.mode
    }
}

/// A resolved step rule: which column, how far apart consecutive values may be, and
/// the unit that distance is measured in.
#[derive(Debug, Clone, PartialEq)]
pub struct GapDetectionPlan {
    pub(crate) column_index: usize,
    pub(crate) column: Box<str>,
    pub(crate) name: Box<str>,
    pub(crate) expected_step: f64,
    pub(crate) tolerance: f64,
    pub(crate) max_gaps: u64,
    pub(crate) nulls: MonotonicNullPolicyAst,
    pub(crate) kernel: KernelKind,
    pub(crate) unit: &'static str,
}

impl GapDetectionPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }

    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn expected_step(&self) -> f64 {
        self.expected_step
    }

    #[must_use]
    pub const fn tolerance(&self) -> f64 {
        self.tolerance
    }

    #[must_use]
    pub const fn max_gaps(&self) -> u64 {
        self.max_gaps
    }

    #[must_use]
    pub const fn nulls(&self) -> MonotonicNullPolicyAst {
        self.nulls
    }

    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }

    /// The unit `expected_step` is expressed in, named so a finding can say it.
    #[must_use]
    pub const fn unit(&self) -> &'static str {
        self.unit
    }
}

/// A resolved ordering rule: which column, which direction, and what a null means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonotonicityPlan {
    pub(crate) column_index: usize,
    pub(crate) column: Box<str>,
    pub(crate) name: Box<str>,
    pub(crate) direction: MonotonicDirectionAst,
    pub(crate) nulls: MonotonicNullPolicyAst,
    pub(crate) kernel: KernelKind,
}

impl MonotonicityPlan {
    #[must_use]
    pub const fn column_index(&self) -> usize {
        self.column_index
    }

    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn direction(&self) -> MonotonicDirectionAst {
        self.direction
    }

    #[must_use]
    pub const fn nulls(&self) -> MonotonicNullPolicyAst {
        self.nulls
    }

    #[must_use]
    pub const fn kernel(&self) -> &KernelKind {
        &self.kernel
    }
}

#[derive(Debug, Clone, Default)]
pub struct DatasetPlan {
    pub(crate) row_count: Option<CountRangeAst>,
    pub(crate) null_ratios: Vec<RatioPlan>,
    pub(crate) distinct_counts: Vec<CountPlan>,
    pub(crate) distinct_ratios: Vec<RatioPlan>,
    pub(crate) composite_unique: Vec<CompositeUniquePlan>,
    pub(crate) references: Vec<ReferencePlan>,
    pub(crate) monotonicity: Vec<MonotonicityPlan>,
    pub(crate) gap_detection: Vec<GapDetectionPlan>,
    pub(crate) mutually_exclusive: Vec<MutuallyExclusivePlan>,
    pub(crate) sums: Vec<SumPlan>,
    pub(crate) statistics: Vec<StatisticPlan>,
    pub(crate) conditional_unique: Vec<ConditionalUniquePlan>,
    pub(crate) row_count_delta: Vec<RowCountDeltaPlan>,
    pub(crate) balance_equal: Vec<BalanceEqualPlan>,
    pub(crate) dominant_value: Vec<DominantValuePlan>,
}

impl DatasetPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.row_count.is_none()
            && self.null_ratios.is_empty()
            && self.distinct_counts.is_empty()
            && self.distinct_ratios.is_empty()
            && self.composite_unique.is_empty()
            && self.references.is_empty()
            && self.monotonicity.is_empty()
            && self.gap_detection.is_empty()
            && self.mutually_exclusive.is_empty()
            && self.sums.is_empty()
            && self.statistics.is_empty()
            && self.conditional_unique.is_empty()
            && self.row_count_delta.is_empty()
            && self.balance_equal.is_empty()
            && self.dominant_value.is_empty()
    }

    #[must_use]
    pub fn balance_equal(&self) -> &[BalanceEqualPlan] {
        &self.balance_equal
    }

    #[must_use]
    pub fn dominant_value(&self) -> &[DominantValuePlan] {
        &self.dominant_value
    }

    #[must_use]
    pub fn row_count_delta(&self) -> &[RowCountDeltaPlan] {
        &self.row_count_delta
    }

    #[must_use]
    pub fn conditional_unique(&self) -> &[ConditionalUniquePlan] {
        &self.conditional_unique
    }

    #[must_use]
    pub fn statistics(&self) -> &[StatisticPlan] {
        &self.statistics
    }

    #[must_use]
    pub fn sums(&self) -> &[SumPlan] {
        &self.sums
    }

    #[must_use]
    pub fn mutually_exclusive(&self) -> &[MutuallyExclusivePlan] {
        &self.mutually_exclusive
    }

    #[must_use]
    pub fn gap_detection(&self) -> &[GapDetectionPlan] {
        &self.gap_detection
    }

    #[must_use]
    pub fn monotonicity(&self) -> &[MonotonicityPlan] {
        &self.monotonicity
    }

    pub(crate) fn compile(
        source: &ContractAstV2,
        schema: &Schema,
    ) -> Result<Self, ProofFrameError> {
        let rules = &source.dataset_rules;
        let null_ratios = rules
            .null_ratio
            .iter()
            .map(|(column, range)| ratio_plan(schema, column, range, "null_ratio"))
            .collect::<Result<Vec<_>, _>>()?;
        let distinct_counts = rules
            .distinct_count
            .iter()
            .map(|(column, range)| count_plan(schema, column, range))
            .collect::<Result<Vec<_>, _>>()?;
        let distinct_ratios = rules
            .distinct_ratio
            .iter()
            .map(|(column, range)| ratio_plan(schema, column, range, "distinct_ratio"))
            .collect::<Result<Vec<_>, _>>()?;
        let composite_unique = rules
            .composite_unique
            .iter()
            .enumerate()
            .map(|(rule_index, rule)| {
                let columns = rule
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(column_index, column)| {
                        schema.index_of(column).map_err(|_| {
                            ProofFrameError::contract(
                                ErrorCode::MissingColumn,
                                format!("Composite key column `{column}` is absent"),
                                Some(format!(
                                    "$.dataset_rules.composite_unique[{rule_index}].columns[{column_index}]"
                                )),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(CompositeUniquePlan {
                    name: rule.name.clone().into_boxed_str(),
                    columns,
                    nulls: rule.nulls,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let references = rules
            .references
            .iter()
            .enumerate()
            .map(|(rule_index, rule)| {
                let mut columns = Vec::with_capacity(rule.columns.len());
                let mut fields = Vec::with_capacity(rule.columns.len());
                for (column_index, column) in rule.columns.iter().enumerate() {
                    let resolved = schema.index_of(column).map_err(|_| {
                        ProofFrameError::contract(
                            ErrorCode::MissingColumn,
                            format!("Reference key column `{column}` is absent"),
                            Some(format!(
                                "$.dataset_rules.references[{rule_index}].columns[{column_index}]"
                            )),
                        )
                    })?;
                    columns.push(resolved);
                    fields.push(schema.field(resolved).clone().into());
                }
                Ok(ReferencePlan {
                    name: rule.name.clone().into_boxed_str(),
                    columns,
                    fields,
                    reference: rule.reference.clone().into_boxed_str(),
                    reference_columns: rule
                        .reference_columns
                        .iter()
                        .map(|column| column.clone().into_boxed_str())
                        .collect(),
                    nulls: rule.nulls,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let monotonicity = rules
            .monotonicity
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let column_index = schema.index_of(&rule.column).map_err(|_| {
                    ProofFrameError::contract(
                        ErrorCode::MissingColumn,
                        format!("Monotonicity column `{}` is absent", rule.column),
                        Some(format!("$.dataset_rules.monotonicity[{index}].column")),
                    )
                })?;
                let field = schema.field(column_index);
                Ok(MonotonicityPlan {
                    column_index,
                    column: rule.column.clone().into_boxed_str(),
                    name: rule.name.clone().into_boxed_str(),
                    direction: rule.direction,
                    nulls: rule.nulls,
                    kernel: KernelKind::from_data_type_for_plan(field.data_type()),
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let gap_detection = rules
            .gap_detection
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let path = |field: &str| Some(format!("$.dataset_rules.gap_detection[{index}].{field}"));
                if !(rule.expected_step.is_finite() && rule.expected_step > 0.0) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`expected_step` must be a finite positive number".to_string(),
                        path("expected_step"),
                    ));
                }
                if !(rule.tolerance.is_finite() && rule.tolerance >= 0.0) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`tolerance` must be a finite non-negative number".to_string(),
                        path("tolerance"),
                    ));
                }
                let column_index = schema.index_of(&rule.column).map_err(|_| {
                    ProofFrameError::contract(
                        ErrorCode::MissingColumn,
                        format!("Gap detection column `{}` is absent", rule.column),
                        path("column"),
                    )
                })?;
                let data_type = schema.field(column_index).data_type();
                let unit = step_unit(data_type).ok_or_else(|| {
                    ProofFrameError::contract(
                        ErrorCode::UnsupportedType,
                        format!(
                            "Gap detection needs an ordered numeric or temporal column; `{}` is `{data_type}`",
                            rule.column
                        ),
                        path("column"),
                    )
                })?;
                Ok(GapDetectionPlan {
                    column_index,
                    column: rule.column.clone().into_boxed_str(),
                    name: rule.name.clone().into_boxed_str(),
                    expected_step: rule.expected_step,
                    tolerance: rule.tolerance,
                    max_gaps: rule.max_gaps,
                    nulls: rule.nulls,
                    kernel: KernelKind::from_data_type_for_plan(data_type),
                    unit,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let mutually_exclusive = rules
            .mutually_exclusive
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                if rule.columns.len() < 2 {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`mutually_exclusive` needs at least two columns".to_string(),
                        Some(format!("$.dataset_rules.mutually_exclusive[{index}].columns")),
                    ));
                }
                let columns = rule
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(position, column)| {
                        schema.index_of(column).map_err(|_| {
                            ProofFrameError::contract(
                                ErrorCode::MissingColumn,
                                format!("Mutually exclusive column `{column}` is absent"),
                                Some(format!(
                                    "$.dataset_rules.mutually_exclusive[{index}].columns[{position}]"
                                )),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, ProofFrameError>>()?;
                Ok(MutuallyExclusivePlan {
                    columns,
                    names: rule.columns.iter().map(|c| c.clone().into_boxed_str()).collect(),
                    name: rule.name.clone().into_boxed_str(),
                    mode: rule.mode,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let sums = rules
            .sum
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let path = |field: &str| Some(format!("$.dataset_rules.sum[{index}].{field}"));
                let column_index = schema.index_of(&rule.column).map_err(|_| {
                    ProofFrameError::contract(
                        ErrorCode::MissingColumn,
                        format!("Sum column `{}` is absent", rule.column),
                        path("column"),
                    )
                })?;
                let data_type = schema.field(column_index).data_type();
                let kernel = KernelKind::from_data_type_for_plan(data_type);
                let bounds = sum_bounds(rule, &kernel, data_type, &path)?;
                Ok(SumPlan {
                    column_index,
                    column: rule.column.clone().into_boxed_str(),
                    name: rule.name.clone().into_boxed_str(),
                    kernel,
                    bounds,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let statistics = [
            (StatisticKind::Mean, &rules.mean, "mean"),
            (StatisticKind::StdDev, &rules.std_dev, "std_dev"),
        ]
        .into_iter()
        .flat_map(|(kind, declared, field)| {
            declared
                .iter()
                .enumerate()
                .map(move |(index, rule)| (kind, index, rule, field))
        })
        .map(|(kind, index, rule, field)| {
            let path = |part: &str| Some(format!("$.dataset_rules.{field}[{index}].{part}"));
            for (bound, part) in [(rule.min, "min"), (rule.max, "max")] {
                if bound.is_some_and(|value| !value.is_finite()) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        format!("`{part}` must be a finite number"),
                        path(part),
                    ));
                }
            }
            let column_index = schema.index_of(&rule.column).map_err(|_| {
                ProofFrameError::contract(
                    ErrorCode::MissingColumn,
                    format!("Statistic column `{}` is absent", rule.column),
                    path("column"),
                )
            })?;
            let data_type = schema.field(column_index).data_type();
            let kernel = KernelKind::from_data_type_for_plan(data_type);
            if !matches!(
                kernel,
                KernelKind::I8
                    | KernelKind::I16
                    | KernelKind::I32
                    | KernelKind::I64
                    | KernelKind::U8
                    | KernelKind::U16
                    | KernelKind::U32
                    | KernelKind::U64
                    | KernelKind::F32
                    | KernelKind::F64
            ) {
                return Err(ProofFrameError::contract(
                    ErrorCode::UnsupportedType,
                    format!(
                        "A statistic needs a numeric column; `{}` is `{data_type}`",
                        rule.column
                    ),
                    path("column"),
                ));
            }
            Ok(StatisticPlan {
                column_index,
                column: rule.column.clone().into_boxed_str(),
                name: rule.name.clone().into_boxed_str(),
                kernel,
                kind,
                min: rule.min,
                max: rule.max,
            })
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let conditional_unique = rules
            .conditional_unique
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let path = format!("$.dataset_rules.conditional_unique[{index}]");
                if rule.columns.is_empty() {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`conditional_unique` needs at least one column".to_string(),
                        Some(format!("{path}.columns")),
                    ));
                }
                let columns = rule
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(position, column)| {
                        schema.index_of(column).map_err(|_| {
                            ProofFrameError::contract(
                                ErrorCode::MissingColumn,
                                format!("Conditional key column `{column}` is absent"),
                                Some(format!("{path}.columns[{position}]")),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, ProofFrameError>>()?;
                Ok(ConditionalUniquePlan {
                    name: rule.name.clone().into_boxed_str(),
                    columns,
                    nulls: rule.nulls,
                    predicate: super::row::compile_compare(
                        &rule.when,
                        schema,
                        &format!("{path}.when"),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let row_count_delta = rules
            .row_count_delta
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let path = format!("$.dataset_rules.row_count_delta[{index}]");
                for (bound, part) in [(rule.min_ratio, "min_ratio"), (rule.max_ratio, "max_ratio")]
                {
                    if bound.is_some_and(|value| !value.is_finite()) {
                        return Err(ProofFrameError::contract(
                            ErrorCode::ContractInvalidBound,
                            format!("`{part}` must be a finite number"),
                            Some(format!("{path}.{part}")),
                        ));
                    }
                }
                if rule.against_reference.is_empty() {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`against_reference` must name a dataset".to_string(),
                        Some(format!("{path}.against_reference")),
                    ));
                }
                Ok(RowCountDeltaPlan {
                    name: rule.name.clone().into_boxed_str(),
                    reference: rule.against_reference.clone().into_boxed_str(),
                    min_ratio: rule.min_ratio,
                    max_ratio: rule.max_ratio,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let numeric = |column: &str, path: &str| -> Result<(usize, KernelKind), ProofFrameError> {
            let index = schema.index_of(column).map_err(|_| {
                ProofFrameError::contract(
                    ErrorCode::MissingColumn,
                    format!("Column `{column}` is absent"),
                    Some(path.to_string()),
                )
            })?;
            let data_type = schema.field(index).data_type();
            let kernel = KernelKind::from_data_type_for_plan(data_type);
            if !matches!(
                kernel,
                KernelKind::I8
                    | KernelKind::I16
                    | KernelKind::I32
                    | KernelKind::I64
                    | KernelKind::U8
                    | KernelKind::U16
                    | KernelKind::U32
                    | KernelKind::U64
                    | KernelKind::F32
                    | KernelKind::F64
            ) {
                return Err(ProofFrameError::contract(
                    ErrorCode::UnsupportedType,
                    format!("A balance needs numeric columns; `{column}` is `{data_type}`"),
                    Some(path.to_string()),
                ));
            }
            Ok((index, kernel))
        };
        let balance_equal = rules
            .balance_equal
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let path = format!("$.dataset_rules.balance_equal[{index}]");
                if !(rule.tolerance.is_finite() && rule.tolerance >= 0.0) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`tolerance` must be a finite non-negative number".to_string(),
                        Some(format!("{path}.tolerance")),
                    ));
                }
                let (left, left_kernel) =
                    numeric(&rule.left_column, &format!("{path}.left_column"))?;
                let (right, right_kernel) =
                    numeric(&rule.right_column, &format!("{path}.right_column"))?;
                Ok(BalanceEqualPlan {
                    name: rule.name.clone().into_boxed_str(),
                    left,
                    right,
                    left_column: rule.left_column.clone().into_boxed_str(),
                    right_column: rule.right_column.clone().into_boxed_str(),
                    left_kernel,
                    right_kernel,
                    tolerance: rule.tolerance,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let dominant_value = rules
            .max_dominant_value_ratio
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let path = format!("$.dataset_rules.max_dominant_value_ratio[{index}]");
                if !(rule.max.is_finite() && (0.0..=1.0).contains(&rule.max)) {
                    return Err(ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        "`max` must be a ratio between 0 and 1".to_string(),
                        Some(format!("{path}.max")),
                    ));
                }
                let column_index = schema.index_of(&rule.column).map_err(|_| {
                    ProofFrameError::contract(
                        ErrorCode::MissingColumn,
                        format!("Column `{}` is absent", rule.column),
                        Some(format!("{path}.column")),
                    )
                })?;
                Ok(DominantValuePlan {
                    name: rule.name.clone().into_boxed_str(),
                    column: rule.column.clone().into_boxed_str(),
                    column_index,
                    max: rule.max,
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        Ok(Self {
            row_count: rules.row_count.clone(),
            null_ratios,
            distinct_counts,
            distinct_ratios,
            composite_unique,
            references,
            monotonicity,
            gap_detection,
            mutually_exclusive,
            sums,
            statistics,
            conditional_unique,
            row_count_delta,
            balance_equal,
            dominant_value,
        })
    }

    #[must_use]
    pub fn composite_unique(&self) -> &[CompositeUniquePlan] {
        &self.composite_unique
    }
    #[must_use]
    pub fn references(&self) -> &[ReferencePlan] {
        &self.references
    }
    #[must_use]
    pub const fn row_count(&self) -> Option<&CountRangeAst> {
        self.row_count.as_ref()
    }
    #[must_use]
    pub fn null_ratios(&self) -> &[RatioPlan] {
        &self.null_ratios
    }
    #[must_use]
    pub fn distinct_counts(&self) -> &[CountPlan] {
        &self.distinct_counts
    }
    #[must_use]
    pub fn distinct_ratios(&self) -> &[RatioPlan] {
        &self.distinct_ratios
    }
}

fn ratio_plan(
    schema: &Schema,
    column: &str,
    range: &RatioRangeAst,
    rule: &str,
) -> Result<RatioPlan, ProofFrameError> {
    let column_index = schema.index_of(column).map_err(|_| {
        ProofFrameError::contract(
            ErrorCode::MissingColumn,
            format!("Dataset rule column `{column}` is absent"),
            Some(format!("$.dataset_rules.{rule}.{column}")),
        )
    })?;
    Ok(RatioPlan {
        column_index,
        column: column.to_string().into_boxed_str(),
        range: range.clone(),
        kernel: KernelKind::from_data_type_for_plan(schema.field(column_index).data_type()),
    })
}

fn count_plan(
    schema: &Schema,
    column: &str,
    range: &CountRangeAst,
) -> Result<CountPlan, ProofFrameError> {
    let column_index = schema.index_of(column).map_err(|_| {
        ProofFrameError::contract(
            ErrorCode::MissingColumn,
            format!("Distinct-count column `{column}` is absent"),
            Some(format!("$.dataset_rules.distinct_count.{column}")),
        )
    })?;
    Ok(CountPlan {
        column_index,
        column: column.to_string().into_boxed_str(),
        range: range.clone(),
        kernel: KernelKind::from_data_type_for_plan(schema.field(column_index).data_type()),
    })
}

/// The unit a step is measured in, or `None` for a column that has no distance.
///
/// Naming it is the whole point: `expected_step: 60` on a nanosecond timestamp is a
/// minute the author will never get, and silence about the unit is how that ships.
fn step_unit(data_type: &arrow::datatypes::DataType) -> Option<&'static str> {
    use arrow::datatypes::{DataType, TimeUnit};
    Some(match data_type {
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => "units",
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => "units",
        DataType::Float32 | DataType::Float64 => "units",
        DataType::Date32 => "days",
        DataType::Date64 => "milliseconds",
        DataType::Timestamp(TimeUnit::Second, _) => "seconds",
        DataType::Timestamp(TimeUnit::Millisecond, _) => "milliseconds",
        DataType::Timestamp(TimeUnit::Microsecond, _) => "microseconds",
        DataType::Timestamp(TimeUnit::Nanosecond, _) => "nanoseconds",
        _ => return None,
    })
}

/// Read the declared bounds in the column's own arithmetic.
///
/// An integer column keeps integer bounds so a limit past 2^53 survives; a float
/// column takes float bounds. Mixing them would put the contract's own numbers
/// through the rounding this rule exists to detect.
fn sum_bounds(
    rule: &super::SumAst,
    kernel: &KernelKind,
    data_type: &arrow::datatypes::DataType,
    path: &dyn Fn(&str) -> Option<String>,
) -> Result<SumBounds, ProofFrameError> {
    let integer = matches!(
        kernel,
        KernelKind::I8
            | KernelKind::I16
            | KernelKind::I32
            | KernelKind::I64
            | KernelKind::U8
            | KernelKind::U16
            | KernelKind::U32
            | KernelKind::U64
    );
    let float = matches!(kernel, KernelKind::F32 | KernelKind::F64);
    if !integer && !float {
        return Err(ProofFrameError::contract(
            ErrorCode::UnsupportedType,
            format!(
                "A total needs a numeric column; `{}` is `{data_type}`",
                rule.column
            ),
            path("column"),
        ));
    }
    // The bound keeps its source spelling; only the column's arithmetic decides how
    // it is read back.
    let spelling = |bound: Option<&super::BoundAst>| bound.map(|value| value.as_text().to_string());
    let min_text = spelling(rule.min.as_ref());
    let max_text = spelling(rule.max.as_ref());
    if integer {
        let parse = |text: Option<String>, field: &str| -> Result<Option<i128>, ProofFrameError> {
            text.map(|value| {
                value.parse::<i128>().map_err(|_| {
                    ProofFrameError::contract(
                        ErrorCode::ContractInvalidBound,
                        format!("`{value}` is not an integer total bound"),
                        path(field),
                    )
                })
            })
            .transpose()
        };
        Ok(SumBounds::Integer {
            min: parse(min_text, "min")?,
            max: parse(max_text, "max")?,
        })
    } else {
        let parse = |text: Option<String>, field: &str| -> Result<Option<f64>, ProofFrameError> {
            text.map(|value| {
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| {
                        ProofFrameError::contract(
                            ErrorCode::ContractInvalidBound,
                            format!("`{value}` is not a finite total bound"),
                            path(field),
                        )
                    })
            })
            .transpose()
        };
        Ok(SumBounds::Float {
            min: parse(min_text, "min")?,
            max: parse(max_text, "max")?,
        })
    }
}
