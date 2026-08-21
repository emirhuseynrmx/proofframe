//! Schema-compiled, column-oriented validation execution.

mod dataset_state;
mod kernels;
mod partition;
mod resource;
mod row_kernels;

pub use partition::{PartitionReader, check_partition_readers};

pub use resource::{
    CancellationToken, MemoryReservation, ResourceAccount, ResourceLimits, TempReservation,
};

use arrow::record_batch::RecordBatchReader;
use std::num::NonZeroUsize;

use crate::{
    CompiledContract, ExactState, ExecutionMetrics, FastValidationReport, Finding, Fingerprint,
    FingerprintOptions, FingerprintVersion, KernelKind, ProofFrameError, ValidationState,
    ValueKind, encoding::V2FingerprintState,
};

/// Execution hints that do not alter contract semantics.
#[derive(Debug, Clone, Default)]
pub struct ExecutionOptions {
    /// Exact row count when the caller knows it without consuming the stream.
    pub row_count_hint: Option<u64>,
    pub resources: ResourceLimits,
    pub cancellation: CancellationToken,
    /// Maximum partition workers. `None` uses bounded host parallelism.
    pub threads: Option<NonZeroUsize>,
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
    execute_reader_inner(reader, plan, options, false).map(|(report, _)| report)
}

/// Execute validation and compute the V2 dataset fingerprint from the same batch traversal.
pub fn execute_reader_with_fingerprint<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
) -> Result<(FastValidationReport, Fingerprint), ProofFrameError>
where
    R: RecordBatchReader,
{
    let (report, fingerprint) = execute_reader_inner(reader, plan, options, true)?;
    fingerprint
        .map(|fingerprint| (report, fingerprint))
        .ok_or_else(|| {
            ProofFrameError::InvalidReceipt("V2 fingerprint state was not created".into())
        })
}

fn execute_reader_inner<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    fingerprint: bool,
) -> Result<(FastValidationReport, Option<Fingerprint>), ProofFrameError>
where
    R: RecordBatchReader,
{
    let resource_root = ResourceAccount::root(options.resources);
    execute_reader_inner_with_account(reader, plan, options, fingerprint, resource_root)
}

pub(super) fn execute_reader_inner_with_account<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    fingerprint: bool,
    resource_root: ResourceAccount,
) -> Result<(FastValidationReport, Option<Fingerprint>), ProofFrameError>
where
    R: RecordBatchReader,
{
    let reader_schema = reader.schema();
    if reader_schema.as_ref() != plan.schema() {
        return Err(ProofFrameError::SchemaMismatch(
            "reader schema differs from the schema used to compile the contract".to_string(),
        ));
    }
    let mut fingerprint = fingerprint
        .then(|| {
            V2FingerprintState::new(
                reader_schema.as_ref(),
                &FingerprintOptions::new(FingerprintVersion::V2),
            )
        })
        .transpose()?;
    let finding_limit = plan.max_findings().min(options.resources.max_samples);
    let mut validation = ValidationState::new_accounted(finding_limit, resource_root.clone());
    let mut dataset_state = dataset_state::DatasetState::new(
        plan.dataset_plan(),
        &resource_root,
        options.row_count_hint,
        &options.cancellation,
    )?;
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
        if batch.schema().as_ref() != plan.schema() {
            return Err(ProofFrameError::SchemaMismatch(
                "record batch schema changed after contract compilation".to_string(),
            ));
        }
        if let Some(state) = fingerprint.as_mut() {
            state.update(&batch)?;
        }
        for (plan_index, column) in plan.columns().iter().enumerate() {
            let array = batch.column(column.column_index());
            kernels::scan_column(
                column,
                array.as_ref(),
                rows,
                &mut validation,
                unique_states[plan_index].as_mut(),
            )?;
            validation.check_resources()?;
        }
        for row_plan in plan.row_plans() {
            row_kernels::scan_row_plan(row_plan, &batch, rows, &mut validation)?;
            validation.check_resources()?;
        }
        dataset_state.update(&batch, rows, &mut validation)?;
        validation.check_resources()?;
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
            validation.check_resources()?;
        }
        validation.violation_count += summary.duplicate_count.saturating_sub(sampled);
    }

    let dataset_metrics = dataset_state.finish(rows, &mut validation)?;
    spill_bytes = spill_bytes.saturating_add(dataset_metrics.spill_bytes);
    exact_runs = exact_runs.saturating_add(dataset_metrics.exact_runs);
    validation.check_resources()?;

    let outcome = validation.finish();
    let report = FastValidationReport {
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
        resources: options.resources,
        compiled_plan_digest: plan.compiled_plan_digest()?,
        schema_digest: plan.schema_digest()?,
    };
    Ok((report, fingerprint.map(V2FingerprintState::finish)))
}

pub(super) fn exact_kind(kernel: &KernelKind) -> ValueKind {
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
        | KernelKind::Utf8View
        | KernelKind::Binary
        | KernelKind::LargeBinary
        | KernelKind::BinaryView
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
    if validation.findings.len() < validation.max_findings {
        validation.record(Finding {
            rule,
            column: column.to_string(),
            row,
            message: message(),
        });
    } else {
        validation.violation_count += 1;
    }
}
