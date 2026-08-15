# ProofFrame 0.5 stability and performance design

Status: accepted for implementation

Date: 2026-08-15

Compatibility policy: alpha APIs may change; `pf-fp-v1` verification remains supported

## 1. Purpose

ProofFrame 0.5 turns the existing alpha into a fail-closed, bounded-memory data
validation engine. It must improve throughput without weakening correctness or
using performance claims that cannot be reproduced.

The implementation will refactor the current code in place. A clean-room
rewrite and a custom global allocator are explicitly rejected: neither is
required to remove the measured per-cell allocations, and both would increase
release risk.

## 2. Release invariants

The following are release blockers:

1. Contracts are parsed strictly and compiled against the input Arrow schema
   before the first batch is inspected.
2. Unsupported rule/type combinations fail during compilation. They never
   become skipped checks.
3. Integer, unsigned integer, decimal and timestamp bounds are compared without
   conversion through `f64`.
4. Engine-owned memory, temporary storage, report samples and emitted change
   records are governed by explicit limits.
5. `pf-fp-v1` golden vectors continue to verify byte-for-byte. New evidence
   selects its fingerprint version explicitly.
6. Corrupt, truncated or oversized spill data is an error. Partial EOF is not a
   successful end-of-stream.
7. Receipt signature validity and signer trust are separate results.
8. Primitive validation and fingerprint hot loops allocate no heap memory per
   cell after plan construction.
9. Python batch processing releases the GIL and prefers Arrow's C stream
   protocol over materializing conversion methods.
10. Published performance numbers come from checked-in, reproducible benchmark
    artifacts and include memory limits and hardware metadata.

## 3. Architecture

The 0.4 `lib.rs` monolith is split by responsibility while preserving a small
public facade:

```text
src/
  lib.rs                 public facade and compatibility exports
  contract/
    mod.rs               versioned input schema and strict deserialization
    compile.rs           Arrow schema -> CompiledContract
    bounds.rs            exact typed literals and comparison plans
  execution/
    mod.rs               batch orchestration and cancellation
    kernels.rs           typed, column-oriented validation kernels
    resource.rs          ResourceBudget and accounting
  encoding/
    mod.rs               shared encoder interfaces
    v1.rs                frozen pf-fp-v1 implementation
    v2.rs                typed, column-oriented pf-fp-v2 implementation
    scratch.rs           reusable variable-width encoding buffers
  distinct/
    mod.rs               bounded exact distinct state
    arena.rs             append-only byte arena with collision verification
    spill.rs             sorted external runs
  diff/
    mod.rs               bounded diff orchestration
    partition.rs         versioned, checksummed spill representation
    sink.rs              bounded samples and streaming full output
  evidence/
    mod.rs               versioned evidence envelope
  receipt/
    mod.rs               receipt v1 reader and receipt v2
    trust.rs             expected key and trust-store policy
  pii.rs                 detection with corrected key identity
  error.rs               stable error codes and typed context
```

The dependency direction is one way: parsing -> compilation -> execution ->
evidence. Receipt signing consumes evidence but validation never depends on
receipt code.

## 4. Contract compilation

Public contract structures use `deny_unknown_fields`. Version selection is
explicit. Optional fields distinguish absent, null and invalid values where the
contract format requires it.

`CompiledContract::compile(contract, schema, options)` performs all structural
work once:

- resolves every referenced column to a physical index;
- validates rule/type compatibility;
- parses bounds into exact typed literals;
- compiles regular expressions once;
- selects one typed kernel and one encoder per field;
- computes capacities from `ResourceBudget`;
- returns diagnostics containing stable error code, rule and column path.

The scanner receives only compiled plans. It does not perform `HashMap` rule
lookups, regex compilation or long dynamic downcast chains inside a row loop.

## 5. Column-oriented execution

Execution is batch-oriented and column-major. Each compiled column plan owns a
typed kernel selected before scanning. Fixed-width arrays are downcast once per
batch, and their validity bitmap and values are traversed sequentially.

For primitive values, canonical type tags and bytes are written directly to the
fingerprint state. Variable-width and nested encodings reuse a scratch buffer
owned by the execution context. The old `canonical_value_bytes(...) -> Vec<u8>`
shape is removed from the hot path.

Validation and fingerprinting share the same decoded column view. Distinct and
unique checks receive that view instead of encoding the same cell again.

No `unsafe` code is required for the first 0.5 release. Any future use of SIMD
or unchecked access requires an isolated module, benchmark evidence, Miri or
sanitizer coverage where applicable, and a separate design decision.

## 6. Fingerprint versions

### pf-fp-v1

The current canonical format is frozen. Its implementation moves to `v1.rs`
without semantic changes. Golden vectors cover nulls, supported Arrow types,
batch boundaries and prior release fixtures. It remains available for explicit
generation during migration and indefinitely for verification.

### pf-fp-v2

V2 binds:

- a canonical, versioned Arrow type description;
- selected stable schema metadata;
- total row count;
- selected column order and identity;
- canonical column streams;
- algorithm and encoding version.

Its result is invariant to RecordBatch segmentation. The encoding plan is
created once per schema. Fixed-width values write directly to the hasher;
variable-width values use length framing without temporary per-cell vectors.
V1 and V2 digests can never be compared without matching version identifiers.

## 7. Bounded exact distinct and unique

Exactness is not replaced by probabilistic counting.

- Fixed-width types use typed sets with capacity reserved from the budget.
- Variable-width bytes are copied once into an append-only arena.
- The index stores a digest and arena span; digest collisions are resolved by
  comparing the original bytes.
- Before an in-memory structure crosses its assigned limit, it emits a sorted
  external run and releases reclaimable memory.
- External runs are merged with bounded buffers.

Every reservation is charged before allocation. If neither memory nor spill
budget can satisfy a reservation, the operation returns `PF_RESOURCE_LIMIT`
with requested and remaining amounts. It never falls back to unbounded growth.

## 8. Bounded diff and spill safety

Diff uses a versioned partition envelope containing magic bytes, format
version, declared lengths and checksum. All decoded lengths are capped before
allocation. Clean EOF is accepted only between complete records.

The result model separates summary from full output:

- counts are always retained;
- samples are limited by `max_samples`;
- full added, removed and changed records are written to an optional streaming
  JSONL or Arrow sink;
- the default API never accumulates every changed key in memory.

Temporary files are written under an operation-owned directory and finalized
atomically. Cancellation or error removes incomplete artifacts. Existing user
paths are never recursively deleted.

## 9. Evidence and receipts

Evidence v2 binds the dataset fingerprint, contract digest, engine version,
fingerprint version, relevant execution options, limits and result summary.
All public serialized structures carry a schema version and reject unknown
fields unless an extension map is explicitly part of that version.

Receipt v2 removes flattened, ambiguous fields. Verification returns two
independent facts:

1. whether the signature is cryptographically valid;
2. whether the signer is accepted by the caller's expected key or trust store.

A receipt carrying its own public key is not trusted solely because its
signature verifies. Receipt v1 remains readable and is labeled as legacy in
typed results.

## 10. Leakage and PII correctness

Leakage keys are encoded using their logical key ordinal and canonical column
identity, never an incidental physical index. Full-row comparisons require
canonical type equality as well as column identity.

PII scanning avoids producing owned strings for each primitive value. Text
arrays are scanned through borrowed slices. Any privacy fingerprint used as an
identity boundary must include an explicit version and enough bits for the
claimed collision resistance.

## 11. Python and CLI boundary

Python input preference is:

1. `__arrow_c_stream__`;
2. an existing `RecordBatchReader`;
3. a streaming file reader;
4. materialization only through an explicit compatibility path.

Long-running Rust operations execute through `py.allow_threads`. Python-facing
results are typed classes or mappings, not JSON strings that are immediately
parsed again. Rust error codes map to stable Python exception subclasses while
preserving column, rule and resource context.

CSV and Parquet CLI commands stream batches. The primary surface is organized
under `proofframe check`, `fingerprint`, `diff`, `sign` and `verify`. Deprecated
0.4 alpha entry points remain as migration shims for one release and emit
actionable warnings.

## 12. Resource model

`ResourceBudget` contains at minimum:

- `max_memory_bytes`;
- `max_temp_bytes`;
- `max_output_records`;
- `max_samples`;
- optional deadline/cancellation token.

Sub-operators receive child budgets, preventing two individually compliant
components from exceeding the operation limit together. Accounting is tested
independently from allocator RSS; benchmark reports show both engine-accounted
bytes and process peak RSS.

The guarantee is bounded behavior, not literally zero allocations throughout
the program. Serialization, variable-cardinality exact state and user-owned
results inherently allocate. Release claims are limited to paths measured by
the allocation harness.

## 13. Failure behavior

Stable error codes include:

- `PF_CONTRACT_UNKNOWN_FIELD`;
- `PF_CONTRACT_TYPE_MISMATCH`;
- `PF_CONTRACT_INVALID_BOUND`;
- `PF_SCHEMA_MISMATCH`;
- `PF_RESOURCE_LIMIT`;
- `PF_CORRUPT_PARTITION`;
- `PF_FINGERPRINT_VERSION`;
- `PF_RECEIPT_INVALID_SIGNATURE`;
- `PF_RECEIPT_UNTRUSTED_SIGNER`;
- `PF_CANCELLED`.

Errors contain structured context but do not expose full sensitive cell values
by default. A failed operation cannot publish a partial evidence or receipt
file as successful.

## 14. Verification strategy

Correctness precedes optimization. Each defect is introduced as a failing test
before implementation.

- contract compile tests cover unknown fields and every rule/type matrix cell;
- exact numeric boundary tests include values beyond `f64` integer precision;
- v1 and v2 golden vectors cover batching and schema changes;
- property and differential tests compare typed kernels with a simple reference;
- corruption tests truncate every spill field and mutate lengths/checksums;
- fault injection exercises allocation, disk-full, cancellation and sink errors;
- fuzz targets cover contracts, receipts and partition decoders;
- Python tests cover C stream preference, GIL release and typed exceptions;
- an instrumented test allocator counts hot-loop allocations without changing
  the library's production allocator;
- Miri covers isolated state machines and parsers that it can execute;
- CI runs formatting, Clippy, Rust tests, Python 3.10-3.13 tests, wheel smoke
  tests and release artifact verification.

## 15. Performance gates

Performance comparisons use the same machine, dataset and options, at least one
warm-up and five measured runs. The median is recorded with hardware, compiler,
Arrow version, memory budget and fingerprint version.

0.5 cannot be called stable unless:

- primitive validation and fingerprint kernels report zero heap allocations per
  cell after setup;
- prepared buffers do not reallocate while the operation remains within its
  declared plan capacity;
- fingerprinting is at least 3x faster than the checked-in 0.4 baseline on the
  existing 7.6M-row fixture;
- exact distinct is at least 2x faster on that fixture while obeying its memory
  budget;
- required and min/max checks regress by no more than 10%;
- bounded-memory tests stay within the configured engine budget plus a measured
  and documented runtime overhead;
- peak RSS is measured by a parent process or OS facility, not a Python monitor
  thread blocked by the GIL.

Stretch numbers may be published only after measurement. A missed speed target
blocks a performance claim but never justifies weakening a correctness or
resource invariant.

## 16. Migration and non-goals

Migration documentation maps each 0.4 alpha API to its 0.5 replacement. Breaking
changes are listed in the changelog and Python warnings name the replacement.

The following are not part of 0.5:

- dataframe mutation or automatic repair;
- query-engine predicate pushdown;
- distributed execution;
- probabilistic distinct as a substitute for exact mode;
- an unsafe SIMD rewrite;
- a custom global allocator requirement.

These boundaries keep 0.5 focused on a stable trust and execution foundation.

## 17. Implementation order

1. Establish strict contracts, stable errors and fail-closed compilation.
2. Freeze v1 vectors and introduce the encoder plan abstraction.
3. Implement v2 and allocation-tested column kernels.
4. Add resource accounting, bounded distinct and hardened spill records.
5. Rebuild diff and evidence on the bounded primitives.
6. Introduce receipt v2 and signer trust policy.
7. Correct leakage/PII identity behavior.
8. Replace Python/CLI materialization and JSON round trips.
9. Run adversarial, fuzz, memory and performance gates.
10. Update migration documentation and produce release artifacts.
