//! Presence algebra for `dataset_rules.mutually_exclusive`.
//!
//! Arrow already stores "is this value present" as one bit per row. Reading those
//! bits back one row at a time to count them would throw that away, so the rule is
//! evaluated as bitmap algebra: whole 64-bit words of rows at a time, with no branch
//! per row and no dependence on how the reader chose its batch sizes.
//!
//! Counting "two or more present" across N columns needs two accumulators and no
//! per-row counter. For each column's presence bitmap `v`:
//!
//! ```text
//! twice |= once & v;   // this column is the second one present in that row
//! once  |= v;          // this row has at least one present
//! ```
//!
//! After the last column, `twice` marks every row with two or more values and `once`
//! marks every row with at least one. Both answers cost one pass over N bitmaps.

use arrow::array::Array;
use arrow::buffer::BooleanBuffer;
use arrow::record_batch::RecordBatch;

use crate::{ExclusiveModeAst, MutuallyExclusivePlan};

/// The rows in `batch` that break `plan`, as a bitmap.
///
/// An Arrow column with no nulls carries no validity buffer at all. Reading that
/// absence as "nothing is present" would inverted the rule, so it is spelled out
/// here: no null buffer means every row is present.
pub(super) fn violations(plan: &MutuallyExclusivePlan, batch: &RecordBatch) -> BooleanBuffer {
    let rows = batch.num_rows();
    let mut once = BooleanBuffer::new_unset(rows);
    let mut twice = BooleanBuffer::new_unset(rows);

    for column_index in plan.columns() {
        let column = batch.column(*column_index);
        let present = column.nulls().map_or_else(
            || BooleanBuffer::new_set(rows),
            |nulls| nulls.inner().clone(),
        );
        twice = &twice | &(&once & &present);
        once = &once | &present;
    }

    match plan.mode() {
        ExclusiveModeAst::AtMostOne => twice,
        // An empty row fills none of the columns, which is not "exactly one" either.
        ExclusiveModeAst::ExactlyOne => &twice | &!&once,
    }
}

/// How many of the rule's columns row `row` of `batch` actually fills.
///
/// Only called for a row already known to break the rule, so the per-row cost stays
/// on the failing path rather than on the scan.
pub(super) fn filled_columns(
    plan: &MutuallyExclusivePlan,
    batch: &RecordBatch,
    row: usize,
) -> Vec<String> {
    plan.columns()
        .iter()
        .zip(plan.names())
        .filter(|(column_index, _)| !batch.column(**column_index).is_null(row))
        .map(|(_, name)| name.to_string())
        .collect()
}
