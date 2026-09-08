mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, TimestampMicrosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use proofframe::{
    CompiledContract, ContractAst, ErrorCode, ExactState, ExecutionOptions, ResourceAccount,
    ResourceLimits, ValueKind, ValueRef, execute_reader,
};
use tempfile::TempDir;

use support::reader_from_batches;

/// A batch of text columns, built from a generator over (column, row).
///
/// Every test about running without a filesystem needs the same subject: text
/// columns, because text is what the byte-arena path handles, and a shape the
/// caller chooses per column so one can be wide and the rest narrow.
fn text_batch(columns: usize, rows: usize, value: impl Fn(usize, usize) -> String) -> RecordBatch {
    use arrow::array::StringArray;

    let fields = (0..columns)
        .map(|column| Field::new(format!("c{column}"), DataType::Utf8, true))
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new(fields));
    let arrays = (0..columns)
        .map(|column| {
            let values = (0..rows).map(|row| value(column, row)).collect::<Vec<_>>();
            Arc::new(StringArray::from(values)) as ArrayRef
        })
        .collect::<Vec<_>>();
    RecordBatch::try_new(schema, arrays).expect("the arrays match the schema they came from")
}

/// Infer a contract from `batch` with uniqueness on and nowhere to spill.
fn suggest_without_spill(batch: RecordBatch) -> Result<(), proofframe::ProofFrameError> {
    use proofframe::{SpillPolicy, SuggestOptions, suggest_reader_with_options};

    let options = SuggestOptions {
        infer_uniqueness: true,
        spill: SpillPolicy::Never,
        ..SuggestOptions::default()
    };
    suggest_reader_with_options(reader_from_batches(vec![batch]), options, None).map(|_| ())
}

fn limits(memory: u64, temp: u64, samples: usize) -> ResourceLimits {
    ResourceLimits {
        max_memory_bytes: memory,
        max_temp_bytes: temp,
        max_output_records: 100_000,
        max_samples: samples,
    }
}

#[test]
fn fixed_width_state_spills_and_finds_cross_run_duplicates_exactly() {
    let directory = TempDir::new().unwrap();
    let account = ResourceAccount::root(limits(32 * 1024, 8 * 1024 * 1024, 8));
    let mut state = ExactState::new(
        ValueKind::I64,
        account.clone(),
        directory.path().to_path_buf(),
        Some(100_001),
    )
    .unwrap();
    for row in 0..100_000_u64 {
        state.insert(ValueRef::I64(row as i64), row).unwrap();
    }
    state.insert(ValueRef::I64(7), 100_000).unwrap();

    let summary = state.finish().unwrap();

    assert_eq!(summary.distinct_count, 100_000);
    assert_eq!(summary.duplicate_count, 1);
    assert_eq!(summary.duplicate_samples[0].first_row, 7);
    assert_eq!(summary.duplicate_samples[0].duplicate_row, 100_000);
    assert!(summary.metrics.runs > 1);
    assert!(summary.metrics.spill_bytes > 0);
    assert!(summary.metrics.peak_memory_bytes <= 32 * 1024);
}

#[test]
fn exact_runs_are_compacted_with_bounded_merge_fan_in() {
    let directory = TempDir::new().unwrap();
    let account = ResourceAccount::root(limits(16 * 1024, 64 * 1024 * 1024, 0));
    let mut state = ExactState::new(
        ValueKind::I64,
        account,
        directory.path().to_path_buf(),
        Some(50_001),
    )
    .unwrap();
    for row in 0..50_000_u64 {
        state.insert(ValueRef::I64(row as i64), row).unwrap();
    }
    state.insert(ValueRef::I64(17), 50_000).unwrap();

    let summary = state.finish().unwrap();

    assert_eq!(summary.distinct_count, 50_000);
    assert_eq!(summary.duplicate_count, 1);
    assert!(summary.metrics.compactions > 0);
    assert!(summary.metrics.max_merge_fan_in <= 32);
    assert!(summary.metrics.runs <= 32);
}

#[test]
fn float_and_byte_equality_use_full_canonical_values() {
    let directory = TempDir::new().unwrap();
    let account = ResourceAccount::root(limits(4 * 1024, 1024 * 1024, 8));
    let nan_a = 0x7ff8_0000_0000_0001_u64;
    let nan_b = 0x7ff8_0000_0000_0002_u64;
    let mut floats = ExactState::new(
        ValueKind::F64,
        account.clone(),
        directory.path().to_path_buf(),
        None,
    )
    .unwrap();
    for (row, bits) in [(-0.0_f64).to_bits(), 0.0_f64.to_bits(), nan_a, nan_b, nan_a]
        .into_iter()
        .enumerate()
    {
        floats.insert(ValueRef::F64(bits), row as u64).unwrap();
    }
    let float_summary = floats.finish().unwrap();
    assert_eq!(float_summary.distinct_count, 4);
    assert_eq!(float_summary.duplicate_count, 1);
    assert_eq!(float_summary.duplicate_samples[0].duplicate_row, 4);

    let mut bytes = ExactState::new(
        ValueKind::Bytes,
        account,
        directory.path().to_path_buf(),
        None,
    )
    .unwrap();
    bytes.insert(ValueRef::Bytes(b"left"), 0).unwrap();
    bytes.insert(ValueRef::Bytes(b"right"), 1).unwrap();
    bytes.insert(ValueRef::Bytes(b"left"), 2).unwrap();
    let byte_summary = bytes.finish().unwrap();
    assert_eq!(byte_summary.distinct_count, 2);
    assert_eq!(byte_summary.duplicate_count, 1);
    assert_eq!(byte_summary.duplicate_samples[0].first_row, 0);
    assert_eq!(byte_summary.duplicate_samples[0].duplicate_row, 2);
}

#[test]
fn spill_is_refused_before_exceeding_the_temp_budget() {
    let directory = TempDir::new().unwrap();
    let account = ResourceAccount::root(limits(256, 0, 1));
    let mut state = ExactState::new(
        ValueKind::I64,
        account,
        directory.path().to_path_buf(),
        None,
    )
    .unwrap();

    let error = (0..10_000_u64)
        .find_map(|row| state.insert(ValueRef::I64(row as i64), row).err())
        .expect("the fixed segment must eventually require a spill");

    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert_eq!(directory.path().read_dir().unwrap().count(), 0);
}

#[test]
fn compiled_unique_execution_uses_the_bounded_exact_state() {
    let rows = 20_000_u64;
    let schema = Arc::new(Schema::new(vec![Field::new(
        "ts",
        DataType::Timestamp(TimeUnit::Microsecond, None),
        false,
    )]));
    let mut values = (0..rows as i64).collect::<Vec<_>>();
    values.push(17);
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(TimestampMicrosecondArray::from(values)) as ArrayRef],
    )
    .unwrap();
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"ts":{"unique":true}},"max_findings":4}"#,
    )
    .unwrap();
    let plan = CompiledContract::compile(&ast, schema.as_ref()).unwrap();

    let report = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions {
            row_count_hint: Some(rows + 1),
            resources: limits(32 * 1024, 8 * 1024 * 1024, 4),
            ..ExecutionOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].rule, "unique");
    assert_eq!(report.findings[0].row, Some(rows));
    assert!(report.metrics.exact_runs > 1);
    assert!(report.metrics.spill_bytes > 0);
    assert!(report.metrics.peak_memory_bytes <= 32 * 1024);
    assert!(report.metrics.peak_temp_bytes <= 8 * 1024 * 1024);
    assert_eq!(report.metrics.capacity_growth_events, 0);
}

#[test]
fn compiled_unique_execution_honors_the_temp_budget() {
    let schema = Arc::new(Schema::new(vec![Field::new("ts", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(arrow::array::Int64Array::from_iter_values(0..10_000_i64)) as ArrayRef],
    )
    .unwrap();
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"ts":{"unique":true}}}"#,
    )
    .unwrap();
    let plan = CompiledContract::compile(&ast, schema.as_ref()).unwrap();

    let error = execute_reader(
        reader_from_batches(vec![batch]),
        &plan,
        &ExecutionOptions {
            resources: limits(256, 0, 1),
            ..ExecutionOptions::default()
        },
    )
    .unwrap_err();

    assert_eq!(error.code(), ErrorCode::ResourceLimit);
}

/// Uniqueness without a filesystem: the answer stays exact, or there is no answer.
///
/// WebAssembly has no filesystem, and a read-only container has no writable one.
/// Requiring a temporary directory to check three rows for duplicates was a
/// portability bug, not a safety property. What must not change is what happens
/// when the budget runs out: the scan stops and says so.
#[test]
fn exact_state_without_a_spill_target_stays_exact_or_stops() {
    let account = ResourceAccount::root(limits(4 * 1024 * 1024, 0, 8));
    let mut state = ExactState::new(ValueKind::I64, account, None, None).unwrap();
    for row in 0..50_000u64 {
        state.insert(ValueRef::I64(row as i64), row).unwrap();
    }
    state.insert(ValueRef::I64(7), 50_000).unwrap();
    let summary = state.finish().unwrap();
    assert_eq!(summary.metrics.spill_bytes, 0, "nothing may be written");
    assert_eq!(summary.duplicate_count, 1);
    assert_eq!(summary.distinct_count, 50_000);

    // A budget too small to hold the data is a resource limit, not a wrong answer.
    let cramped = ResourceAccount::root(limits(8 * 1024, 0, 8));
    let mut state = ExactState::new(ValueKind::I64, cramped, None, None).unwrap();
    let mut refused = None;
    for row in 0..200_000u64 {
        if let Err(error) = state.insert(ValueRef::I64(row as i64), row) {
            refused = Some(error);
            break;
        }
    }
    let error = refused.expect("an unbounded scan on a tiny budget must stop");
    assert_eq!(error.code(), ErrorCode::ResourceLimit, "{error}");
}

/// Many columns with nowhere to spill must all get through.
///
/// Each column gets its own exact state. Rationing the budget between them was one
/// way to stop the first from taking what the rest needed, and it bought that at
/// the price of refusing any column that needed more than its slice: six columns
/// meant a sixth of the budget each, and a scan died holding 67 MB with 512 MB
/// allowed. The states now compete for the whole budget, which the root account
/// enforces, and a segment that fills grows rather than ending the scan.
#[test]
fn many_columns_without_a_spill_target_all_get_through() {
    for columns in [1usize, 12, 40, 80] {
        let batch = text_batch(columns, 64, |column, row| format!("v{row}_{column}"));
        suggest_without_spill(batch)
            .unwrap_or_else(|error| panic!("{columns} columns without spill: {error}"));
    }
}

/// A full segment with nowhere to spill is a reason to grow, not to stop.
///
/// The segment size decides when to write a run. With no run to write it decided
/// nothing and still ended the scan: a column of a million values refused the next
/// one while five hundred megabytes of its budget sat unused, and said so with
/// numbers that described neither. The budget is now the only thing that stops it.
#[test]
fn a_full_segment_without_a_spill_target_grows_instead_of_refusing() {
    let account = ResourceAccount::root(limits(64 * 1024 * 1024, 0, 8));
    let mut state = ExactState::new(ValueKind::I64, account, None, None).unwrap();
    // Well past any single segment the constructor would have chosen.
    for row in 0..600_000u64 {
        state
            .insert(ValueRef::I64(row as i64), row)
            .unwrap_or_else(|error| panic!("refused at row {row}: {error}"));
    }
    let summary = state.finish().unwrap();
    assert_eq!(summary.distinct_count, 600_000);
    assert_eq!(summary.metrics.spill_bytes, 0, "nothing may be written");
}

/// The same for text, where the index and the arena fill up separately.
#[test]
fn a_growing_text_segment_keeps_both_halves_in_step() {
    let account = ResourceAccount::root(limits(64 * 1024 * 1024, 0, 8));
    let mut state = ExactState::new(ValueKind::Bytes, account, None, None).unwrap();
    for row in 0..200_000u64 {
        // Lengths vary so the arena and the index do not fill at the same moment.
        let value = "x".repeat(1 + (row % 37) as usize);
        state
            .insert(ValueRef::Bytes(value.as_bytes()), row)
            .unwrap_or_else(|error| panic!("refused at row {row}: {error}"));
    }
    let summary = state.finish().unwrap();
    assert_eq!(summary.distinct_count, 37);
    assert_eq!(summary.duplicate_count, 200_000 - 37);
}

/// One demanding column must be able to use what the others left alone.
///
/// The budget used to be divided by the number of columns before the scan began,
/// so a file with one high-cardinality column and five narrow ones refused the
/// wide one at a sixth of the budget while the other five sixths sat untouched.
#[test]
fn one_wide_column_may_use_the_budget_the_others_did_not() {
    // Five columns holding one value each, and one holding a different value per
    // row. Only the sixth needs any room at all.
    let batch = text_batch(6, 120_000, |column, row| {
        if column == 5 {
            format!("key-{row:012}")
        } else {
            "same".to_string()
        }
    });
    suggest_without_spill(batch)
        .expect("the wide column must be allowed the room the narrow ones did not take");
}

/// A profile describes; it does not decide.
///
/// One column holding more distinct values than the budget can keep used to end
/// the whole scan, so a ten-million-row file returned no types, no null counts and
/// no ranges either. The count is now dropped for that column alone, and the
/// profile says it was dropped rather than leaving an empty cell to interpret.
#[test]
fn a_column_too_wide_to_count_loses_its_count_not_the_profile() {
    use proofframe::{DistinctMode, ResourceLimits, profile_reader_in_memory};

    // c0 carries a different two-hundred-byte value per row; c1 carries five.
    let rows = 200_000;
    let batch = text_batch(2, rows, |column, row| {
        if column == 0 {
            format!("{row:0>200}")
        } else {
            format!("n{}", row % 5)
        }
    });

    // The exact state keeps every occurrence, not every distinct value, so the
    // narrow column still needs room for 200,000 short records — about 4 MB. The
    // wide one needs ten times that and cannot have it.
    let resources = ResourceLimits {
        max_memory_bytes: 24 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let profile = profile_reader_in_memory(
        reader_from_batches(vec![batch]),
        DistinctMode::Exact,
        resources,
    )
    .expect("one uncountable column must not take the profile with it");

    assert_eq!(profile.rows, rows as u64);
    let wide = &profile.columns[0];
    assert!(wide.distinct_limited, "the wide column gave up its count");
    assert_eq!(wide.distinct_count, None);
    // Everything the scan could still say, it still says.
    assert_eq!(wide.non_null_count, rows as u64);
    assert_eq!(wide.null_count, 0);

    let narrow = &profile.columns[1];
    assert!(!narrow.distinct_limited, "the narrow column kept counting");
    assert_eq!(narrow.distinct_count, Some(5));
}
