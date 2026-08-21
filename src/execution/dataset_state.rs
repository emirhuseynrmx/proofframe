use arrow::array::{Array, Int64Array, LargeStringArray, StringArray, StringViewArray};
use arrow::record_batch::RecordBatch;

use super::record_lazy;
use crate::{
    CompositeNullPolicyAst, CompositeUniquePlan, CountRangeAst, DatasetPlan, ExactState,
    KernelKind, ProofFrameError, RatioPlan, RatioRangeAst, ResourceAccount, ValidationState,
    ValueKind, ValueRef,
};

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DatasetMetrics {
    pub(super) spill_bytes: u64,
    pub(super) exact_runs: u64,
}

struct CompositeState {
    plan: CompositeUniquePlan,
    exact: ExactState,
    scratch: Vec<u8>,
}

struct DistinctState {
    column_index: usize,
    column: Box<str>,
    kernel: KernelKind,
    count_range: Option<CountRangeAst>,
    ratio_range: Option<RatioRangeAst>,
    exact: ExactState,
    saw_null: bool,
}

pub(super) struct DatasetState {
    row_count: Option<crate::CountRangeAst>,
    null_ratios: Vec<RatioPlan>,
    null_counts: Vec<u64>,
    distinct: Vec<DistinctState>,
    composites: Vec<CompositeState>,
    _directory: Option<tempfile::TempDir>,
}

impl DatasetState {
    pub(super) fn new(
        plan: &DatasetPlan,
        account: &ResourceAccount,
        row_count_hint: Option<u64>,
        cancellation: &crate::CancellationToken,
    ) -> Result<Self, ProofFrameError> {
        let has_exact = !plan.composite_unique().is_empty()
            || !plan.distinct_counts().is_empty()
            || !plan.distinct_ratios().is_empty();
        let directory = has_exact.then(tempfile::TempDir::new).transpose()?;
        let mut distinct_specs = std::collections::BTreeMap::<
            usize,
            (
                Box<str>,
                KernelKind,
                Option<CountRangeAst>,
                Option<RatioRangeAst>,
            ),
        >::new();
        for count in plan.distinct_counts() {
            distinct_specs.insert(
                count.column_index(),
                (
                    count.column().to_string().into_boxed_str(),
                    count.kernel().clone(),
                    Some(count.range().clone()),
                    None,
                ),
            );
        }
        for ratio in plan.distinct_ratios() {
            distinct_specs
                .entry(ratio.column_index())
                .and_modify(|spec| spec.3 = Some(ratio.range().clone()))
                .or_insert_with(|| {
                    (
                        ratio.column().to_string().into_boxed_str(),
                        ratio.kernel().clone(),
                        None,
                        Some(ratio.range().clone()),
                    )
                });
        }
        let distinct = distinct_specs
            .into_iter()
            .map(
                |(column_index, (column, kernel, count_range, ratio_range))| {
                    let directory = directory
                        .as_ref()
                        .expect("distinct exact state owns a temporary directory");
                    Ok(DistinctState {
                        column_index,
                        column,
                        exact: ExactState::new_with_cancellation(
                            super::exact_kind(&kernel),
                            account.child(
                                account.limits().max_memory_bytes,
                                account.limits().max_temp_bytes,
                            ),
                            directory.path().to_path_buf(),
                            row_count_hint,
                            cancellation.clone(),
                        )?,
                        kernel,
                        count_range,
                        ratio_range,
                        saw_null: false,
                    })
                },
            )
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        let composites = plan
            .composite_unique()
            .iter()
            .map(|composite| {
                let directory = directory
                    .as_ref()
                    .expect("composite exact state owns a temporary directory");
                Ok(CompositeState {
                    plan: composite.clone(),
                    exact: ExactState::new_with_cancellation(
                        ValueKind::Bytes,
                        account.child(
                            account.limits().max_memory_bytes,
                            account.limits().max_temp_bytes,
                        ),
                        directory.path().to_path_buf(),
                        row_count_hint,
                        cancellation.clone(),
                    )?,
                    scratch: Vec::with_capacity(composite.columns().len().saturating_mul(24)),
                })
            })
            .collect::<Result<Vec<_>, ProofFrameError>>()?;
        Ok(Self {
            row_count: plan.row_count().cloned(),
            null_ratios: plan.null_ratios().to_vec(),
            null_counts: vec![0; plan.null_ratios().len()],
            distinct,
            composites,
            _directory: directory,
        })
    }

    pub(super) fn update(
        &mut self,
        batch: &RecordBatch,
        row_offset: u64,
        validation: &mut ValidationState,
    ) -> Result<(), ProofFrameError> {
        for (index, ratio) in self.null_ratios.iter().enumerate() {
            self.null_counts[index] = self.null_counts[index]
                .saturating_add(batch.column(ratio.column_index()).null_count() as u64);
        }
        for state in &mut self.distinct {
            let array = batch.column(state.column_index);
            for row in 0..batch.num_rows() {
                if array.is_null(row) {
                    state.saw_null = true;
                } else {
                    insert_exact_scalar(
                        &mut state.exact,
                        &state.kernel,
                        array.as_ref(),
                        row,
                        row_offset + row as u64,
                    )?;
                }
            }
        }
        for state in &mut self.composites {
            for row in 0..batch.num_rows() {
                state.scratch.clear();
                let mut rejected_null = false;
                for column_index in state.plan.columns() {
                    let array = batch.column(*column_index);
                    state
                        .scratch
                        .extend_from_slice(&(*column_index as u64).to_le_bytes());
                    if array.is_null(row) {
                        state.scratch.push(0);
                        rejected_null |= state.plan.nulls() == CompositeNullPolicyAst::Reject;
                    } else {
                        state.scratch.push(1);
                        append_scalar(array.as_ref(), row, &mut state.scratch)?;
                    }
                }
                if rejected_null {
                    record_lazy(
                        validation,
                        "composite_unique",
                        "$dataset",
                        Some(row_offset + row as u64),
                        || format!("Composite rule `{}` rejects null keys", state.plan.name()),
                    );
                } else {
                    state
                        .exact
                        .insert(ValueRef::Bytes(&state.scratch), row_offset + row as u64)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        rows: u64,
        validation: &mut ValidationState,
    ) -> Result<DatasetMetrics, ProofFrameError> {
        if let Some(range) = self.row_count.as_ref() {
            let valid = range.exact.is_none_or(|exact| rows == exact)
                && range.min.is_none_or(|min| rows >= min)
                && range.max.is_none_or(|max| rows <= max);
            if !valid {
                record_lazy(validation, "row_count", "$dataset", None, || {
                    format!("Dataset row count `{rows}` is outside the contract range")
                });
            }
        }
        for ((ratio, nulls), _) in self.null_ratios.iter().zip(self.null_counts).zip(0..) {
            let value = if rows == 0 {
                0.0
            } else {
                nulls as f64 / rows as f64
            };
            let valid = ratio.range().min.is_none_or(|min| value >= min)
                && ratio.range().max.is_none_or(|max| value <= max);
            if !valid {
                record_lazy(validation, "null_ratio", ratio.column(), None, || {
                    format!("Null ratio `{value}` is outside the contract range")
                });
            }
        }
        let mut metrics = DatasetMetrics::default();
        for state in self.distinct {
            let summary = state.exact.finish()?;
            metrics.spill_bytes = metrics
                .spill_bytes
                .saturating_add(summary.metrics.spill_bytes);
            metrics.exact_runs = metrics.exact_runs.saturating_add(summary.metrics.runs);
            let distinct = summary.distinct_count + u64::from(state.saw_null);
            if let Some(range) = state.count_range.as_ref() {
                let valid = range.exact.is_none_or(|exact| distinct == exact)
                    && range.min.is_none_or(|min| distinct >= min)
                    && range.max.is_none_or(|max| distinct <= max);
                if !valid {
                    record_lazy(validation, "distinct_count", &state.column, None, || {
                        format!("Exact distinct count `{distinct}` is outside the contract range")
                    });
                }
            }
            if let Some(range) = state.ratio_range.as_ref() {
                let ratio = if rows == 0 {
                    0.0
                } else {
                    distinct as f64 / rows as f64
                };
                let valid = range.min.is_none_or(|min| ratio >= min)
                    && range.max.is_none_or(|max| ratio <= max);
                if !valid {
                    record_lazy(validation, "distinct_ratio", &state.column, None, || {
                        format!("Exact distinct ratio `{ratio}` is outside the contract range")
                    });
                }
            }
        }
        for state in self.composites {
            let summary = state.exact.finish()?;
            metrics.spill_bytes = metrics
                .spill_bytes
                .saturating_add(summary.metrics.spill_bytes);
            metrics.exact_runs = metrics.exact_runs.saturating_add(summary.metrics.runs);
            let sampled = summary.duplicate_samples.len() as u64;
            for duplicate in summary.duplicate_samples {
                record_lazy(
                    validation,
                    "composite_unique",
                    "$dataset",
                    Some(duplicate.duplicate_row),
                    || {
                        format!(
                            "Composite rule `{}` found a duplicate key",
                            state.plan.name()
                        )
                    },
                );
            }
            validation.violation_count = validation
                .violation_count
                .saturating_add(summary.duplicate_count.saturating_sub(sampled));
        }
        Ok(metrics)
    }
}

fn insert_exact_scalar(
    exact: &mut ExactState,
    kernel: &KernelKind,
    array: &dyn Array,
    row: usize,
    global_row: u64,
) -> Result<(), ProofFrameError> {
    match kernel {
        KernelKind::I64 => exact.insert(
            ValueRef::I64(
                array
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .expect("kernel matches Arrow array")
                    .value(row),
            ),
            global_row,
        ),
        KernelKind::Utf8 => exact.insert(
            ValueRef::Bytes(
                array
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("kernel matches Arrow array")
                    .value(row)
                    .as_bytes(),
            ),
            global_row,
        ),
        _ => Err(ProofFrameError::UnsupportedType(format!(
            "exact distinct for {}",
            array.data_type()
        ))),
    }
}

fn append_scalar(
    array: &dyn Array,
    row: usize,
    output: &mut Vec<u8>,
) -> Result<(), ProofFrameError> {
    macro_rules! primitive {
        ($array:ty, $tag:literal) => {
            if let Some(values) = array.as_any().downcast_ref::<$array>() {
                output.push($tag);
                output.extend_from_slice(&values.value(row).to_le_bytes());
                return Ok(());
            }
        };
    }
    primitive!(Int64Array, 4);
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        return append_bytes(output, 19, values.value(row).as_bytes());
    }
    if let Some(values) = array.as_any().downcast_ref::<LargeStringArray>() {
        return append_bytes(output, 20, values.value(row).as_bytes());
    }
    if let Some(values) = array.as_any().downcast_ref::<StringViewArray>() {
        return append_bytes(output, 28, values.value(row).as_bytes());
    }
    Err(ProofFrameError::UnsupportedType(format!(
        "composite uniqueness for {}",
        array.data_type()
    )))
}

fn append_bytes(output: &mut Vec<u8>, tag: u8, value: &[u8]) -> Result<(), ProofFrameError> {
    output.push(tag);
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
    Ok(())
}
