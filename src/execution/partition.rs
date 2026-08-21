use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use arrow::datatypes::SchemaRef;
use arrow::error::ArrowError;
use arrow::record_batch::{RecordBatch, RecordBatchReader};

use super::{ExecutionOptions, ResourceAccount, execute_reader, execute_reader_inner_with_account};
use crate::{CompiledContract, ExecutionMetrics, FastValidationReport, ProofFrameError};

/// One independently consumable Arrow stream in a logical dataset partition.
pub type PartitionReader = Box<dyn RecordBatchReader + Send>;

/// Validate ordered Arrow partitions with bounded native workers.
///
/// Rules with global state (uniqueness and dataset rules) deliberately use one
/// ordered traversal so exact semantics never depend on the worker count.
pub fn check_partition_readers(
    readers: Vec<PartitionReader>,
    plan: &CompiledContract,
    options: &ExecutionOptions,
) -> Result<FastValidationReport, ProofFrameError> {
    options.cancellation.check()?;
    for reader in &readers {
        if reader.schema().as_ref() != plan.schema() {
            return Err(ProofFrameError::SchemaMismatch(
                "partition schema differs from the schema used to compile the contract".into(),
            ));
        }
    }
    if readers.is_empty() || requires_global_state(plan) {
        return execute_reader(
            PartitionSequence::new(readers, Arc::new(plan.schema().clone())),
            plan,
            options,
        );
    }
    execute_independent_partitions(readers, plan, options)
}

fn requires_global_state(plan: &CompiledContract) -> bool {
    !plan.dataset_plan().is_empty() || plan.columns().iter().any(|column| column.rules().unique())
}

fn execute_independent_partitions(
    readers: Vec<PartitionReader>,
    plan: &CompiledContract,
    options: &ExecutionOptions,
) -> Result<FastValidationReport, ProofFrameError> {
    let partition_count = readers.len();
    let requested = options.threads.map_or_else(
        || std::thread::available_parallelism().map_or(1, usize::from),
        usize::from,
    );
    let worker_count = requested.clamp(1, partition_count);
    let queue = Arc::new(Mutex::new(
        readers.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let results = Arc::new(Mutex::new(
        (0..partition_count)
            .map(|_| None)
            .collect::<Vec<Option<Result<FastValidationReport, ProofFrameError>>>>(),
    ));
    let root = ResourceAccount::root(options.resources);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let root = root.clone();
            scope.spawn(move || {
                loop {
                    let work = queue
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .pop_front();
                    let Some((index, reader)) = work else {
                        break;
                    };
                    let account = root.child(
                        options.resources.max_memory_bytes,
                        options.resources.max_temp_bytes,
                    );
                    let result =
                        execute_reader_inner_with_account(reader, plan, options, false, account)
                            .map(|(report, _)| report);
                    results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())[index] = Some(result);
                }
            });
        }
    });

    let mut slots = results
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let reports = slots
        .iter_mut()
        .enumerate()
        .map(|(index, slot)| {
            slot.take().ok_or_else(|| {
                ProofFrameError::CorruptData(format!(
                    "partition worker did not publish logical result {index}"
                ))
            })?
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
    merge_reports(reports, plan, options)
}

fn merge_reports(
    reports: Vec<FastValidationReport>,
    plan: &CompiledContract,
    options: &ExecutionOptions,
) -> Result<FastValidationReport, ProofFrameError> {
    let finding_limit = plan.max_findings().min(options.resources.max_samples);
    let mut findings = Vec::with_capacity(finding_limit);
    let mut row_offset = 0_u64;
    let mut violation_count = 0_u64;
    let mut metrics = ExecutionMetrics::default();
    for report in reports {
        violation_count = violation_count.saturating_add(report.violation_count);
        for mut finding in report.findings {
            if findings.len() == finding_limit {
                break;
            }
            finding.row = finding.row.map(|row| row_offset.saturating_add(row));
            findings.push(finding);
        }
        row_offset = row_offset.saturating_add(report.rows);
        metrics.peak_memory_bytes = metrics
            .peak_memory_bytes
            .max(report.metrics.peak_memory_bytes);
        metrics.peak_temp_bytes = metrics.peak_temp_bytes.max(report.metrics.peak_temp_bytes);
        metrics.spill_bytes = metrics
            .spill_bytes
            .saturating_add(report.metrics.spill_bytes);
        metrics.exact_runs = metrics.exact_runs.saturating_add(report.metrics.exact_runs);
        metrics.capacity_growth_events = metrics
            .capacity_growth_events
            .saturating_add(report.metrics.capacity_growth_events);
    }
    Ok(FastValidationReport {
        valid: violation_count == 0,
        violation_count,
        truncated: (findings.len() as u64) < violation_count,
        findings,
        rows: row_offset,
        mode: "rules_only",
        metrics,
        resources: options.resources,
        compiled_plan_digest: plan.compiled_plan_digest()?,
        schema_digest: plan.schema_digest()?,
    })
}

struct PartitionSequence {
    readers: VecDeque<PartitionReader>,
    schema: SchemaRef,
}

impl PartitionSequence {
    fn new(readers: Vec<PartitionReader>, schema: SchemaRef) -> Self {
        Self {
            readers: readers.into(),
            schema,
        }
    }
}

impl Iterator for PartitionSequence {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let reader = self.readers.front_mut()?;
            if let Some(batch) = reader.next() {
                return Some(batch);
            }
            self.readers.pop_front();
        }
    }
}

impl RecordBatchReader for PartitionSequence {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}
