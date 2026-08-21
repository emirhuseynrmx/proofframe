mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    CompiledContract, ContractAst, ContractDocument, ExecutionOptions, execute_reader,
    fingerprint_reader, scan_pii_reader,
};

use support::reader_from_batches;

struct CountingAllocator;

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static REALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn measure<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    REALLOCATIONS.with(|count| count.set(0));
    TRACKING.with(|tracking| tracking.set(true));
    let result = run();
    TRACKING.with(|tracking| tracking.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    let reallocations = REALLOCATIONS.with(Cell::get);
    (result, allocations, reallocations)
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
        // SAFETY: the request is forwarded unchanged to the process allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the pointer and layout came from the process allocator.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if TRACKING.try_with(Cell::get).unwrap_or(false) {
            let _ = REALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
        // SAFETY: the pointer and layout came from the process allocator and the
        // requested size is forwarded unchanged.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[test]
fn pii_utf8_no_match_path_does_not_allocate_per_cell() {
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

    let (report, allocations, reallocations) = measure(|| scan_pii_reader(reader, 2).unwrap());

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
    const ROWS: usize = 100_000;
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from_iter_values(0..ROWS as i64)) as ArrayRef],
    )
    .unwrap();
    let reader = reader_from_batches(vec![batch]);

    let (digest, allocations, reallocations) = measure(|| fingerprint_reader(reader).unwrap());

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

    let (report, allocations, reallocations) =
        measure(|| execute_reader(reader, &plan, &ExecutionOptions::default()).unwrap());

    assert!(report.valid);
    assert!(
        allocations <= 8,
        "primitive range validation allocated {allocations} times for {ROWS} rows"
    );
    assert_eq!(reallocations, 0, "validation buffers must not reallocate");
}

#[test]
fn relational_i64_validation_does_not_allocate_per_row() {
    const ROWS: usize = 100_000;
    let schema = Arc::new(Schema::new(vec![
        Field::new("low", DataType::Int64, false),
        Field::new("high", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from_iter_values(0..ROWS as i64)) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(1..=ROWS as i64)) as ArrayRef,
        ],
    )
    .unwrap();
    let document = ContractDocument::from_json(
        r#"{
            "version":"proofframe.contract.v2",
            "columns":{},
            "row_rules":[{
                "name":"ordered",
                "compare":{"left":{"column":"low"},"op":"lt","right":{"column":"high"}}
            }]
        }"#,
    )
    .unwrap();
    let plan = CompiledContract::compile_document(&document, schema.as_ref()).unwrap();

    let (report, allocations, reallocations) = measure(|| {
        execute_reader(
            reader_from_batches(vec![batch]),
            &plan,
            &ExecutionOptions::default(),
        )
        .unwrap()
    });

    assert!(report.valid);
    assert!(
        allocations <= 8,
        "relational scan allocated {allocations} times for {ROWS} rows"
    );
    assert_eq!(reallocations, 0, "relational buffers must not reallocate");
}
