# ProofFrame 0.5 Stable Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship ProofFrame 0.5 as a fail-closed, bounded-memory Arrow validation engine with a compiled contract IR, allocation-free primitive hot loops, reproducible performance evidence, and an honest migration path from 0.4 alpha.

**Architecture:** Parse strict contract JSON into a versioned AST, compile it once against the Arrow schema into a schema-ordered typed execution plan, then run column-oriented kernels. Canonical encoders, exact stateful operators, evidence, receipts, and Python bindings consume those compiled primitives and share an explicit resource budget.

**Tech Stack:** Rust 1.85, Arrow 59.1, PyO3 0.29 with `abi3-py310`, Python 3.10+, PyArrow 15+, Serde, BLAKE3, Ed25519, proptest, pytest, Criterion-style release harnesses, Miri-compatible safe Rust.

## Global Constraints

- Keep `#![forbid(unsafe_code)]` for ProofFrame 0.5.
- Preserve existing `pf-fp-v1` golden vectors and verification behavior byte-for-byte.
- Emit new fingerprints with an explicit version; never compare V1 and V2 as if they were the same algorithm.
- Alpha Rust and Python APIs may break, but existing 0.4 Python entry points receive one release of migration shims.
- Unknown contract, receipt, evidence, and spill fields fail closed unless the specific schema version defines an extension map.
- Primitive validation and fingerprint loops perform zero heap allocations per cell after plan construction.
- Exact distinct, unique, diff, leakage, result samples, output records, memory, and temporary storage are explicitly bounded.
- No performance claim is published without five-run median/IQR evidence, hardware metadata, correctness guards, and process-level peak RSS.
- Every production behavior change starts with a failing test and ends with a focused commit.
- Keep plans under `docs/plans/`; do not introduce a `docs/superpowers/` directory.

---

## File Map

Create these focused modules and keep `src/lib.rs` as a small public facade:

```text
src/
  contract/{mod.rs,ast.rs,compile.rs,bounds.rs}
  encoding/{mod.rs,plan.rs,v1.rs,v2.rs,scratch.rs}
  execution/{mod.rs,kernels.rs,resource.rs,report.rs}
  distinct/{mod.rs,arena.rs,run.rs}
  diff/{mod.rs,partition.rs,sink.rs}
  evidence/mod.rs
  receipt/{mod.rs,trust.rs}
  python.rs
  error.rs
  pii.rs
  lib.rs
tests/
  support/mod.rs
  contract_compile.rs
  fingerprint_versions.rs
  allocation_contract.rs
  resource_limits.rs
  distinct_spill.rs
  diff_corruption.rs
  evidence_receipt.rs
  leakage_contract.rs
  test_api.py
  test_python_surface.py
  test_cli_streaming.py
  test_release_gate.py
benchmarks/
  release_gate.py
  fixtures/bitcoin-7_6m-manifest.json
fuzz/fuzz_targets/
  contract_json.rs
  receipt_json.rs
  partition_bytes.rs
```

The public facade re-exports contract, execution option, report, fingerprint-version, receipt-verification, and error-code types. Operator internals remain private.

---

### Task 1: Stable error codes and strict contract AST

**Files:**
- Create: `src/contract/mod.rs`
- Create: `src/contract/ast.rs`
- Modify: `src/error.rs`
- Modify: `src/lib.rs`
- Create: `tests/contract_compile.rs`

**Interfaces:**
- Produces: `ContractAst::from_json(&str) -> Result<ContractAst, ProofFrameError>`
- Produces: `ProofFrameError::code(&self) -> ErrorCode`
- Produces: `ErrorCode::{ContractUnknownField, ContractInvalidJson, ContractInvalidBound, ContractTypeMismatch, SchemaMismatch, ResourceLimit, CorruptPartition, FingerprintVersion, ReceiptInvalidSignature, ReceiptUntrustedSigner, Cancelled}`

- [ ] **Step 1: Write strict parsing regressions**

```rust
#[test]
fn unknown_contract_fields_fail_closed() {
    let error = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{},"max_findngs":4}"#,
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ContractUnknownField);
}

#[test]
fn bound_literals_preserve_integer_text() {
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"min":9007199254740993}}}"#,
    )
    .unwrap();
    assert_eq!(ast.columns["id"].min.as_ref().unwrap().as_text(), "9007199254740993");
}
```

- [ ] **Step 2: Run the tests and verify the new API is missing**

Run: `cargo test --locked --test contract_compile`

Expected: compilation fails because `ContractAst` and `ErrorCode` are not defined.

- [ ] **Step 3: Implement the strict syntax tree**

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractAst {
    pub version: ContractVersion,
    #[serde(default)]
    pub columns: BTreeMap<String, RuleAst>,
    #[serde(default = "default_max_findings")]
    pub max_findings: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum ContractVersion {
    #[serde(rename = "proofframe.contract.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NaNPolicyAst {
    #[default]
    Reject,
    Allow,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BoundAst {
    Number(serde_json::Number),
    Text(String),
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RuleAst {
    #[serde(default)] pub required: bool,
    #[serde(default)] pub not_null: bool,
    #[serde(default)] pub unique: bool,
    pub min: Option<BoundAst>,
    pub max: Option<BoundAst>,
    pub nan: Option<NaNPolicyAst>,
    pub pattern: Option<String>,
    pub allowed: Option<BTreeSet<String>>,
}
```

Parse once into `serde_json::Value`, walk the root and rule objects with explicit allowed-key tables, and report the first unknown key with its JSON path as `ContractUnknownField`. Deserialize the validated value into `ContractAst`; malformed JSON and wrong value shapes become `ContractInvalidJson`. Do not classify stable codes by matching Serde's human-readable error text.

- [ ] **Step 4: Add stable error-code serialization and rerun**

Run: `cargo test --locked --test contract_compile`

Expected: both tests pass and errors include a stable code plus human-readable context.

- [ ] **Step 5: Run the complete Rust suite**

Run: `cargo test --locked --all-features`

Expected: the original 10 tests and the strict AST tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/contract src/error.rs src/lib.rs tests/contract_compile.rs
git commit -m "feat: add strict versioned contract AST"
```

---

### Task 2: Semantic compiler and exact typed bounds

**Files:**
- Create: `src/contract/bounds.rs`
- Create: `src/contract/compile.rs`
- Modify: `src/contract/mod.rs`
- Modify: `src/lib.rs`
- Modify: `tests/contract_compile.rs`

**Interfaces:**
- Consumes: `ContractAst`, `arrow::datatypes::SchemaRef`
- Produces: `CompiledContract::compile(&ContractAst, &Schema) -> Result<CompiledContract, ProofFrameError>`
- Produces: `ColumnPlan { column_index: usize, name: Arc<str>, kernel: KernelKind, rules: CompiledRules }`
- Produces: `TypedBound::{I64(i64), U64(u64), F32(f32), F64(f64), Decimal128 { value: i128, scale: i8 }, Timestamp { value: i64, unit: TimeUnit }}`

- [ ] **Step 1: Write fail-closed semantic tests**

```rust
#[test]
fn unsupported_rule_type_pair_is_a_compile_error() {
    let schema = Schema::new(vec![Field::new("payload", DataType::Binary, false)]);
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"payload":{"min":1}}}"#,
    )
    .unwrap();
    let error = CompiledContract::compile(&ast, &schema).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ContractTypeMismatch);
}

#[test]
fn unsigned_bound_above_f64_precision_remains_exact() {
    let schema = Schema::new(vec![Field::new("id", DataType::UInt64, false)]);
    let ast = ContractAst::from_json(
        r#"{"version":"proofframe.contract.v1","columns":{"id":{"min":9007199254740993}}}"#,
    )
    .unwrap();
    let plan = CompiledContract::compile(&ast, &schema).unwrap();
    assert_eq!(plan.columns()[0].rules().min(), Some(&TypedBound::U64(9_007_199_254_740_993)));
}
```

Add the remaining matrix cases explicitly:

```rust
#[test]
fn semantic_errors_are_distinct() {
    let decimal = Schema::new(vec![Field::new("amount", DataType::Decimal128(20, 4), false)]);
    let wrong_scale = ast(r#"{"columns":{"amount":{"min":"1.23456"}}}"#);
    assert_eq!(compile(wrong_scale, decimal).unwrap_err().code(), ErrorCode::ContractInvalidBound);

    let text = Schema::new(vec![Field::new("name", DataType::Utf8, false)]);
    let invalid_regex = ast(r#"{"columns":{"name":{"pattern":"["}}}"#);
    assert_eq!(compile(invalid_regex, text).unwrap_err().code(), ErrorCode::ContractInvalidBound);

    let numbers = Schema::new(vec![Field::new("value", DataType::Int64, false)]);
    let reversed = ast(r#"{"columns":{"value":{"min":10,"max":1}}}"#);
    assert_eq!(compile(reversed, numbers).unwrap_err().code(), ErrorCode::ContractInvalidBound);
}

#[test]
fn timestamp_and_nan_policies_compile_to_typed_rules() {
    let schema = Schema::new(vec![
        Field::new("at", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("score", DataType::Float64, false),
    ]);
    let plan = compile(
        ast(r#"{"columns":{"at":{"min":1700000000000000000},"score":{"nan":"reject"}}}"#),
        schema,
    )
    .unwrap();
    assert!(matches!(plan.columns()[0].rules().min(), Some(TypedBound::Timestamp { value: 1_700_000_000_000_000_000, unit: TimeUnit::Nanosecond })));
    assert_eq!(plan.columns()[1].rules().nan(), NaNPolicy::Reject);
}
```

The test helper `ast` injects `"version":"proofframe.contract.v1"` into compact fixtures; production parsing never injects a version.

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked --test contract_compile`

Expected: compilation fails because semantic compilation types are absent.

- [ ] **Step 3: Implement exact bound parsing and type matrix**

```rust
pub enum KernelKind {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    F32, F64,
    Decimal128 { precision: u8, scale: i8 },
    Timestamp(TimeUnit),
    Utf8, LargeUtf8, Binary, LargeBinary, Boolean, Nested,
}

pub struct CompiledRules {
    pub required: bool,
    pub not_null: bool,
    pub unique: bool,
    pub min: Option<TypedBound>,
    pub max: Option<TypedBound>,
    pub nan: NaNPolicy,
    pub pattern: Option<Regex>,
    pub allowed: Option<Arc<HashSet<Box<str>, ahash::RandomState>>>,
}
```

Compile columns in Arrow schema order. Collect missing required columns as deterministic schema findings; reject a referenced non-required missing column when it carries any value rule.

- [ ] **Step 4: Prove full integer-domain behavior**

Run: `cargo test --locked --test contract_compile -- --nocapture`

Expected: exact `i64::MIN`, `i64::MAX`, `u64::MAX`, decimal, timestamp, NaN, and incompatibility cases pass without an `f64` conversion.

- [ ] **Step 5: Commit**

```bash
git add src/contract src/lib.rs tests/contract_compile.rs
git commit -m "feat: compile contracts into exact typed plans"
```

---

### Task 3: Frozen V1 encoder with allocation-tested primitive writes

**Files:**
- Create: `src/encoding/mod.rs`
- Create: `src/encoding/plan.rs`
- Create: `src/encoding/scratch.rs`
- Create: `src/encoding/v1.rs`
- Modify: `src/lib.rs`
- Create: `tests/fingerprint_versions.rs`
- Create: `tests/allocation_contract.rs`
- Create: `tests/support/mod.rs`

**Interfaces:**
- Produces: `EncodingPlan::for_schema(&Schema) -> Result<EncodingPlan, ProofFrameError>`
- Produces: `fingerprint_v1<R: RecordBatchReader>(reader: R) -> Result<Fingerprint, ProofFrameError>`
- Produces: `Fingerprint { version: FingerprintVersion, digest: [u8; 32], rows: u64 }`
- Produces test helper: `reader_from_batches(Vec<RecordBatch>) -> impl RecordBatchReader`

- [ ] **Step 1: Move the existing V1 pin into public integration fixtures**

```rust
mod support;

use support::reader_from_batches;

#[test]
fn v1_golden_vector_is_frozen() {
    let batch = RecordBatch::try_from_iter(vec![
        ("id", Arc::new(Int64Array::from(vec![1, 2, 3])) as ArrayRef),
        ("name", Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])) as ArrayRef),
    ])
    .unwrap();
    assert_eq!(
        fingerprint_reader(reader_from_batches(vec![batch])).unwrap(),
        "pf-fp-v1:4dc74e666725f040dea7e788827d0411e59c19a72b55f2ce27f22ed9a00afb42"
    );
}
```

Copy the exact existing golden value from the baseline test rather than recalculating it.

The helper is concrete and shared by integration tests:

```rust
pub fn reader_from_batches(
    batches: Vec<RecordBatch>,
) -> impl RecordBatchReader {
    let schema = batches.first().expect("at least one batch").schema();
    RecordBatchIterator::new(batches.into_iter().map(Ok), schema)
}
```

- [ ] **Step 2: Add a counting allocator regression**

Use a test-binary global allocator that counts allocation calls. Construct the Arrow batch and reader before resetting the counter, fingerprint 100,000 non-null `Int64` cells, and assert the call count is bounded by fixed setup/output allocations rather than row count:

```rust
ALLOCATIONS.store(0, Ordering::SeqCst);
let digest = fingerprint_reader(reader).unwrap();
let calls = ALLOCATIONS.load(Ordering::SeqCst);
assert!(digest.starts_with("pf-fp-v1:"));
assert!(calls <= 8, "primitive fingerprint allocated {calls} times");
```

- [ ] **Step 3: Verify RED on the existing encoder**

Run: `cargo test --release --locked --test allocation_contract primitive_fingerprint`

Expected: failure with allocation count proportional to the number of cells.

- [ ] **Step 4: Implement planned direct writes while preserving V1 framing**

```rust
pub(crate) enum ValueEncoder {
    I64,
    U64,
    F64,
    TimestampSecond,
    Utf8,
    Binary,
    Nested,
}

impl ValueEncoder {
    fn update_v1(
        &self,
        hasher: &mut blake3::Hasher,
        array: &dyn Array,
        row: usize,
        scratch: &mut Scratch,
    ) -> Result<(), ProofFrameError>;
}
```

For fixed-width values, write the precomputed encoded length, tag, and little-endian bytes directly to BLAKE3. For variable-width values, write known lengths directly. Reuse `Scratch` only for recursive nested values whose V1 outer length must be known first.

- [ ] **Step 5: Verify compatibility and allocation contract**

Run: `cargo test --release --locked --test fingerprint_versions --test allocation_contract`

Expected: V1 golden and segmentation vectors pass; primitive allocation count is fixed and independent of row count.

- [ ] **Step 6: Commit**

```bash
git add src/encoding src/lib.rs tests/fingerprint_versions.rs tests/allocation_contract.rs tests/support/mod.rs
git commit -m "perf: eliminate primitive per-cell fingerprint allocations"
```

---

### Task 4: PF-FP-V2 column streams and explicit version selection

**Files:**
- Create: `src/encoding/v2.rs`
- Modify: `src/encoding/mod.rs`
- Modify: `src/lib.rs`
- Modify: `tests/fingerprint_versions.rs`

**Interfaces:**
- Produces: `FingerprintVersion::{V1, V2}`
- Produces: `fingerprint_reader_with_options<R>(reader: R, options: &FingerprintOptions) -> Result<Fingerprint, ProofFrameError>`
- Produces: `FingerprintOptions { version: FingerprintVersion, metadata_keys: BTreeSet<String> }`

- [ ] **Step 1: Write V2 invariance and separation tests**

```rust
fn whole_batch() -> RecordBatch {
    RecordBatch::try_from_iter(vec![
        ("id", Arc::new(Int64Array::from(vec![1, 2, 3, 4])) as ArrayRef),
        ("flag", Arc::new(BooleanArray::from(vec![true, false, true, false])) as ArrayRef),
    ])
    .unwrap()
}

fn segmented_batches() -> Vec<RecordBatch> {
    vec![
        RecordBatch::try_from_iter(vec![
            ("id", Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef),
            ("flag", Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef),
        ])
        .unwrap(),
        RecordBatch::try_from_iter(vec![
            ("id", Arc::new(Int64Array::from(vec![3, 4])) as ArrayRef),
            ("flag", Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef),
        ])
        .unwrap(),
    ]
}

#[test]
fn v2_is_invariant_to_batch_segmentation() {
    let options = FingerprintOptions::new(FingerprintVersion::V2);
    let direct = fingerprint_reader_with_options(
        reader_from_batches(vec![whole_batch()]),
        &options,
    )
    .unwrap();
    let segmented = fingerprint_reader_with_options(
        reader_from_batches(segmented_batches()),
        &options,
    )
    .unwrap();
    assert_eq!(direct, segmented);
}

#[test]
fn fingerprint_versions_are_never_equal_by_digest_string_alone() {
    let v1 = fingerprint_reader_with_options(
        reader_from_batches(vec![whole_batch()]),
        &FingerprintOptions::new(FingerprintVersion::V1),
    )
    .unwrap();
    let v2 = fingerprint_reader_with_options(
        reader_from_batches(vec![whole_batch()]),
        &FingerprintOptions::new(FingerprintVersion::V2),
    )
    .unwrap();
    assert_eq!(v1.version(), FingerprintVersion::V1);
    assert_eq!(v2.version(), FingerprintVersion::V2);
    assert_ne!(v1.to_string(), v2.to_string());
}
```

Cover every V2 binding with direct mutations:

```rust
let base = v2(table_with_schema(DataType::Int64, false, [("unit", "id")], [1, 2]));
assert_ne!(base, v2(table_with_schema(DataType::UInt64, false, [("unit", "id")], [1, 2])));
assert_ne!(base, v2(table_with_schema(DataType::Int64, true, [("unit", "id")], [1, 2])));
assert_ne!(base, v2(table_with_schema(DataType::Int64, false, [("unit", "other")], [1, 2])));
assert_ne!(base, v2(table_with_schema(DataType::Int64, false, [("unit", "id")], [2, 1])));
assert_ne!(base, v2(two_column_table(["a", "b"])));
assert_ne!(v2(two_column_table(["a", "b"])), v2(two_column_table(["b", "a"])));
```

`table_with_schema`, `two_column_table`, and `v2` are local test helpers defined immediately above these assertions and construct real Arrow readers through `reader_from_batches`.

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked --test fingerprint_versions v2_`

Expected: missing V2 API.

- [ ] **Step 3: Implement per-column V2 hash streams**

```rust
struct V2State {
    schema_digest: blake3::Hasher,
    columns: Vec<blake3::Hasher>,
    rows: u64,
}
```

Each column stream includes a canonical type enum, null/value framing, and ordered values. Finalization hashes the version, canonical schema digest, total rows, column count, column ordinal, and each column digest into the root. Never use `DataType::to_string()` as the V2 canonical type representation.

- [ ] **Step 4: Verify deterministic behavior**

Run: `cargo test --locked --test fingerprint_versions`

Expected: all V1 and V2 vectors pass across requested batch sizes.

- [ ] **Step 5: Commit**

```bash
git add src/encoding src/lib.rs tests/fingerprint_versions.rs
git commit -m "feat: add typed batch-invariant pf-fp-v2"
```

---

### Task 5: Column-oriented compiled execution kernels

**Files:**
- Create: `src/execution/mod.rs`
- Create: `src/execution/kernels.rs`
- Create: `src/execution/report.rs`
- Modify: `src/lib.rs`
- Create: `tests/execution_contract.rs`
- Modify: `tests/allocation_contract.rs`

**Interfaces:**
- Consumes: `CompiledContract`, `EncodingPlan`, `RecordBatchReader`
- Produces: `execute_reader<R>(reader: R, plan: &CompiledContract, options: &ExecutionOptions) -> Result<ValidationReport, ProofFrameError>`
- Produces: `ExecutionOptions { fingerprint: Option<FingerprintVersion>, distinct: DistinctMode, resources: ResourceLimits, row_count_hint: Option<u64> }`
- Produces: `ProfileValue::{I64(i64), U64(u64), F64(f64), Decimal128 { value: i128, scale: i8 }, Timestamp { value: i64, unit: TimeUnit }}` so profile extrema do not lose integer precision

- [ ] **Step 1: Write semantic parity and allocation tests**

Test full/fast equality at `i64::MIN`, `i64::MAX`, `u64::MAX`, decimal scale boundaries, positive/negative zero, NaN policies, nulls, regex and allowed values. Define `finding_keys` as a test helper that maps each report finding to `(rule, column.as_str(), row)`. Assert primitive range validation performs a fixed number of allocations after plan/report capacity setup.

```rust
let finding_keys = |report: &ValidationReport| {
    report.findings.iter().map(|item| (item.rule, item.column.as_str(), item.row)).collect::<Vec<_>>()
};
assert_eq!(finding_keys(&full), finding_keys(&fast));
assert_eq!(report.findings[0].row, Some(2));
assert!(validation_allocations <= 4);
```

Use Arrow arrays containing `[i64::MIN, 9_007_199_254_740_993, i64::MAX]`, `[u64::MAX - 1, u64::MAX]`, `[-0.0, 0.0, f64::NAN]`, and `Decimal128(20, 4)` raw values. The contract bounds use their exact JSON integer/text forms.

- [ ] **Step 2: Verify RED**

Run: `cargo test --release --locked --test execution_contract --test allocation_contract`

Expected: exact typed boundary cases or allocation limit fail on the row-oriented implementation.

- [ ] **Step 3: Implement schema-ordered kernel dispatch**

Downcast once per column per batch. Execute `not_null`, exact range, unique feed, matcher, profile and fingerprint actions through a `ColumnPlan`; do not look up rules in a `HashMap` inside the scan.

```rust
match column.kernel {
    KernelKind::I64 => kernels::scan_i64(array, column, state, row_offset)?,
    KernelKind::U64 => kernels::scan_u64(array, column, state, row_offset)?,
    KernelKind::F64 => kernels::scan_f64(array, column, state, row_offset)?,
    KernelKind::Utf8 => kernels::scan_utf8(array, column, state, row_offset)?,
    _ => kernels::scan_supported(array, column, state, row_offset)?,
}
```

Format a cell value only when a retained finding needs a message. Counting a truncated violation must not allocate a message.

- [ ] **Step 4: Remove the duplicated row-major profiling path**

Route `validate_reader`, `validate_fast_reader`, `profile_reader`, and fingerprint entry points through the compiled plan and shared encoders. Keep compatibility return shapes at the facade.

```rust
pub fn validate_reader<R: RecordBatchReader>(
    reader: R,
    contract: &ContractAst,
) -> Result<ValidationReport, ProofFrameError> {
    let schema = reader.schema();
    let plan = CompiledContract::compile(contract, schema.as_ref())?;
    execute_reader(reader, &plan, &ExecutionOptions::profiled())
}
```

Delete `inspect_batches`, `numeric_value`, and the row-loop `HashMap` lookup path only after all compatibility tests pass through the facade.

- [ ] **Step 5: Verify correctness and formatting**

Run: `cargo fmt --all -- --check && cargo clippy --locked --all-targets --all-features -- -D warnings && cargo test --locked --all-features`

Expected: no warnings; all old and new behavior tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/execution src/lib.rs tests/execution_contract.rs tests/allocation_contract.rs
git commit -m "perf: execute compiled contracts with column kernels"
```

---

### Task 6: Hierarchical resource accounting

**Files:**
- Create: `src/execution/resource.rs`
- Modify: `src/execution/mod.rs`
- Modify: `src/error.rs`
- Create: `tests/resource_limits.rs`

**Interfaces:**
- Produces: `ResourceLimits { max_memory_bytes: u64, max_temp_bytes: u64, max_output_records: u64, max_samples: usize }`
- Produces: `CancellationToken::cancel()` and `CancellationToken::check() -> Result<(), ProofFrameError>`
- Produces: `ResourceAccount::root(ResourceLimits) -> ResourceAccount`
- Produces: `ResourceAccount::child(&self, memory_cap: u64, temp_cap: u64) -> ResourceAccount`
- Produces: `try_reserve_memory(u64) -> Result<MemoryReservation, ProofFrameError>` and `try_reserve_temp(u64) -> Result<TempReservation, ProofFrameError>`

- [ ] **Step 1: Write atomic accounting tests**

```rust
let root = ResourceAccount::root(ResourceLimits {
    max_memory_bytes: 1024,
    max_temp_bytes: 0,
    max_output_records: 0,
    max_samples: 0,
});
let child = root.child(768, 0);
let first = child.try_reserve_memory(700).unwrap();
assert_eq!(root.memory_used(), 700);
assert_eq!(child.try_reserve_memory(100).unwrap_err().code(), ErrorCode::ResourceLimit);
drop(first);
assert_eq!(root.memory_used(), 0);
```

Add overflow, sibling contention, temp accounting, zero limits, peak-usage and cancellation tests. Every batch boundary and external merge iteration checks cancellation and returns `ErrorCode::Cancelled` without publishing partial output.

```rust
assert_eq!(root.try_reserve_memory(u64::MAX).unwrap_err().code(), ErrorCode::ResourceLimit);
let sibling = root.child(1024, 0);
let held = child.try_reserve_memory(700).unwrap();
assert!(sibling.try_reserve_memory(400).is_err());
assert_eq!(root.peak_memory_used(), 700);
drop(held);

let token = CancellationToken::new();
token.cancel();
assert_eq!(token.check().unwrap_err().code(), ErrorCode::Cancelled);
```

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked --test resource_limits`

Expected: resource API missing.

- [ ] **Step 3: Implement reservation-before-allocation**

Use shared atomics for root totals and child-local totals; perform checked compare/exchange before allocation. Reservation `Drop` returns the exact charge. Store peak counters separately. Do not install a production global allocator.

```rust
pub struct MemoryReservation {
    root: Arc<Counters>,
    child: Arc<Counters>,
    bytes: u64,
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.child.memory.fetch_sub(self.bytes, Ordering::AcqRel);
        self.root.memory.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
```

`try_reserve_memory` first reserves the child counter, then the root counter; if the root reservation fails it rolls back the child counter before returning `PF_RESOURCE_LIMIT`.

The public defaults are 512 MiB engine memory, 4 GiB temporary storage, 100,000 streamed output records and 100 retained samples. Every value is serialized into evidence so two executions with different limits cannot accidentally claim identical execution context.

- [ ] **Step 4: Verify resource tests under contention**

Run: `cargo test --locked --test resource_limits -- --test-threads=8`

Expected: deterministic pass with no counter underflow or limit overshoot.

- [ ] **Step 5: Commit**

```bash
git add src/execution/resource.rs src/execution/mod.rs src/error.rs tests/resource_limits.rs
git commit -m "feat: enforce hierarchical resource budgets"
```

---

### Task 7: Bounded exact unique and distinct state

**Files:**
- Create: `src/distinct/mod.rs`
- Create: `src/distinct/arena.rs`
- Create: `src/distinct/run.rs`
- Modify: `src/execution/kernels.rs`
- Modify: `src/execution/mod.rs`
- Create: `tests/distinct_spill.rs`

**Interfaces:**
- Produces: `ExactState::new(ValueKind, ResourceAccount, PathBuf, Option<u64>)`
- Produces: `ValueRef<'a>::{I64(i64), U64(u64), F64(u64), Bytes(&'a [u8])}` where float values are canonicalized bit patterns
- Produces: `ExactState::insert(ValueRef<'_>, row: u64) -> Result<(), ProofFrameError>`
- Produces: `ExactState::finish() -> Result<ExactSummary, ProofFrameError>`
- Produces: `ExactSummary { distinct_count: u64, duplicate_count: u64, duplicate_samples: Vec<DuplicateSample>, metrics: ExactMetrics }`

- [ ] **Step 1: Write spill exactness and collision tests**

Force a 32 KiB memory limit over 100,000 unique timestamps, add duplicates across separate runs, and assert the exact duplicate row and distinct count. Inject two test digests with the same hash and different bytes; assert they remain distinct through byte verification.

```rust
let mut state = exact_i64_state(32 * 1024);
for row in 0..100_000_u64 {
    state.insert(ValueRef::I64(row as i64), row).unwrap();
}
state.insert(ValueRef::I64(7), 100_000).unwrap();
let summary = state.finish().unwrap();
assert_eq!(summary.distinct_count, 100_000);
assert_eq!(summary.duplicate_count, 1);
assert_eq!(summary.duplicate_samples[0].duplicate_row, 100_000);
assert!(summary.metrics.spill_bytes > 0);
assert!(summary.metrics.peak_memory_bytes <= 32 * 1024);
```

Under `#[cfg(test)]`, inject a digest function returning the same 128-bit digest for `b"left"` and `b"right"`; finish must report `distinct_count == 2`.

- [ ] **Step 2: Verify RED**

Run: `cargo test --locked --test distinct_spill`

Expected: existing unbounded sets cannot honor the memory limit or external-run contract.

- [ ] **Step 3: Implement typed in-memory state and append-only arena**

Use fixed-capacity `Vec<FixedRecord<T>>` segments for fixed-width values; each record carries the value and first row. When `row_count_hint` fits the budget, reserve the complete typed buffer once. Otherwise allocate fixed-size segments under reservations. Store variable bytes once in fixed-capacity arena segments and index `(digest, segment, offset, length, first_row)`. No segment reallocates.

```rust
#[repr(C)]
struct FixedRecord<T> {
    value: T,
    row: u64,
}

struct FixedSegment<T> {
    records: Vec<FixedRecord<T>>,
    capacity: usize,
    reservation: MemoryReservation,
}
```

Create a segment with `Vec::with_capacity(capacity)` only after reserving `capacity * size_of::<FixedRecord<T>>()`; seal it before `len == capacity` can trigger growth.

- [ ] **Step 4: Implement sorted runs and bounded merge**

Sort fixed records by `(value, row)` and variable records by canonical bytes. Deduplicate sequentially, write a checksummed run when the next segment cannot be reserved, release in-memory reservations, and continue. Merge runs with fixed-size buffers to calculate exact distinct and duplicate rows. Duplicate reporting is finalized during the merge so values that landed in different runs are detected without retaining an unbounded cross-run index.

```rust
pub(crate) struct RunHeader {
    magic: [u8; 8],
    version: u16,
    kind: ValueKind,
    records: u64,
    payload_bytes: u64,
    checksum: [u8; 32],
}

fn merge_runs(
    runs: &[RunPath],
    samples: &mut BoundedSamples<DuplicateSample>,
    account: &ResourceAccount,
) -> Result<ExactSummary, ProofFrameError>;
```

- [ ] **Step 5: Verify behavior and scaling counters**

Run: `cargo test --release --locked --test distinct_spill --test execution_contract`

Expected: exact results match an in-memory reference; memory peak is within the configured limit; spill bytes are reported.

- [ ] **Step 6: Commit**

```bash
git add src/distinct src/execution tests/distinct_spill.rs
git commit -m "feat: add bounded exact distinct and unique state"
```

---

### Task 8: Hardened bounded diff storage and output sinks

**Files:**
- Create: `src/diff/mod.rs`
- Create: `src/diff/partition.rs`
- Create: `src/diff/sink.rs`
- Modify: `src/lib.rs`
- Create: `tests/diff_corruption.rs`

**Interfaces:**
- Produces: `DiffOptions { resources: ResourceLimits, max_samples: usize, output: Option<DiffOutput> }`
- Produces: `DiffOutput::{JsonLines(PathBuf), ArrowIpc(PathBuf)}`
- Produces: `DiffReport { added_count, removed_count, changed_count, added_samples, removed_samples, changed_samples, truncated, metrics }`

- [ ] **Step 1: Write adversarial partition decoder tests**

Inside `src/diff/partition.rs`, add a `#[cfg(test)]` module. For every header/record boundary, truncate one byte and require `PF_CORRUPT_PARTITION`. Mutate magic, version, checksum, record length, value count and UTF-8. A length above `max_record_bytes` must fail before allocation. Keep `tests/diff_corruption.rs` for public diff round-trip, bounded-report and atomic-sink behavior.

```rust
let error = PartitionReader::open(
    Cursor::new(mutated),
    PartitionLimits { max_record_bytes: 1024, max_columns: 16 },
)
.unwrap_err();
assert_eq!(error.code(), ErrorCode::CorruptPartition);
```

- [ ] **Step 2: Write bounded report tests**

Diff 10,000 entirely changed rows with `max_samples = 3`; assert counts are 10,000, retained vectors have length 3, `truncated` is true, and full JSONL output has 10,000 complete records when a sink is supplied.

```rust
let report = diff_readers_with_options(
    before_reader(10_000),
    after_reader_all_changed(10_000),
    &["id".to_string()],
    DiffOptions { max_samples: 3, output: Some(DiffOutput::JsonLines(output.clone())), ..bounded() },
)
.unwrap();
assert_eq!(report.changed_count, 10_000);
assert_eq!(report.changed_samples.len(), 3);
assert!(report.truncated);
assert_eq!(std::fs::read_to_string(output).unwrap().lines().count(), 10_000);
```

- [ ] **Step 3: Verify RED**

Run: `cargo test --locked --test diff_corruption`

Expected: current partial EOF and unbounded-vector behavior fails.

- [ ] **Step 4: Implement `PFPART02` framing**

Frame each partition with fixed magic, version, header length, schema digest, record count, payload length, payload checksum and capped length-prefixed records. Distinguish clean EOF between records from truncated EOF inside a field.

```rust
const PARTITION_MAGIC: [u8; 8] = *b"PFPART02";

struct PartitionHeader {
    magic: [u8; 8],
    version: u16,
    header_len: u16,
    schema_digest: [u8; 32],
    record_count: u64,
    payload_len: u64,
    checksum: [u8; 32],
}
```

- [ ] **Step 5: Implement bounded merge and atomic sink finalization**

Sort/spill both sides under child budgets, merge by key, retain only samples, and stream full records to an operation-owned temporary file. Rename the completed sink atomically. On error/cancellation, delete only the explicit operation-owned path.

```rust
trait DiffSink {
    fn write(&mut self, record: &DiffRecord) -> Result<(), ProofFrameError>;
    fn finish(self: Box<Self>) -> Result<CompletedOutput, ProofFrameError>;
    fn abort(self: Box<Self>);
}
```

`finish` flushes and syncs the temporary file before an atomic rename; `abort` receives the already validated operation-owned path rather than a computed directory or glob.

- [ ] **Step 6: Verify diff suite**

Run: `cargo test --locked --test diff_corruption && cargo test --locked --all-features`

Expected: corruption fails closed; counts and sinks remain exact; compatibility diff tests pass through the shim.

- [ ] **Step 7: Commit**

```bash
git add src/diff src/lib.rs tests/diff_corruption.rs
git commit -m "feat: harden diff storage and bound result memory"
```

---

### Task 9: Versioned evidence envelope and receipt trust

**Files:**
- Create: `src/evidence/mod.rs`
- Move: `src/receipt.rs` to `src/receipt/mod.rs`
- Create: `src/receipt/trust.rs`
- Modify: `src/lib.rs`
- Create: `tests/evidence_receipt.rs`

**Interfaces:**
- Produces: `EvidenceV2 { schema, dataset, contract_digest, engine, execution, result }`
- Produces: `TrustPolicy::{SignatureOnly, ExpectedKey(VerifyingKey), TrustStore(TrustStore)}`
- Produces: `ReceiptVerification { signature_valid, report_hash_matches, schema_supported, signer_trusted, valid }`

- [ ] **Step 1: Write evidence binding tests**

Change fingerprint version, contract digest, memory limits, result count and engine version one at a time; require the evidence digest to change. Unknown envelope fields must fail deserialization.

```rust
let base = evidence_fixture();
for changed in [
    base.with_fingerprint_version(FingerprintVersion::V1),
    base.with_contract_digest([7; 32]),
    base.with_memory_limit(64 * 1024 * 1024),
    base.with_violation_count(2),
    base.with_engine_version("0.5.1"),
] {
    assert_ne!(base.digest().unwrap(), changed.digest().unwrap());
}
let mut unknown = serde_json::to_value(&base).unwrap();
unknown.as_object_mut().unwrap().insert("unknown".into(), 1.into());
assert!(serde_json::from_value::<EvidenceV2>(unknown).is_err());
```

- [ ] **Step 2: Write signer-trust tests**

Sign with key A. Verification under `ExpectedKey(A)` must be valid; under `ExpectedKey(B)` signature validity remains true while signer trust and overall validity are false. Receipt V1 remains readable as `legacy = true`.

```rust
let signed = sign_v2(evidence_fixture(), &key_a).unwrap();
let accepted = verify_v2(&signed, &TrustPolicy::ExpectedKey(key_a.verifying_key())).unwrap();
assert!(accepted.signature_valid && accepted.signer_trusted && accepted.valid);
let rejected = verify_v2(&signed, &TrustPolicy::ExpectedKey(key_b.verifying_key())).unwrap();
assert!(rejected.signature_valid);
assert!(!rejected.signer_trusted);
assert!(!rejected.valid);
```

- [ ] **Step 3: Verify RED**

Run: `cargo test --locked --test evidence_receipt`

Expected: self-asserted V1 public keys cannot satisfy the new trust contract.

- [ ] **Step 4: Implement strict V2 schemas**

Remove `serde(flatten)` from V2, apply `deny_unknown_fields`, domain-separate evidence/report hashes, and serialize explicit nested objects. Preserve the V1 reader in an isolated compatibility module.

```rust
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReceiptV2 {
    schema: ReceiptSchema,
    unsigned: UnsignedReceiptV2,
    signature: String,
}
```

- [ ] **Step 5: Verify cryptographic and structural contracts**

Run: `cargo test --locked --test evidence_receipt && cargo test --locked receipt::`

Expected: tampering, wrong expected key, unsupported schema and unsafe I-JSON integers produce distinct typed results/errors.

- [ ] **Step 6: Commit**

```bash
git add src/evidence src/receipt src/lib.rs tests/evidence_receipt.rs
git commit -m "feat: bind evidence and separate signer trust"
```

---

### Task 10: Leakage identity and allocation-conscious PII scanning

**Files:**
- Modify: `src/pii.rs`
- Create: `src/leakage.rs`
- Modify: `src/lib.rs`
- Create: `tests/leakage_contract.rs`

**Interfaces:**
- Produces: `LeakageOptions { resources: ResourceLimits, max_samples: usize }`
- Produces: canonical key encoding based on logical key ordinal and canonical field identity

- [ ] **Step 1: Write physical-order adversarial tests**

Create semantically identical keyed tables where the key column has a different physical index; keyed identity must match because ordinal zero in the selected key tuple is stable. Full-row mode with same names but different Arrow types must return `SchemaMismatch`.

```rust
let left = reader_from_batches(vec![table! { "id" => [7], "feature" => ["x"] }]);
let right = reader_from_batches(vec![table! { "feature" => ["other"], "id" => [7] }]);
let report = detect_leakage_with_options(left, right, &["id".into()], bounded()).unwrap();
assert_eq!(report.overlap_count, 1);

let error = detect_full_row(
    reader_from_batches(vec![table! { "id" => Int64Array::from(vec![1]) }]),
    reader_from_batches(vec![table! { "id" => StringArray::from(vec!["1"]) }]),
).unwrap_err();
assert_eq!(error.code(), ErrorCode::SchemaMismatch);
```

The `table!` notation above is a local test macro creating `RecordBatch` values; define its two used arms in the test file rather than adding a production macro.

- [ ] **Step 2: Write PII borrowed-string allocation test**

Scan 100,000 non-matching UTF-8 cells after setup and assert allocations do not grow per cell. Retained findings may allocate only up to `max_findings`.

```rust
ALLOCATIONS.store(0, Ordering::SeqCst);
let report = scan_pii_reader(reader, 2).unwrap();
let calls = ALLOCATIONS.load(Ordering::SeqCst);
assert_eq!(report.finding_count, 0);
assert!(calls <= 8, "PII no-match path allocated {calls} times");
```

- [ ] **Step 3: Verify RED**

Run: `cargo test --release --locked --test leakage_contract --test allocation_contract`

Expected: physical-index identity or per-cell string materialization violates the new tests.

- [ ] **Step 4: Implement canonical logical identity and borrowed detectors**

Hash key tuple position, canonical field identity, type and value. Keep binary digests in memory rather than hex `String`. Pass `&str` slices from Arrow string arrays directly to PII detectors; format other values only through typed stack buffers or retained findings.

```rust
fn update_key_identity(
    hasher: &mut blake3::Hasher,
    key_ordinal: u32,
    field: &Field,
    encoder: &ValueEncoder,
    array: &dyn Array,
    row: usize,
    scratch: &mut Scratch,
) -> Result<(), ProofFrameError>;
```

- [ ] **Step 5: Bound leakage exact state with the distinct operator**

Reuse `ExactState`/external runs for train and test identity sets and bounded overlap samples. Do not create unbounded `HashSet<String>` collections.

```rust
fn intersect_identity_runs(
    train: ExactRuns,
    test: ExactRuns,
    samples: &mut BoundedSamples<[u8; 32]>,
) -> Result<OverlapSummary, ProofFrameError>;
```

- [ ] **Step 6: Commit**

```bash
git add src/pii.rs src/leakage.rs src/lib.rs tests/leakage_contract.rs tests/allocation_contract.rs
git commit -m "fix: canonicalize leakage identity and remove PII cell copies"
```

---

### Task 11: Typed PyO3 boundary, C stream priority, and GIL release

**Files:**
- Create: `src/python.rs`
- Modify: `src/lib.rs`
- Modify: `python/proofframe/api.py`
- Create: `python/proofframe/errors.py`
- Modify: `python/proofframe/__init__.py`
- Modify: `tests/test_python_surface.py`
- Modify: `tests/test_api.py`

**Interfaces:**
- Produces Python exception classes: `ContractError`, `SchemaError`, `ResourceLimitError`, `CorruptDataError`, `ReceiptError`
- Preserves high-level Python `dict` results without Rust JSON-string/Python `json.loads` round trips
- Prefers `__arrow_c_stream__` before `to_arrow`

- [ ] **Step 1: Write C stream precedence test**

```python
class BothProtocols:
    def __arrow_c_stream__(self, requested_schema=None):
        return table().__arrow_c_stream__(requested_schema)
    def to_arrow(self):
        raise AssertionError("materializing path was selected")

assert proofframe.profile(BothProtocols(), distinct="none")["rows"] == 2
```

- [ ] **Step 2: Write GIL progress test**

Run a sufficiently large native validation while a Python thread increments a counter. Require counter progress during the native call, not merely before/after it.

```python
ticks: list[float] = []
stop = threading.Event()

def worker() -> None:
    while not stop.is_set():
        ticks.append(time.perf_counter())

thread = threading.Thread(target=worker)
thread.start()
started = time.perf_counter()
proofframe.fingerprint(pa.table({"id": pa.array(range(2_000_000), type=pa.int64())}))
finished = time.perf_counter()
stop.set()
thread.join()
assert any(started < tick < finished for tick in ticks)
```

- [ ] **Step 3: Write typed exception tests**

Unknown contract field raises `ContractError` with `.code == "PF_CONTRACT_UNKNOWN_FIELD"`; exhausted memory raises `ResourceLimitError` carrying requested and remaining byte fields.

```python
with pytest.raises(proofframe.ContractError) as caught:
    proofframe.validate(table(), {"columns": {}, "max_findngs": 1})
assert caught.value.code == "PF_CONTRACT_UNKNOWN_FIELD"

with pytest.raises(proofframe.ResourceLimitError) as caught:
    proofframe.profile(high_cardinality_table(), max_memory=1024)
assert caught.value.code == "PF_RESOURCE_LIMIT"
assert caught.value.requested_bytes > caught.value.remaining_bytes
```

- [ ] **Step 4: Verify RED**

Run: `python -m pytest tests/test_python_surface.py -q`

Expected: current `to_arrow` order, `ValueError` flattening, JSON round trips or held GIL fail the tests.

- [ ] **Step 5: Implement direct Python object conversion and detach native work**

Accept `Python<'_>` in each PyO3 function, move the Arrow reader and owned options into `py.detach(move || ...)`, then build Python mappings/lists after reacquiring the GIL. Map `ErrorCode` to the dedicated exception classes without string parsing.

```rust
#[pyfunction]
fn validate_arrow(
    py: Python<'_>,
    source: PyArrowType<ArrowArrayStreamReader>,
    contract_json: String,
    options: NativeExecutionOptions,
) -> PyResult<Py<PyAny>> {
    let result = py.detach(move || validate_native(source.0, &contract_json, options));
    result.and_then(|report| report.into_py_report(py)).map_err(map_py_error)
}
```

- [ ] **Step 6: Keep migration shims**

Existing `profile`, `fingerprint`, `validate`, `diff`, `scan_pii`, `detect_leakage`, `sign_receipt`, and `verify_receipt` names remain. Where return fields changed, add documented compatibility aliases and `DeprecationWarning` with the 0.5 replacement.

When the source is a PyArrow `Table` or `RecordBatch`, populate `ExecutionOptions.row_count_hint` before converting it to a reader. Generic C stream providers keep the hint absent and use fixed-capacity segments/spill.

```python
def validate(data, contract, *, include_profile=True, **options):
    warnings.warn(
        "validate() is retained for 0.5 compatibility; use check()",
        DeprecationWarning,
        stacklevel=2,
    )
    legacy = dict(contract)
    legacy.setdefault("version", "proofframe.contract.v1")
    return check(data, legacy, profile=include_profile, **options)
```

- [ ] **Step 7: Verify Python matrix locally**

Run: `maturin develop --release && python -m pytest -q && python -m ruff check python tests`

Expected: Python API, protocol selection, exception and GIL tests pass without warnings other than explicitly asserted deprecations.

- [ ] **Step 8: Commit**

```bash
git add src/python.rs src/lib.rs python/proofframe tests/test_api.py tests/test_python_surface.py
git commit -m "feat: add typed nonblocking Python boundary"
```

---

### Task 12: Streaming CLI and bounded outputs

**Files:**
- Modify: `python/proofframe/cli.py`
- Create: `tests/test_cli_streaming.py`
- Modify: `README.md`

**Interfaces:**
- Produces commands: `proofframe check`, `fingerprint`, `diff`, `sign`, `verify`
- Produces streaming `_open_reader(path: Path, batch_size: int) -> pa.RecordBatchReader`

- [ ] **Step 1: Write no-materialization CLI tests**

Monkeypatch `pyarrow.csv.read_csv` and `pyarrow.parquet.read_table` to raise. Run `check` and `fingerprint` over fixtures; they must succeed through `open_csv` and `ParquetFile.iter_batches` readers.

```python
monkeypatch.setattr(arrow_csv, "read_csv", lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("materialized CSV")))
monkeypatch.setattr(parquet, "read_table", lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("materialized Parquet")))
assert run_cli("check", str(csv_path), "--contract", str(contract_path)).returncode == 0
assert run_cli("fingerprint", str(parquet_path)).returncode == 0
```

- [ ] **Step 2: Write exit-code and atomic-output tests**

Assert: valid is 0, contract violation is 1, usage/config is 2, engine failure is 3, resource exhaustion is 4. Interrupt a diff sink and assert no successful final output exists.

```python
assert invoke(valid_args).returncode == 0
assert invoke(contract_violation_args).returncode == 1
assert invoke(usage_error_args).returncode == 2
assert invoke(corrupt_input_args).returncode == 3
assert invoke(resource_exhaustion_args).returncode == 4
with pytest.raises(KeyboardInterrupt):
    invoke_with_interrupt(diff_args)
assert not final_output.exists()
```

- [ ] **Step 3: Verify RED**

Run: `python -m pytest tests/test_cli_streaming.py -q`

Expected: current `_read()` materializes full tables and lacks the new commands/options.

- [ ] **Step 4: Implement readers and resource flags**

Add `--batch-size`, `--max-memory`, `--max-temp`, `--max-samples`, `--output`, and `--fingerprint-version`. Use `pyarrow.csv.open_csv` and `ParquetFile.iter_batches`; wrap Parquet batches with `RecordBatchReader.from_batches` without forming a Table.

```python
def _open_reader(path: Path, batch_size: int) -> pa.RecordBatchReader:
    if path.suffix.lower() == ".csv":
        return arrow_csv.open_csv(path, read_options=arrow_csv.ReadOptions(block_size=batch_size))
    if path.suffix.lower() == ".parquet":
        file = parquet.ParquetFile(path)
        return pa.RecordBatchReader.from_batches(file.schema_arrow, file.iter_batches(batch_size))
    raise ValueError(f"Unsupported input: {path}. Use .csv or .parquet")
```

- [ ] **Step 5: Verify CLI and compatibility aliases**

Run: `python -m pytest tests/test_cli_streaming.py tests/test_api.py -q`

Expected: streaming tests pass; `profile` and `validate` aliases emit a migration warning and retain compatible exit behavior.

- [ ] **Step 6: Commit**

```bash
git add python/proofframe/cli.py tests/test_cli_streaming.py README.md
git commit -m "feat: stream CLI inputs and bound outputs"
```

---

### Task 13: Reproducible allocation, cache, RSS, and throughput gates

**Files:**
- Create: `benchmarks/release_gate.py`
- Create: `benchmarks/fixtures/bitcoin-7_6m-manifest.json`
- Modify: `benchmarks/rule_matrix.py`
- Modify: `.github/workflows/benchmarks.yml`
- Modify: `docs/testing.md`
- Create: `tests/test_release_gate.py`

**Interfaces:**
- Produces JSON fields: `median_ms`, `iqr_ms`, `rows_per_second`, `allocations_per_row`, `capacity_growth_events`, `engine_peak_bytes`, `process_peak_rss_bytes`, `spill_bytes`, `hardware`, `compiler`, `dataset_sha256`

- [ ] **Step 1: Write benchmark manifest validation tests**

In `tests/test_release_gate.py`, exercise the artifact validator with temporary JSON fixtures. The harness refuses mismatched dataset SHA-256, missing run count, fewer than five samples, absent correctness guards, mixed fingerprint versions, or a baseline recorded on different hardware/compiler settings.

```python
@pytest.mark.parametrize(
    "mutation",
    ["wrong_sha256", "four_runs", "missing_correctness", "mixed_version", "different_cpu"],
)
def test_release_artifact_rejects_incomparable_inputs(valid_artifact, mutation):
    candidate = mutate(valid_artifact, mutation)
    with pytest.raises(ArtifactError):
        validate_comparison(valid_artifact, candidate)
```

- [ ] **Step 2: Replace thread RSS sampling**

Spawn a dedicated benchmark worker process. The parent samples the child through OS process counters and records the maximum until process exit. On Linux also record `resource.getrusage`/`/proc`; on Windows use `psutil.Process.memory_info().rss`. Report the larger credible value and the sampling interval.

```python
process = subprocess.Popen(worker_command, stdout=subprocess.PIPE, text=True)
observed_peak = 0
sample_interval_seconds = 0.005
while process.poll() is None:
    observed_peak = max(observed_peak, psutil.Process(process.pid).memory_info().rss)
    time.sleep(sample_interval_seconds)
worker_result = json.loads(process.stdout.read())
worker_result["process_peak_rss_bytes"] = max(observed_peak, worker_result["os_reported_peak_rss_bytes"])
```

- [ ] **Step 3: Add scaling diagnostics**

Run 10K through 7.6M sizes and retain the original four operations: numeric min, timestamp unique, full contract, fingerprint. Add allocation/growth/state/spill counters. Keep metadata latency separate from rows/s.

```python
ROW_TARGETS = [10_000, 100_000, 500_000, 1_000_000, 2_500_000, 5_000_000, 7_645_034]
CASES = ["numeric_min", "timestamp_unique", "full_contract", "fingerprint"]
for rows, case in itertools.product(ROW_TARGETS, CASES):
    samples = [run_worker(rows, case) for _ in range(5)]
    artifact[case][str(rows)] = summarize(samples)
```

On Linux performance runners, collect `cycles`, `instructions`, `cache-misses`, `branch-misses` and stalled-cycle counters with `perf stat` when permitted. Store unavailable counters as explicit `null` plus the OS error; never substitute estimates. These counters diagnose code generation and locality but do not become cross-machine pass/fail gates.

- [ ] **Step 4: Encode release gates**

```python
assert current["fingerprint"]["median_ms"] <= baseline["fingerprint"]["median_ms"] / 3
assert current["timestamp_unique"]["median_ms"] <= baseline["timestamp_unique"]["median_ms"] / 2
assert current["numeric_min"]["median_ms"] <= baseline["numeric_min"]["median_ms"] * 1.10
assert current["fingerprint"]["allocations_per_row"] == 0
assert current["engine_peak_bytes"] <= manifest["max_memory_bytes"]
```

- [ ] **Step 5: Run a local synthetic smoke benchmark**

Run: `python benchmarks/release_gate.py --rows 100000 --runs 5 --output target/release-gate-smoke.json`

Expected: correctness and artifact-schema checks pass. This smoke result is not published as the 7.6M claim.

- [ ] **Step 6: Commit**

```bash
git add benchmarks .github/workflows/benchmarks.yml docs/testing.md tests/test_release_gate.py
git commit -m "bench: gate allocation memory and scaling claims"
```

---

### Task 14: Fuzzing, release CI, migration, and version 0.5.0

**Files:**
- Create: `fuzz/Cargo.toml`
- Create: `fuzz/fuzz_targets/contract_json.rs`
- Create: `fuzz/fuzz_targets/receipt_json.rs`
- Create: `fuzz/fuzz_targets/partition_bytes.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/wheels.yml`
- Modify: `.github/workflows/publish.yml`
- Modify: `Cargo.toml`
- Modify: `pyproject.toml`
- Modify: `CHANGELOG.md`
- Create: `docs/migration-0.4-to-0.5.md`
- Modify: `README.md`
- Modify: `README-crates.md`

**Interfaces:**
- Produces version `0.5.0` in Cargo and Python metadata
- Produces a migration table for every 0.4 alpha public entry point and serialized field

- [x] **Step 1: Add parser fuzz targets**

Each target accepts arbitrary bytes and calls only the public strict decoder. Success must round-trip to the same versioned structure; failure must return a typed error without panic or unbounded allocation. Partition fuzzing uses a 1 MiB resource cap.

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = proofframe::ContractAst::from_json(text);
    }
});
```

Receipt target calls the strict V1/V2 dispatcher. Partition target calls `PartitionReader` through a small public fuzz-only feature with `max_record_bytes = 1 << 20` and `max_columns = 1024`.

- [x] **Step 2: Add release CI jobs**

Run format, Clippy `-D warnings`, Rust tests, Python 3.10-3.13 tests, wheel smoke tests on Linux/macOS/Windows, Miri-compatible module tests, 60-second fuzz smoke jobs, package content checks, and README examples. Pin actions by immutable commit SHA.

```yaml
- run: cargo fmt --all -- --check
- run: cargo clippy --locked --all-targets --all-features -- -D warnings
- run: cargo test --locked --all-features
- run: cargo +nightly miri test -p proofframe contract:: encoding:: diff::partition::
- run: cargo fuzz run contract_json -- -max_total_time=60
```

- [x] **Step 3: Write migration and claim audit**

Document removed/renamed APIs, V1/V2 selection, resource defaults, exception mapping, receipt trust requirements, streaming CLI changes and exact compatibility duration. Search README/changelog for every performance number and require a linked checked-in artifact.

```markdown
| 0.4 alpha | 0.5 | Compatibility |
|---|---|---|
| `validate(..., include_profile=False)` | `check(..., profile=False)` | shim through 0.5.x |
| implicit `pf-fp-v1` | explicit `fingerprint_version=` | V1 verification retained |
| self-asserted receipt key | `TrustPolicy` | caller must choose trust policy |
```

- [x] **Step 4: Set release versions only after gates pass**

Change Cargo to `0.5.0` and Python to `0.5.0`. Update classifiers from Alpha to Production/Stable only if all stable gates pass; otherwise use Beta and do not label the release stable.

```toml
# Cargo.toml
[package]
version = "0.5.0"

# pyproject.toml
[project]
version = "0.5.0"
```

Run `cargo metadata --no-deps` and `python -c "import tomllib; print(tomllib.load(open('pyproject.toml','rb'))['project']['version'])"`; both must report `0.5.0`.

- [ ] **Step 5: Run the complete verification matrix**

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo test --release --locked --test allocation_contract
python -m pytest -q
python -m ruff check python tests benchmarks
maturin build --release --locked
python -m twine check target/wheels/*
cargo package --locked
```

Expected: every command exits 0 with no unreviewed warnings. The 7.6M release gate is executed on the pinned dataset environment before publication.

- [ ] **Step 6: Commit the release candidate**

```bash
git add .github fuzz Cargo.toml Cargo.lock pyproject.toml CHANGELOG.md README.md README-crates.md docs
git commit -m "release: prepare ProofFrame 0.5.0"
```

---

## Execution Checkpoints

1. After Tasks 1-2: contract AST and typed IR correctness review.
2. After Tasks 3-5: fingerprint compatibility, allocation, and kernel review.
3. After Tasks 6-8: bounded-memory and corruption review.
4. After Tasks 9-12: trust boundary and Python/CLI review.
5. After Tasks 13-14: evidence-backed release audit.

At every checkpoint, keep the worktree passing before beginning the next group. Do not combine failed hypotheses across checkpoints; return to root-cause investigation when a RED test fails for an unexpected reason.
