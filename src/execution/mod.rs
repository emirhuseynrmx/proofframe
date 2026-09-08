//! Schema-compiled, column-oriented validation execution.

mod dataset_state;
mod exclusive;
mod kernels;
mod ordering;
mod partition;
mod references;
mod resource;
mod row_kernels;
mod total;

pub use partition::{
    PartitionReader, check_partition_readers, check_partition_readers_with_evidence,
    check_partition_readers_with_evidence_and_references, check_partition_readers_with_references,
};

pub use references::{ReferenceBindings, ReferenceOutcome};

pub use resource::{
    CancellationToken, MemoryReservation, ResourceAccount, ResourceLimits, TempReservation,
};

use arrow::record_batch::RecordBatchReader;
use std::num::NonZeroUsize;

use crate::{
    CompiledContract, ExactState, ExecutionMetrics, FastValidationReport, Finding, Fingerprint,
    FingerprintOptions, FingerprintVersion, KernelKind, ProofFrameError, SpillPolicy,
    ValidationState, ValueKind, encoding::V2FingerprintState,
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
    /// Whether exact state may spill to a temporary directory.
    ///
    /// `Never` runs uniqueness entirely in memory and fails closed when the budget
    /// cannot hold the next value. It is what a read-only filesystem, a locked-down
    /// container, or WebAssembly needs, and it never creates a directory at all.
    pub spill: SpillPolicy,
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
    execute_reader_inner(reader, plan, options, false, ReferenceBindings::new())
        .map(|(report, _)| report)
}

/// Execute a compiled contract whose reference rules resolve against `references`.
///
/// A contract with no reference rules and empty bindings behaves exactly like
/// [`execute_reader`]; any other mismatch between rules and bindings is an error.
pub fn execute_reader_with_references<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    references: ReferenceBindings,
) -> Result<FastValidationReport, ProofFrameError>
where
    R: RecordBatchReader,
{
    execute_reader_inner(reader, plan, options, false, references).map(|(report, _)| report)
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
    execute_reader_with_fingerprint_and_references(reader, plan, options, ReferenceBindings::new())
}

/// Validate, fingerprint, and resolve reference rules from the same batch traversal.
pub fn execute_reader_with_fingerprint_and_references<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    references: ReferenceBindings,
) -> Result<(FastValidationReport, Fingerprint), ProofFrameError>
where
    R: RecordBatchReader,
{
    let (report, fingerprint) = execute_reader_inner(reader, plan, options, true, references)?;
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
    references: ReferenceBindings,
) -> Result<(FastValidationReport, Option<Fingerprint>), ProofFrameError>
where
    R: RecordBatchReader,
{
    let resource_root = ResourceAccount::root(options.resources);
    execute_reader_inner_with_account(
        reader,
        plan,
        options,
        fingerprint,
        resource_root,
        references,
    )
}

pub(super) fn execute_reader_inner_with_account<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    fingerprint: bool,
    resource_root: ResourceAccount,
    references: ReferenceBindings,
) -> Result<(FastValidationReport, Option<Fingerprint>), ProofFrameError>
where
    R: RecordBatchReader,
{
    let reader_schema = reader.schema();
    validate_reader_schema(reader_schema.as_ref(), plan)?;
    let mut fingerprint = initialize_fingerprint(reader_schema.as_ref(), fingerprint)?;
    let finding_limit = plan.max_findings().min(options.resources.max_samples);
    let mut validation = ValidationState::new_accounted(finding_limit, resource_root.clone());
    validation.prepare_evaluated(plan.columns().len());
    let mut dataset_state = dataset_state::DatasetState::new(
        plan.dataset_plan(),
        &resource_root,
        options.row_count_hint,
        &options.cancellation,
        options.spill,
    )?;
    let (_unique_directory, mut unique_states) =
        initialize_unique_states(plan, options, &resource_root)?;
    // Reference datasets are scanned before the subject so that a missing binding, an
    // absent reference column, or a key type mismatch fails before any work is spent.
    // Row-count comparisons take their datasets first: only the count is needed, and
    // taking them here keeps the key-set scan below unaware of them.
    let mut references = references;
    let delta_rows = references::count_delta_references(
        plan.dataset_plan().row_count_delta(),
        &mut references,
        &options.cancellation,
    )?;
    let prepared = references::prepare(
        plan.dataset_plan().references(),
        references,
        plan.schema(),
        &resource_root,
        options.resources,
        &options.cancellation,
    )?;
    let (mut reference_state, prepared) = references::ReferenceState::new(
        prepared,
        &resource_root,
        options.resources,
        options.row_count_hint,
        &options.cancellation,
    )?;
    let rows = scan_reader(
        reader,
        plan,
        options,
        ScanState {
            fingerprint: &mut fingerprint,
            validation: &mut validation,
            unique_states: &mut unique_states,
            dataset_state: &mut dataset_state,
            reference_state: &mut reference_state,
        },
    )?;
    let mut metrics = finish_unique_states(plan, unique_states, &mut validation)?;
    let (reference_outcomes, reference_metrics) =
        reference_state.finish(prepared, finding_limit, &mut validation)?;
    metrics.spill_bytes = metrics
        .spill_bytes
        .saturating_add(reference_metrics.spill_bytes);
    metrics.exact_runs = metrics
        .exact_runs
        .saturating_add(reference_metrics.exact_runs);
    check_row_count_delta(
        plan.dataset_plan().row_count_delta(),
        rows,
        &delta_rows,
        &mut validation,
    );
    let dataset_metrics = dataset_state.finish(rows, &mut validation)?;
    metrics.spill_bytes = metrics
        .spill_bytes
        .saturating_add(dataset_metrics.spill_bytes);
    metrics.exact_runs = metrics
        .exact_runs
        .saturating_add(dataset_metrics.exact_runs);
    validation.check_resources()?;
    let report = build_report(
        plan,
        options,
        &resource_root,
        validation,
        rows,
        metrics,
        reference_outcomes,
    )?;
    Ok((report, fingerprint.map(V2FingerprintState::finish)))
}

#[derive(Debug, Clone, Copy, Default)]
struct ExactMetrics {
    spill_bytes: u64,
    exact_runs: u64,
}

fn validate_reader_schema(
    reader_schema: &arrow::datatypes::Schema,
    plan: &CompiledContract,
) -> Result<(), ProofFrameError> {
    if reader_schema == plan.schema() {
        return Ok(());
    }
    Err(ProofFrameError::SchemaMismatch(
        "reader schema differs from the schema used to compile the contract".to_string(),
    ))
}

fn initialize_fingerprint(
    schema: &arrow::datatypes::Schema,
    enabled: bool,
) -> Result<Option<V2FingerprintState>, ProofFrameError> {
    enabled
        .then(|| V2FingerprintState::new(schema, &FingerprintOptions::new(FingerprintVersion::V2)))
        .transpose()
}

/// Compare this dataset's row count with the datasets a contract named.
///
/// The ratio is `(rows - reference) / reference`. A reference with no rows has no
/// ratio at all, and saying so is the only honest answer: dividing by it would turn
/// an empty yesterday into an infinite change or a silent pass.
fn check_row_count_delta(
    plans: &[crate::RowCountDeltaPlan],
    rows: u64,
    counted: &std::collections::BTreeMap<String, u64>,
    validation: &mut ValidationState,
) {
    for plan in plans {
        let Some(&reference) = counted.get(plan.reference()) else {
            continue;
        };
        if reference == 0 {
            record_lazy(validation, "row_count_delta", "$dataset", None, || {
                format!(
                    "Rule `{}` compares against `{}`, which has no rows",
                    plan.name(),
                    plan.reference()
                )
            });
            continue;
        }
        let ratio = (rows as f64 - reference as f64) / reference as f64;
        let reason = match (plan.min_ratio(), plan.max_ratio()) {
            (Some(min), _) if ratio < min => Some(format!("below the minimum {min}")),
            (_, Some(max)) if ratio > max => Some(format!("above the maximum {max}")),
            _ => None,
        };
        if let Some(reason) = reason {
            record_lazy(validation, "row_count_delta", "$dataset", None, || {
                format!(
                    "Rule `{}` moved {ratio} against `{}` ({rows} rows against {reference}), which is {reason}",
                    plan.name(),
                    plan.reference()
                )
            });
        }
    }
}

fn initialize_unique_states(
    plan: &CompiledContract,
    options: &ExecutionOptions,
    resource_root: &ResourceAccount,
) -> Result<(Option<tempfile::TempDir>, Vec<Option<ExactState>>), ProofFrameError> {
    let has_unique = plan.columns().iter().any(|column| column.rules().unique());
    let directory = (has_unique && options.spill == SpillPolicy::Auto)
        .then(tempfile::TempDir::new)
        .transpose()?;
    let states = plan
        .columns()
        .iter()
        .map(|column| {
            if !column.rules().unique() {
                return Ok(None);
            }
            ExactState::new_with_cancellation(
                exact_kind(column.kernel()),
                resource_root.child(
                    options.resources.max_memory_bytes,
                    options.resources.max_temp_bytes,
                ),
                directory.as_ref().map(|dir| dir.path().to_path_buf()),
                options.row_count_hint,
                options.cancellation.clone(),
            )
            .map(Some)
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
    Ok((directory, states))
}

/// Everything one traversal accumulates, so the scan takes the plan, the stream, and this.
struct ScanState<'a> {
    fingerprint: &'a mut Option<V2FingerprintState>,
    validation: &'a mut ValidationState,
    unique_states: &'a mut [Option<ExactState>],
    dataset_state: &'a mut dataset_state::DatasetState,
    reference_state: &'a mut references::ReferenceState,
}

fn scan_reader<R>(
    reader: R,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    state: ScanState<'_>,
) -> Result<u64, ProofFrameError>
where
    R: RecordBatchReader,
{
    let ScanState {
        fingerprint,
        validation,
        unique_states,
        dataset_state,
        reference_state,
    } = state;
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
            // A rule that never saw a value did not pass; it was never asked. Arrow keeps
            // the null count as batch metadata, so counting what each column actually
            // offered costs one read per batch and nothing in the per-row kernels.
            validation.record_evaluated(plan_index, (array.len() - array.null_count()) as u64);
            kernels::scan_column(
                column,
                array.as_ref(),
                rows,
                validation,
                unique_states[plan_index].as_mut(),
            )?;
            validation.check_resources()?;
        }
        for row_plan in plan.row_plans() {
            row_kernels::scan_row_plan(row_plan, &batch, rows, validation)?;
            validation.check_resources()?;
        }
        dataset_state.update(&batch, rows, validation)?;
        validation.check_resources()?;
        reference_state.update(&batch, rows, validation)?;
        validation.check_resources()?;
        rows += batch.num_rows() as u64;
    }
    Ok(rows)
}

fn finish_unique_states(
    plan: &CompiledContract,
    unique_states: Vec<Option<ExactState>>,
    validation: &mut ValidationState,
) -> Result<ExactMetrics, ProofFrameError> {
    let mut metrics = ExactMetrics::default();
    for (column, state) in plan.columns().iter().zip(unique_states) {
        let Some(state) = state else {
            continue;
        };
        let summary = state.finish()?;
        metrics.spill_bytes = metrics
            .spill_bytes
            .saturating_add(summary.metrics.spill_bytes);
        metrics.exact_runs = metrics.exact_runs.saturating_add(summary.metrics.runs);
        let sampled = summary.duplicate_samples.len() as u64;
        for duplicate in summary.duplicate_samples {
            record_lazy(
                validation,
                "unique",
                column.field().name(),
                Some(duplicate.duplicate_row),
                || "Duplicate value detected".to_string(),
            );
            validation.check_resources()?;
        }
        validation.violation_count += summary.duplicate_count.saturating_sub(sampled);
    }
    Ok(metrics)
}

fn build_report(
    plan: &CompiledContract,
    options: &ExecutionOptions,
    resource_root: &ResourceAccount,
    validation: ValidationState,
    rows: u64,
    metrics: ExactMetrics,
    references: Vec<ReferenceOutcome>,
) -> Result<FastValidationReport, ProofFrameError> {
    let outcome = validation.finish();
    Ok(FastValidationReport {
        references,
        valid: outcome.violation_count == 0,
        violation_count: outcome.violation_count,
        truncated: outcome.truncated,
        findings: outcome.findings,
        // Sized from the plan before the scan, so an empty dataset still reports
        // every column as having been offered nothing.
        evaluated_columns: outcome.evaluated,
        evaluated_indices: plan
            .columns()
            .iter()
            .map(|column| column.column_index() as u32)
            .collect(),
        rows,
        mode: "rules_only",
        metrics: ExecutionMetrics {
            peak_memory_bytes: resource_root.peak_memory_used(),
            peak_temp_bytes: resource_root.peak_temp_used(),
            spill_bytes: metrics.spill_bytes,
            exact_runs: metrics.exact_runs,
            capacity_growth_events: 0,
        },
        resources: options.resources,
        compiled_plan_digest: plan.compiled_plan_digest()?,
        schema_digest: plan.schema_digest()?,
    })
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
