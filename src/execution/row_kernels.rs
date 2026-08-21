use arrow::array::{Array, Int64Array, StringArray};
use arrow::record_batch::RecordBatch;

use super::record_lazy;
use crate::{
    CompareOpAst, ComparePlan, NullPolicyAst, OperandPlan, ProofFrameError, RowPlan, RowPlanKind,
    ScalarValuePlan, ValidationState,
};

pub(super) fn scan_row_plan(
    plan: &RowPlan,
    batch: &RecordBatch,
    row_offset: u64,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    match plan.kind() {
        RowPlanKind::Compare(compare) => {
            scan_predicate(compare, batch, |row, outcome| {
                if matches!(outcome, Some(false)) {
                    let column = finding_column(compare);
                    record_lazy(
                        validation,
                        "compare",
                        column,
                        Some(row_offset + row as u64),
                        || format!("Relational rule `{}` failed", plan.name()),
                    );
                }
            })?;
        }
        RowPlanKind::Conditional {
            predicate,
            assertion_column,
            assertion_field,
            assertion,
        } => {
            let target = batch.column(*assertion_column);
            scan_predicate(predicate, batch, |row, outcome| {
                if matches!(outcome, Some(true)) && assertion.not_null() && target.is_null(row) {
                    record_lazy(
                        validation,
                        "conditional",
                        assertion_field.name(),
                        Some(row_offset + row as u64),
                        || format!("Conditional rule `{}` failed", plan.name()),
                    );
                }
            })?;
        }
    }
    Ok(())
}

fn scan_predicate(
    plan: &ComparePlan,
    batch: &RecordBatch,
    mut visit: impl FnMut(usize, Option<bool>),
) -> Result<(), ProofFrameError> {
    match (plan.left(), plan.right()) {
        (
            OperandPlan::Column {
                column_index: left, ..
            },
            OperandPlan::Column {
                column_index: right,
                ..
            },
        ) => {
            let left = batch.column(*left);
            let right = batch.column(*right);
            if let (Some(left), Some(right)) = (
                left.as_any().downcast_ref::<Int64Array>(),
                right.as_any().downcast_ref::<Int64Array>(),
            ) {
                for row in 0..batch.num_rows() {
                    let outcome = if left.is_null(row) || right.is_null(row) {
                        null_outcome(left.is_null(row), right.is_null(row), plan)
                    } else {
                        Some(compare(left.value(row), right.value(row), plan.op()))
                    };
                    visit(row, outcome);
                }
                return Ok(());
            }
        }
        (
            OperandPlan::Column { column_index, .. },
            OperandPlan::Literal(ScalarValuePlan::Text(literal)),
        ) => {
            let values = batch.column(*column_index);
            if let Some(values) = values.as_any().downcast_ref::<StringArray>() {
                for row in 0..batch.num_rows() {
                    let outcome = if values.is_null(row) {
                        null_outcome(true, false, plan)
                    } else {
                        Some(compare(values.value(row), literal.as_ref(), plan.op()))
                    };
                    visit(row, outcome);
                }
                return Ok(());
            }
        }
        (
            OperandPlan::Literal(ScalarValuePlan::Text(literal)),
            OperandPlan::Column { column_index, .. },
        ) => {
            let values = batch.column(*column_index);
            if let Some(values) = values.as_any().downcast_ref::<StringArray>() {
                for row in 0..batch.num_rows() {
                    let outcome = if values.is_null(row) {
                        null_outcome(false, true, plan)
                    } else {
                        Some(compare(literal.as_ref(), values.value(row), plan.op()))
                    };
                    visit(row, outcome);
                }
                return Ok(());
            }
        }
        _ => {}
    }
    Err(ProofFrameError::UnsupportedType(format!(
        "relational execution for {:?} and {:?}",
        plan.left().kernel(),
        plan.right().kernel()
    )))
}

fn null_outcome(left_null: bool, right_null: bool, plan: &ComparePlan) -> Option<bool> {
    match plan.nulls() {
        NullPolicyAst::Skip => None,
        NullPolicyAst::Fail => Some(false),
        NullPolicyAst::Equal => Some(match plan.op() {
            CompareOpAst::Eq => left_null && right_null,
            CompareOpAst::Ne => left_null != right_null,
            CompareOpAst::Lt | CompareOpAst::Lte | CompareOpAst::Gt | CompareOpAst::Gte => false,
        }),
    }
}

fn compare<T: PartialEq + PartialOrd>(left: T, right: T, op: CompareOpAst) -> bool {
    match op {
        CompareOpAst::Eq => left == right,
        CompareOpAst::Ne => left != right,
        CompareOpAst::Lt => left < right,
        CompareOpAst::Lte => left <= right,
        CompareOpAst::Gt => left > right,
        CompareOpAst::Gte => left >= right,
    }
}

fn finding_column(plan: &ComparePlan) -> &str {
    plan.left()
        .field()
        .or_else(|| plan.right().field())
        .map_or("$row", |field| field.name())
}
