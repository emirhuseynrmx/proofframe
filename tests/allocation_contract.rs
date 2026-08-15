mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    CompiledContract, ContractAst, ExecutionOptions, execute_reader, fingerprint_reader,
    scan_pii_reader,
};

use support::reader_from_batches;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the request is forwarded unchanged to the process allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the pointer and layout came from the process allocator.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the pointer and layout came from the process allocator and the
        // requested size is forwarded unchanged.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[test]
fn pii_utf8_no_match_path_does_not_allocate_per_cell() {
    let _measurement_guard = ALLOCATION_TEST_LOCK.lock().unwrap();
    const ROWS: usize = 100_000;
    let schema = Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, false)]));
    let warmup = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(StringArray::from(Vec::<&str>::new())) as ArrayRef],
    )
    .unwrap();
    scan_pii_reader(reader_from_batches(vec![warmup]), 2).unwrap();
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
            "ordinary-value",
            ROWS,
        ))) as ArrayRef],
    )
    .unwrap();
    let reader = reader_from_batches(vec![batch]);

    ALLOCATIONS.store(0, Ordering::SeqCst);
    REALLOCATIONS.store(0, Ordering::SeqCst);
    let report = scan_pii_reader(reader, 2).unwrap();
    let allocations = ALLOCATIONS.load(Ordering::SeqCst);
    let reallocations = REALLOCATIONS.load(Ordering::SeqCst);

    assert_eq!(report.finding_count, 0);
    assert!(
        allocations <= 8,
        "PII no-match scan allocated {allocations} times for {ROWS} borrowed strings"
    );
    assert_eq!(reallocations, 0);
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[test]
fn primitive_fingerprint_allocations_are_constant_after_setup() {
    let _measurement_guard = ALLOCATION_TEST_LOCK.lock().unwrap();
    const ROWS: usize = 100_000;
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from_iter_values(0..ROWS as i64)) as ArrayRef],
    )
    .unwrap();
    let reader = reader_from_batches(vec![batch]);

    ALLOCATIONS.store(0, Ordering::SeqCst);
    REALLOCATIONS.store(0, Ordering::SeqCst);
    let digest = fingerprint_reader(reader).unwrap();
    let allocations = ALLOCATIONS.load(Ordering::SeqCst);
    let reallocations = REALLOCATIONS.load(Ordering::SeqCst);

    assert!(digest.starts_with("pf-fp-v1:"));
    assert!(
        allocations <= 8,
        "primitive fingerprint allocated {allocations} times for {ROWS} rows"
    );
    assert_eq!(
        reallocations, 0,
        "prepared fingerprint buffers must not reallocate"
    );
}

#[test]
fn primitive_range_validation_allocations_are_constant_after_setup() {
    let _measurement_guard = ALLOCATION_TEST_LOCK.lock().unwrap();
    const ROWS: usize = 100_000;
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from_iter_values(0..ROWS as i64)) as ArrayRef],
    )
    .unwrap();
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"min":0}}}"#,
    )
    .unwrap();
    let plan = CompiledContract::compile(&ast, schema.as_ref()).unwrap();
    let reader = reader_from_batches(vec![batch]);

    ALLOCATIONS.store(0, Ordering::SeqCst);
    REALLOCATIONS.store(0, Ordering::SeqCst);
    let report = execute_reader(reader, &plan, &ExecutionOptions::default()).unwrap();
    let allocations = ALLOCATIONS.load(Ordering::SeqCst);
    let reallocations = REALLOCATIONS.load(Ordering::SeqCst);

    assert!(report.valid);
    assert!(
        allocations <= 8,
        "primitive range validation allocated {allocations} times for {ROWS} rows"
    );
    assert_eq!(reallocations, 0, "validation buffers must not reallocate");
}
