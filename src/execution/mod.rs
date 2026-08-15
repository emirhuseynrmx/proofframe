//! Schema-compiled, column-oriented validation execution.

mod kernels;
mod resource;

pub use resource::{
    CancellationToken, MemoryReservation, ResourceAccount, ResourceLimits, TempReservation,
};

use arrow::record_batch::RecordBatchReader;

use crate::{
    CompiledContract, ExactState, ExecutionMetrics, FastValidationReport, Finding, KernelKind,
    ProofFrameError, ValidationState, ValueKind,
};

/// Execution hints that do not alter contract semantics.
#[derive(Debug, Clone, Default)]
pub struct ExecutionOptions {
    /// Exact row count when the caller knows it without consuming the stream.
    pub row_count_hint: Option<u64>,
    pub resources: ResourceLimits,
    pub cancellation: CancellationToken,
}

/// Execute a compiled contract without reparsing or performing rule-map lookups.
pub fn execute_reader<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
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

    let resource_root = ResourceAccount::root(options.resources);
    let has_unique = plan.columns().iter().any(|column| column.rules().unique());
    let unique_directory = has_unique.then(tempfile::TempDir::new).transpose()?;
    let mut unique_states = plan
        .columns()
        .iter()
        .map(|column| {
            if !column.rules().unique() {
                return Ok(None);
            }
            let directory = unique_directory
                .as_ref()
                .expect("a unique plan owns a temporary directory");
            ExactState::new_with_cancellation(
                exact_kind(column.kernel()),
                resource_root.child(
                    options.resources.max_memory_bytes,
                    options.resources.max_temp_bytes,
                ),
                directory.path().to_path_buf(),
                options.row_count_hint,
                options.cancellation.clone(),
            )
            .map(Some)
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
    let mut rows = 0_u64;
    for maybe_batch in reader {
        options.cancellation.check()?;
        let batch = maybe_batch?;
        for (plan_index, column) in plan.columns().iter().enumerate() {
            let array = batch.column(column.column_index());
            kernels::scan_column(
                column,
                array.as_ref(),
                rows,
                &mut validation,
                unique_states[plan_index].as_mut(),
            )?;
        }
        rows += batch.num_rows() as u64;
    }

    let mut spill_bytes = 0_u64;
    let mut exact_runs = 0_u64;
    for (column, state) in plan.columns().iter().zip(unique_states) {
        let Some(state) = state else {
            continue;
        };
        let summary = state.finish()?;
        spill_bytes = spill_bytes.saturating_add(summary.metrics.spill_bytes);
        exact_runs = exact_runs.saturating_add(summary.metrics.runs);
        let sampled = summary.duplicate_samples.len() as u64;
        for duplicate in summary.duplicate_samples {
            record_lazy(
                &mut validation,
                "unique",
                column.field().name(),
                Some(duplicate.duplicate_row),
                || "Duplicate value detected".to_string(),
            );
        }
        validation.violation_count += summary.duplicate_count.saturating_sub(sampled);
    }

    let outcome = validation.finish();
    Ok(FastValidationReport {
        valid: outcome.violation_count == 0,
        violation_count: outcome.violation_count,
        truncated: outcome.truncated,
        findings: outcome.findings,
        rows,
        mode: "rules_only",
        metrics: ExecutionMetrics {
            peak_memory_bytes: resource_root.peak_memory_used(),
            peak_temp_bytes: resource_root.peak_temp_used(),
            spill_bytes,
            exact_runs,
            capacity_growth_events: 0,
        },
    })
}

fn exact_kind(kernel: &KernelKind) -> ValueKind {
    match kernel {
        KernelKind::I8
        | KernelKind::I16
        | KernelKind::I32
        | KernelKind::I64
        | KernelKind::Date32
        | KernelKind::Date64
        | KernelKind::Timestamp(_) => ValueKind::I64,
        KernelKind::U8
        | KernelKind::U16
        | KernelKind::U32
        | KernelKind::U64
        | KernelKind::Boolean => ValueKind::U64,
        KernelKind::F32 | KernelKind::F64 => ValueKind::F64,
        KernelKind::Decimal128 { .. }
        | KernelKind::Utf8
        | KernelKind::LargeUtf8
        | KernelKind::Binary
        | KernelKind::LargeBinary
        | KernelKind::Nested
        | KernelKind::NullOnly => ValueKind::Bytes,
    }
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
