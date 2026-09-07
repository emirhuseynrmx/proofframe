use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use arrow::datatypes::SchemaRef;
use arrow::error::ArrowError;
use arrow::record_batch::{RecordBatch, RecordBatchReader};

use super::{ExecutionOptions, ResourceAccount, execute_reader_inner_with_account};
use crate::encoding::V2FingerprintState;
use crate::evidence::{
    PartitionEvidenceV1, PartitionManifestV1, partition_result_digest, validation_result_digest,
};
use crate::{
    CompiledContract, ExecutionMetrics, FastValidationReport, Fingerprint, FingerprintOptions,
    FingerprintVersion, ProofFrameError,
};

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
    check_partition_readers_with_references(readers, plan, options, super::ReferenceBindings::new())
}

/// Validate ordered partitions whose reference rules resolve against `references`.
///
/// A reference rule makes the dataset plan non-empty, so the partitions are traversed as one
/// ordered sequence and every key is resolved against the whole reference dataset exactly
/// once, independently of how the data was partitioned.
pub fn check_partition_readers_with_references(
    readers: Vec<PartitionReader>,
    plan: &CompiledContract,
    options: &ExecutionOptions,
    references: super::ReferenceBindings,
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
        return super::execute_reader_with_references(
            PartitionSequence::new(readers, Arc::new(plan.schema().clone())),
            plan,
            options,
            references,
        );
    }
    // Without global state there are no reference rules, so an unused binding here is the
    // same caller mistake `prepare` rejects on the ordered path.
    if !references.is_empty() {
        return Err(ProofFrameError::contract(
            crate::ErrorCode::ReferenceUnbound,
            "No reference rule uses the bound reference datasets",
            None,
        ));
    }
    execute_independent_partitions(readers, plan, options)
}

/// Validate ordered partitions and bind each partition fingerprint into a manifest.
pub fn check_partition_readers_with_evidence(
    readers: Vec<PartitionReader>,
    plan: &CompiledContract,
    contract_source_digest: &str,
    options: &ExecutionOptions,
) -> Result<(FastValidationReport, PartitionManifestV1), ProofFrameError> {
    check_partition_readers_with_evidence_and_references(
        readers,
        plan,
        contract_source_digest,
        options,
        super::ReferenceBindings::new(),
    )
}

/// Bind partition fingerprints into a manifest while resolving reference rules.
pub fn check_partition_readers_with_evidence_and_references(
    readers: Vec<PartitionReader>,
    plan: &CompiledContract,
    contract_source_digest: &str,
    options: &ExecutionOptions,
    references: super::ReferenceBindings,
) -> Result<(FastValidationReport, PartitionManifestV1), ProofFrameError> {
    if readers.is_empty() {
        return Err(ProofFrameError::InvalidContract(
            "At least one partition is required".into(),
        ));
    }
    for reader in &readers {
        if reader.schema().as_ref() != plan.schema() {
            return Err(ProofFrameError::SchemaMismatch(
                "partition schema differs from the schema used to compile the contract".into(),
            ));
        }
    }
    validate_source_digest(contract_source_digest)?;
    let fingerprints = Arc::new(Mutex::new(
        (0..readers.len())
            .map(|_| None)
            .collect::<Vec<Option<Fingerprint>>>(),
    ));
    let sequence = FingerprintedPartitionSequence::new(
        readers,
        Arc::new(plan.schema().clone()),
        Arc::clone(&fingerprints),
    )?;
    let report = super::execute_reader_with_references(sequence, plan, options, references)?;
    let schema_digest = report.schema_digest.clone();
    let compiled_plan_digest = report.compiled_plan_digest.clone();
    let report_value = serde_json::to_value(&report)?;
    let global_result_digest = validation_result_digest(&report_value)?;
    let mut slots = fingerprints
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let partitions = slots
        .iter_mut()
        .enumerate()
        .map(|(index, slot)| {
            let fingerprint = slot.take().ok_or_else(|| {
                ProofFrameError::CorruptData(format!(
                    "partition fingerprint {index} was not finalized"
                ))
            })?;
            Ok(PartitionEvidenceV1 {
                index: index as u64,
                schema_digest: schema_digest.clone(),
                contract_source_digest: contract_source_digest.to_string(),
                compiled_plan_digest: compiled_plan_digest.clone(),
                rows: fingerprint.rows(),
                fingerprint_version: fingerprint.version(),
                fingerprint_digest: *fingerprint.digest(),
                result_digest: partition_result_digest(
                    index as u64,
                    &fingerprint,
                    &schema_digest,
                    contract_source_digest,
                    &compiled_plan_digest,
                )?,
            })
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
    let manifest = PartitionManifestV1::new(partitions, global_result_digest, options.resources)?;
    Ok((report, manifest))
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
                    // Reference rules live in the dataset plan, so `requires_global_state`
                    // has already routed them to the single ordered traversal; a partition
                    // reaching this worker never has one to resolve.
                    let result = execute_reader_inner_with_account(
                        reader,
                        plan,
                        options,
                        false,
                        account,
                        super::ReferenceBindings::new(),
                    )
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
    let mut evaluated: Vec<u64> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for report in reports {
        violation_count = violation_count.saturating_add(report.violation_count);
        if indices.len() < report.evaluated_indices.len() {
            indices = report.evaluated_indices.clone();
        }
        if evaluated.len() < report.evaluated_columns.len() {
            evaluated.resize(report.evaluated_columns.len(), 0);
        }
        for (total, part) in evaluated.iter_mut().zip(&report.evaluated_columns) {
            *total = total.saturating_add(*part);
        }
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
        references: Vec::new(),
        valid: violation_count == 0,
        violation_count,
        truncated: (findings.len() as u64) < violation_count,
        findings,
        // Partitions are scanned separately; a column is exercised when any
        // partition offered it a value, so the counts add.
        evaluated_columns: evaluated,
        evaluated_indices: indices,
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

struct FingerprintedPartitionSequence {
    readers: VecDeque<PartitionReader>,
    schema: SchemaRef,
    fingerprints: Arc<Mutex<Vec<Option<Fingerprint>>>>,
    index: usize,
    state: Option<V2FingerprintState>,
}

impl FingerprintedPartitionSequence {
    fn new(
        readers: Vec<PartitionReader>,
        schema: SchemaRef,
        fingerprints: Arc<Mutex<Vec<Option<Fingerprint>>>>,
    ) -> Result<Self, ProofFrameError> {
        let state = Some(V2FingerprintState::new(
            schema.as_ref(),
            &FingerprintOptions::new(FingerprintVersion::V2),
        )?);
        Ok(Self {
            readers: readers.into(),
            schema,
            fingerprints,
            index: 0,
            state,
        })
    }

    fn finish_current(&mut self) {
        let fingerprint = self
            .state
            .take()
            .expect("each logical partition owns a fingerprint state")
            .finish();
        self.fingerprints
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())[self.index] = Some(fingerprint);
    }
}

impl Iterator for FingerprintedPartitionSequence {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let reader = self.readers.front_mut()?;
            if let Some(batch) = reader.next() {
                return Some(batch.and_then(|batch| {
                    self.state
                        .as_mut()
                        .expect("active partition owns a fingerprint state")
                        .update(&batch)
                        .map_err(|error| ArrowError::ExternalError(Box::new(error)))?;
                    Ok(batch)
                }));
            }
            self.finish_current();
            self.readers.pop_front();
            self.index += 1;
            if !self.readers.is_empty() {
                self.state = Some(
                    V2FingerprintState::new(
                        self.schema.as_ref(),
                        &FingerprintOptions::new(FingerprintVersion::V2),
                    )
                    .expect("schema was already accepted for the first partition"),
                );
            }
        }
    }
}

impl RecordBatchReader for FingerprintedPartitionSequence {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}

fn validate_source_digest(value: &str) -> Result<(), ProofFrameError> {
    let valid = ["pf-contract-v1:", "pf-contract-v2:"].iter().any(|prefix| {
        value.strip_prefix(prefix).is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    });
    if valid {
        Ok(())
    } else {
        Err(ProofFrameError::InvalidReceipt(
            "Partition contract source digest is malformed".into(),
        ))
    }
}
