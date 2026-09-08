use arrow::array::{
    Array, BinaryArray, BinaryViewArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, LargeBinaryArray,
    LargeStringArray, StringArray, StringViewArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};
use arrow::record_batch::RecordBatch;
use std::cmp::Ordering;

use super::exclusive;
use super::ordering::{self, Ordered};
use super::record_lazy;
use super::total::{self, Frequencies, Moments, Total};
use crate::{
    BalanceEqualPlan, CompositeNullPolicyAst, CompositeUniquePlan, ConditionalUniquePlan,
    CountRangeAst, DatasetPlan, DominantValuePlan, ExactState, ExclusiveModeAst, GapDetectionPlan,
    KernelKind, MonotonicNullPolicyAst, MonotonicityPlan, MutuallyExclusivePlan, ProofFrameError,
    RatioPlan, RatioRangeAst, ResourceAccount, StatisticKind, StatisticPlan, SumBounds, SumPlan,
    ValidationState, ValueKind, ValueRef,
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
    ordering: Vec<OrderingState>,
    gaps: Vec<GapState>,
    exclusive: Vec<MutuallyExclusivePlan>,
    sums: Vec<SumState>,
    statistics: Vec<StatisticState>,
    conditional: Vec<ConditionalState>,
    balances: Vec<BalanceState>,
    dominants: Vec<DominantState>,
    _directory: Option<tempfile::TempDir>,
}

/// Two running totals that must meet.
struct BalanceState {
    plan: BalanceEqualPlan,
    left: Total,
    right: Total,
}

/// One column's value frequencies, bounded by the memory budget.
struct DominantState {
    plan: DominantValuePlan,
    counts: Frequencies,
    scratch: Vec<u8>,
}

/// A conditional uniqueness rule and the keys it has seen so far.
struct ConditionalState {
    plan: ConditionalUniquePlan,
    exact: ExactState,
    scratch: Vec<u8>,
}

/// One statistic rule and the moments it has accumulated.
struct StatisticState {
    plan: StatisticPlan,
    moments: Moments,
}

/// One total rule and its running accumulator.
struct SumState {
    plan: SumPlan,
    total: Total,
}

/// One step rule, the last value it saw, and how many gaps it has counted.
struct GapState {
    plan: GapDetectionPlan,
    previous: Option<Ordered>,
    gaps: u64,
    first_row: Option<u64>,
}

/// One ordering rule and the last value it accepted.
struct OrderingState {
    plan: MonotonicityPlan,
    previous: Option<Ordered>,
}

impl DatasetState {
    pub(super) fn new(
        plan: &DatasetPlan,
        account: &ResourceAccount,
        row_count_hint: Option<u64>,
        cancellation: &crate::CancellationToken,
        spill: crate::SpillPolicy,
    ) -> Result<Self, ProofFrameError> {
        let has_exact = !plan.composite_unique().is_empty()
            || !plan.distinct_counts().is_empty()
            || !plan.distinct_ratios().is_empty()
            || !plan.conditional_unique().is_empty();
        let directory = (has_exact && spill == crate::SpillPolicy::Auto)
            .then(tempfile::TempDir::new)
            .transpose()?;
        // Every rule may ask for the whole budget. A child account charges its
        // parent too, so the total is enforced where it is true, and a rule that
        // needs most of it is not refused because of rules that needed none.
        let share = account.limits().max_memory_bytes;
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
                    Ok(DistinctState {
                        column_index,
                        column,
                        exact: ExactState::new_with_cancellation(
                            super::exact_kind(&kernel),
                            account.child(share, account.limits().max_temp_bytes),
                            directory.as_ref().map(|dir| dir.path().to_path_buf()),
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
                Ok(CompositeState {
                    plan: composite.clone(),
                    exact: ExactState::new_with_cancellation(
                        ValueKind::Bytes,
                        account.child(share, account.limits().max_temp_bytes),
                        directory.as_ref().map(|dir| dir.path().to_path_buf()),
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
            ordering: plan
                .monotonicity()
                .iter()
                .map(|rule| OrderingState {
                    plan: rule.clone(),
                    previous: None,
                })
                .collect(),
            exclusive: plan.mutually_exclusive().to_vec(),
            balances: plan
                .balance_equal()
                .iter()
                .map(|rule| BalanceState {
                    plan: rule.clone(),
                    left: Total::float(),
                    right: Total::float(),
                })
                .collect(),
            dominants: plan
                .dominant_value()
                .iter()
                .map(|rule| DominantState {
                    plan: rule.clone(),
                    counts: Frequencies::new(account.limits().max_memory_bytes / 8),
                    scratch: Vec::with_capacity(64),
                })
                .collect(),
            conditional: plan
                .conditional_unique()
                .iter()
                .map(|rule| {
                    Ok(ConditionalState {
                        exact: ExactState::new_with_cancellation(
                            ValueKind::Bytes,
                            account.child(share, account.limits().max_temp_bytes),
                            directory.as_ref().map(|dir| dir.path().to_path_buf()),
                            row_count_hint,
                            cancellation.clone(),
                        )?,
                        scratch: Vec::with_capacity(rule.columns().len().saturating_mul(24)),
                        plan: rule.clone(),
                    })
                })
                .collect::<Result<Vec<_>, ProofFrameError>>()?,
            statistics: plan
                .statistics()
                .iter()
                .map(|rule| StatisticState {
                    plan: rule.clone(),
                    moments: Moments::new(),
                })
                .collect(),
            sums: plan
                .sums()
                .iter()
                .map(|rule| SumState {
                    total: match rule.bounds() {
                        SumBounds::Integer { .. } => Total::integer(),
                        SumBounds::Float { .. } => Total::float(),
                    },
                    plan: rule.clone(),
                })
                .collect(),
            gaps: plan
                .gap_detection()
                .iter()
                .map(|rule| GapState {
                    plan: rule.clone(),
                    previous: None,
                    gaps: 0,
                    first_row: None,
                })
                .collect(),
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
        for state in &mut self.ordering {
            let array = batch.column(state.plan.column_index());
            for row in 0..batch.num_rows() {
                let global_row = row_offset + row as u64;
                if array.is_null(row) {
                    // A null has no place in an order. `skip` keeps the previous value
                    // so the sequence is compared across the gap, not through it.
                    if state.plan.nulls() == MonotonicNullPolicyAst::Reject {
                        record_lazy(
                            validation,
                            "monotonicity",
                            state.plan.column(),
                            Some(global_row),
                            || format!("Rule `{}` rejects null values", state.plan.name()),
                        );
                    }
                    continue;
                }
                let value = ordering::ordered_value(state.plan.kernel(), array.as_ref(), row)?;
                if let Some(previous) = state.previous.as_ref() {
                    if !ordering::follows(state.plan.direction(), previous, &value)? {
                        record_lazy(
                            validation,
                            "monotonicity",
                            state.plan.column(),
                            Some(global_row),
                            || {
                                format!(
                                    "Rule `{}` {} but this row is out of order",
                                    state.plan.name(),
                                    ordering::describe(state.plan.direction())
                                )
                            },
                        );
                    }
                }
                state.previous = Some(value);
            }
        }
        for state in &mut self.gaps {
            let array = batch.column(state.plan.column_index());
            for row in 0..batch.num_rows() {
                let global_row = row_offset + row as u64;
                if array.is_null(row) {
                    if state.plan.nulls() == MonotonicNullPolicyAst::Reject {
                        record_lazy(
                            validation,
                            "gap_detection",
                            state.plan.column(),
                            Some(global_row),
                            || format!("Rule `{}` rejects null values", state.plan.name()),
                        );
                    }
                    continue;
                }
                let value = ordering::ordered_value(state.plan.kernel(), array.as_ref(), row)?;
                if let Some(previous) = state.previous.as_ref() {
                    let step = ordering::distance(previous, &value).ok_or_else(|| {
                        ProofFrameError::CorruptData(
                            "Gap detection compared two different value kinds".into(),
                        )
                    })?;
                    if step > state.plan.expected_step() + state.plan.tolerance() {
                        state.gaps = state.gaps.saturating_add(1);
                        if state.first_row.is_none() {
                            state.first_row = Some(global_row);
                        }
                    }
                }
                state.previous = Some(value);
            }
        }
        for state in &mut self.sums {
            let array = batch.column(state.plan.column_index());
            total::accumulate(&mut state.total, state.plan.kernel(), array.as_ref())?;
        }
        for state in &mut self.statistics {
            let array = batch.column(state.plan.column_index());
            total::observe(&mut state.moments, state.plan.kernel(), array.as_ref())?;
        }
        for state in &mut self.balances {
            total::accumulate(
                &mut state.left,
                state.plan.left_kernel(),
                batch.column(state.plan.left()).as_ref(),
            )?;
            total::accumulate(
                &mut state.right,
                state.plan.right_kernel(),
                batch.column(state.plan.right()).as_ref(),
            )?;
        }
        for state in &mut self.dominants {
            let array = batch.column(state.plan.column_index());
            for row in 0..batch.num_rows() {
                state.scratch.clear();
                if array.is_null(row) {
                    // A mostly-empty column is exactly what this rule is for, so a
                    // null counts as the value the column held.
                    state.scratch.push(0);
                } else {
                    state.scratch.push(1);
                    append_scalar(array.as_ref(), row, &mut state.scratch)?;
                }
                state.counts.add(&state.scratch)?;
            }
        }
        for state in &mut self.conditional {
            // Only the rows the predicate selects take part; the rest are not part of
            // the claim, so counting them would answer a question nobody asked.
            let mut selected = Vec::new();
            super::row_kernels::scan_predicate(state.plan.predicate(), batch, |row, outcome| {
                if outcome == Some(true) {
                    selected.push(row);
                }
            })?;
            for row in selected {
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
                        "conditional_unique",
                        "$dataset",
                        Some(row_offset + row as u64),
                        || format!("Rule `{}` rejects null keys", state.plan.name()),
                    );
                } else {
                    state
                        .exact
                        .insert(ValueRef::Bytes(&state.scratch), row_offset + row as u64)?;
                }
            }
        }
        for plan in &self.exclusive {
            let broken = exclusive::violations(plan, batch);
            for row in broken.set_indices() {
                let filled = exclusive::filled_columns(plan, batch, row);
                record_lazy(
                    validation,
                    "mutually_exclusive",
                    "$dataset",
                    Some(row_offset + row as u64),
                    || match plan.mode() {
                        ExclusiveModeAst::AtMostOne => format!(
                            "Rule `{}` allows at most one of its columns; this row fills {}",
                            plan.name(),
                            filled.join(", ")
                        ),
                        ExclusiveModeAst::ExactlyOne if filled.is_empty() => format!(
                            "Rule `{}` needs exactly one of its columns; this row fills none",
                            plan.name()
                        ),
                        ExclusiveModeAst::ExactlyOne => format!(
                            "Rule `{}` needs exactly one of its columns; this row fills {}",
                            plan.name(),
                            filled.join(", ")
                        ),
                    },
                );
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
        for state in &self.balances {
            let plan = &state.plan;
            let left = total::as_float(&state.left);
            let right = total::as_float(&state.right);
            if (left - right).abs() > plan.tolerance() {
                record_lazy(validation, "balance_equal", "$dataset", None, || {
                    format!(
                        "Rule `{}` has `{}` totalling {left} and `{}` totalling {right}",
                        plan.name(),
                        plan.left_column(),
                        plan.right_column()
                    )
                });
            }
        }
        for state in &self.dominants {
            let plan = &state.plan;
            let Some((share, rows)) = state.counts.dominant() else {
                continue;
            };
            if share > plan.max() {
                record_lazy(
                    validation,
                    "max_dominant_value_ratio",
                    plan.column(),
                    None,
                    || {
                        format!(
                            "Rule `{}` has one value in {rows} row(s), a share of {share}, above the maximum {}",
                            plan.name(),
                            plan.max()
                        )
                    },
                );
            }
        }
        for state in &self.statistics {
            let plan = &state.plan;
            let observed = match plan.kind() {
                StatisticKind::Mean => state.moments.mean(),
                StatisticKind::StdDev => state.moments.std_dev(),
            };
            // A statistic of nothing is not zero, and zero would pass bounds it
            // never earned.
            let Some(observed) = observed else {
                record_lazy(validation, plan.kind().label(), plan.column(), None, || {
                    format!(
                        "Rule `{}` had no value to take a {} from",
                        plan.name(),
                        plan.kind().label()
                    )
                });
                continue;
            };
            let reason = match (plan.min(), plan.max()) {
                (Some(min), _) if observed < min => Some(format!("below the minimum {min}")),
                (_, Some(max)) if observed > max => Some(format!("above the maximum {max}")),
                _ => None,
            };
            if let Some(reason) = reason {
                record_lazy(validation, plan.kind().label(), plan.column(), None, || {
                    format!(
                        "Rule `{}` has {} {observed} over {} value(s), which is {reason}",
                        plan.name(),
                        plan.kind().label(),
                        state.moments.count()
                    )
                });
            }
        }
        for state in &self.sums {
            let total = total::describe(&state.total);
            if let Some(reason) = total::outside(&state.total, state.plan.bounds()) {
                let plan = &state.plan;
                record_lazy(validation, "sum", plan.column(), None, || {
                    format!("Rule `{}` totals {total}, which is {reason}", plan.name())
                });
            }
        }
        for state in &self.gaps {
            if state.gaps <= state.plan.max_gaps() {
                continue;
            }
            let plan = &state.plan;
            record_lazy(
                validation,
                "gap_detection",
                plan.column(),
                state.first_row,
                || {
                    format!(
                        "Rule `{}` found {} step(s) longer than {} {} (allowed {})",
                        plan.name(),
                        state.gaps,
                        plan.expected_step(),
                        plan.unit(),
                        plan.max_gaps()
                    )
                },
            );
        }
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
        for (ratio, nulls) in self.null_ratios.iter().zip(self.null_counts) {
            let value = if rows == 0 {
                0.0
            } else {
                nulls as f64 / rows as f64
            };
            let valid =
                ratio.range().min.is_none_or(|min| {
                    compare_fraction_to_bound(nulls, rows, min) != Ordering::Less
                }) && ratio.range().max.is_none_or(|max| {
                    compare_fraction_to_bound(nulls, rows, max) != Ordering::Greater
                });
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
                let valid = range.min.is_none_or(|min| {
                    compare_fraction_to_bound(distinct, rows, min) != Ordering::Less
                }) && range.max.is_none_or(|max| {
                    compare_fraction_to_bound(distinct, rows, max) != Ordering::Greater
                });
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
        for state in self.conditional {
            let summary = state.exact.finish()?;
            metrics.spill_bytes = metrics
                .spill_bytes
                .saturating_add(summary.metrics.spill_bytes);
            metrics.exact_runs = metrics.exact_runs.saturating_add(summary.metrics.runs);
            let sampled = summary.duplicate_samples.len() as u64;
            for duplicate in summary.duplicate_samples {
                record_lazy(
                    validation,
                    "conditional_unique",
                    "$dataset",
                    Some(duplicate.duplicate_row),
                    || {
                        format!(
                            "Rule `{}` found a duplicate among the rows it selects",
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
    macro_rules! signed {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                return exact.insert(
                    ValueRef::I64(downcast::<$array>(array).value(row) as i64),
                    global_row,
                );
            }
        };
    }
    macro_rules! unsigned {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                return exact.insert(
                    ValueRef::U64(downcast::<$array>(array).value(row) as u64),
                    global_row,
                );
            }
        };
    }
    signed!(KernelKind::I8, Int8Array);
    signed!(KernelKind::I16, Int16Array);
    signed!(KernelKind::I32, Int32Array);
    signed!(KernelKind::I64, Int64Array);
    signed!(KernelKind::Date32, Date32Array);
    signed!(KernelKind::Date64, Date64Array);
    signed!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Second),
        TimestampSecondArray
    );
    signed!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Millisecond),
        TimestampMillisecondArray
    );
    signed!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Microsecond),
        TimestampMicrosecondArray
    );
    signed!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Nanosecond),
        TimestampNanosecondArray
    );
    unsigned!(KernelKind::U8, UInt8Array);
    unsigned!(KernelKind::U16, UInt16Array);
    unsigned!(KernelKind::U32, UInt32Array);
    unsigned!(KernelKind::U64, UInt64Array);
    match kernel {
        KernelKind::Boolean => exact.insert(
            ValueRef::U64(u64::from(downcast::<BooleanArray>(array).value(row))),
            global_row,
        ),
        KernelKind::F32 => exact.insert(
            ValueRef::F64(u64::from(
                downcast::<Float32Array>(array).value(row).to_bits(),
            )),
            global_row,
        ),
        KernelKind::F64 => exact.insert(
            ValueRef::F64(downcast::<Float64Array>(array).value(row).to_bits()),
            global_row,
        ),
        KernelKind::Decimal128 { .. } => {
            let encoded = downcast::<Decimal128Array>(array).value(row).to_le_bytes();
            exact.insert(ValueRef::Bytes(&encoded), global_row)
        }
        KernelKind::Utf8 => exact.insert(
            ValueRef::Bytes(downcast::<StringArray>(array).value(row).as_bytes()),
            global_row,
        ),
        KernelKind::LargeUtf8 => exact.insert(
            ValueRef::Bytes(downcast::<LargeStringArray>(array).value(row).as_bytes()),
            global_row,
        ),
        KernelKind::Utf8View => exact.insert(
            ValueRef::Bytes(downcast::<StringViewArray>(array).value(row).as_bytes()),
            global_row,
        ),
        KernelKind::Binary => exact.insert(
            ValueRef::Bytes(downcast::<BinaryArray>(array).value(row)),
            global_row,
        ),
        KernelKind::LargeBinary => exact.insert(
            ValueRef::Bytes(downcast::<LargeBinaryArray>(array).value(row)),
            global_row,
        ),
        KernelKind::BinaryView => exact.insert(
            ValueRef::Bytes(downcast::<BinaryViewArray>(array).value(row)),
            global_row,
        ),
        _ => Err(ProofFrameError::UnsupportedType(format!(
            "exact distinct for {}",
            array.data_type()
        ))),
    }
}

pub(super) fn append_scalar(
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
    primitive!(Int8Array, 1);
    primitive!(Int16Array, 2);
    primitive!(Int32Array, 3);
    primitive!(Int64Array, 4);
    primitive!(UInt8Array, 5);
    primitive!(UInt16Array, 6);
    primitive!(UInt32Array, 7);
    primitive!(UInt64Array, 8);
    primitive!(Float32Array, 9);
    primitive!(Float64Array, 10);
    primitive!(Date32Array, 11);
    primitive!(Date64Array, 12);
    primitive!(TimestampSecondArray, 13);
    primitive!(TimestampMillisecondArray, 14);
    primitive!(TimestampMicrosecondArray, 15);
    primitive!(TimestampNanosecondArray, 16);
    primitive!(Decimal128Array, 17);
    if let Some(values) = array.as_any().downcast_ref::<BooleanArray>() {
        output.extend_from_slice(&[18, u8::from(values.value(row))]);
        return Ok(());
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        return append_bytes(output, 19, values.value(row).as_bytes());
    }
    if let Some(values) = array.as_any().downcast_ref::<LargeStringArray>() {
        return append_bytes(output, 20, values.value(row).as_bytes());
    }
    if let Some(values) = array.as_any().downcast_ref::<StringViewArray>() {
        return append_bytes(output, 28, values.value(row).as_bytes());
    }
    if let Some(values) = array.as_any().downcast_ref::<BinaryArray>() {
        return append_bytes(output, 21, values.value(row));
    }
    if let Some(values) = array.as_any().downcast_ref::<LargeBinaryArray>() {
        return append_bytes(output, 22, values.value(row));
    }
    if let Some(values) = array.as_any().downcast_ref::<BinaryViewArray>() {
        return append_bytes(output, 29, values.value(row));
    }
    Err(ProofFrameError::UnsupportedType(format!(
        "composite uniqueness for {}",
        array.data_type()
    )))
}

pub(super) fn downcast<T: 'static>(array: &dyn Array) -> &T {
    array
        .as_any()
        .downcast_ref::<T>()
        .expect("kernel matches the compiled Arrow array")
}

fn append_bytes(output: &mut Vec<u8>, tag: u8, value: &[u8]) -> Result<(), ProofFrameError> {
    output.push(tag);
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn compare_fraction_to_bound(numerator: u64, denominator: u64, bound: f64) -> Ordering {
    if denominator == 0 {
        return 0.0_f64.total_cmp(&bound);
    }
    if bound <= 0.0 {
        return numerator.cmp(&0);
    }
    if bound >= 1.0 {
        return numerator.cmp(&denominator);
    }

    let bits = bound.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    if exponent_bits == 0 {
        return if numerator == 0 {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }

    let mut bound_numerator = (1_u64 << 52) | (bits & ((1_u64 << 52) - 1));
    let mut denominator_shift = (1075 - exponent_bits) as u32;
    let cancelled = bound_numerator.trailing_zeros().min(denominator_shift);
    bound_numerator >>= cancelled;
    denominator_shift -= cancelled;

    if denominator_shift >= u128::BITS {
        return if numerator == 0 {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let bound_denominator = 1_u128 << denominator_shift;
    let Some(left) = u128::from(numerator).checked_mul(bound_denominator) else {
        return Ordering::Greater;
    };
    let right = u128::from(denominator) * u128::from(bound_numerator);
    left.cmp(&right)
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::compare_fraction_to_bound;

    #[test]
    fn ratio_comparison_does_not_round_large_integer_counts() {
        let rows = u64::MAX;
        let count = rows / 2 + 1;

        assert_eq!(
            compare_fraction_to_bound(count, rows, 0.5),
            Ordering::Greater
        );
        assert_eq!(
            compare_fraction_to_bound(0, rows, f64::MIN_POSITIVE),
            Ordering::Less
        );
        assert_eq!(
            compare_fraction_to_bound(1, rows, f64::MIN_POSITIVE),
            Ordering::Greater
        );
    }
}
