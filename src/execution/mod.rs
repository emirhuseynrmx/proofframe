//! Schema-compiled, column-oriented validation execution.

mod kernels;

use arrow::record_batch::RecordBatchReader;

use crate::{
    CompiledContract, FastValidationReport, Finding, ProofFrameError, UniqueState, ValidationState,
    check_unique,
};

/// Execution hints that do not alter contract semantics.
#[derive(Debug, Clone, Default)]
pub struct ExecutionOptions {
    /// Exact row count when the caller knows it without consuming the stream.
    pub row_count_hint: Option<u64>,
}

/// Execute a compiled contract without reparsing or performing rule-map lookups.
pub fn execute_reader<R>(
    reader: R,
    plan: &CompiledContract,
    _options: &ExecutionOptions,
) -> Result<FastValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    let mut validation = ValidationState::new(plan.max_findings());
    for name in plan.missing_required() {
        record_lazy(&mut validation, "required", name, None, || {
            format!("Required column `{name}` is missing")
        });
    }

    let mut unique_states = (0..plan.columns().len())
        .map(|_| None)
        .collect::<Vec<Option<UniqueState>>>();
    let mut rows = 0_u64;
    for maybe_batch in reader {
        let batch = maybe_batch?;
        for (plan_index, column) in plan.columns().iter().enumerate() {
            let array = batch.column(column.column_index());
            kernels::scan_column(column, array.as_ref(), rows, &mut validation)?;
            if column.rules().unique() {
                let state = unique_states[plan_index]
                    .get_or_insert_with(|| UniqueState::for_array(array.as_ref()));
                check_unique(
                    state,
                    array.as_ref(),
                    column.field().name(),
                    rows,
                    &mut validation,
                )?;
            }
        }
        rows += batch.num_rows() as u64;
    }

    let outcome = validation.finish();
    Ok(FastValidationReport {
        valid: outcome.violation_count == 0,
        violation_count: outcome.violation_count,
        truncated: outcome.truncated,
        findings: outcome.findings,
        rows,
        mode: "rules_only",
    })
}

pub(crate) fn record_lazy(
    validation: &mut ValidationState,
    rule: &'static str,
    column: &str,
    row: Option<u64>,
    message: impl FnOnce() -> String,
) {
    validation.violation_count += 1;
    if validation.findings.len() < validation.max_findings {
        validation.findings.push(Finding {
            rule,
            column: column.to_string(),
            row,
            message: message(),
        });
    }
}
