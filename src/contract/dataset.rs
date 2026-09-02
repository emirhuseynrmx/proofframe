use arrow::datatypes::{FieldRef, Schema};

use super::{
    CompositeNullPolicyAst, ContractAstV2, CountRangeAst, RatioRangeAst, ReferenceNullPolicyAst,
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

#[derive(Debug, Clone, Default)]
pub struct DatasetPlan {
    pub(crate) row_count: Option<CountRangeAst>,
    pub(crate) null_ratios: Vec<RatioPlan>,
    pub(crate) distinct_counts: Vec<CountPlan>,
    pub(crate) distinct_ratios: Vec<RatioPlan>,
    pub(crate) composite_unique: Vec<CompositeUniquePlan>,
    pub(crate) references: Vec<ReferencePlan>,
}

impl DatasetPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.row_count.is_none()
            && self.null_ratios.is_empty()
            && self.distinct_counts.is_empty()
            && self.distinct_ratios.is_empty()
            && self.composite_unique.is_empty()
            && self.references.is_empty()
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
        Ok(Self {
            row_count: rules.row_count.clone(),
            null_ratios,
            distinct_counts,
            distinct_ratios,
            composite_unique,
            references,
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
