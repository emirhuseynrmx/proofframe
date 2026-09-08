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
