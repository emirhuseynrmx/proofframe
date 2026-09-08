<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame" width="100%" />
</div>

# ProofFrame for Rust

[![Crates.io](https://img.shields.io/crates/v/proofframe.svg)](https://crates.io/crates/proofframe)
[![docs.rs](https://img.shields.io/docsrs/proofframe)](https://docs.rs/proofframe)
[![CI](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml/badge.svg)](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/emirhuseynrmx/proofframe/graph/badge.svg)](https://codecov.io/gh/emirhuseynrmx/proofframe)
[![DeepSource](https://app.deepsource.com/gh/emirhuseynrmx/proofframe.svg/?label=active+issues&show_trend=true)](https://app.deepsource.com/gh/emirhuseynrmx/proofframe/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange)](https://www.rust-lang.org/)

**Compiled Arrow contracts with deterministic, resource-bounded evidence.**

ProofFrame compiles strict, versioned contracts against Arrow schemas and executes typed kernels
over `RecordBatchReader` streams. It provides exact validation verdicts, canonical fingerprints,
bounded findings, keyed diffs, evidence records, and signed proof receipts from one Rust core.

> **Current release — 0.7.1**
>
> Eleven more contract rules, a review the crate renders itself, and exact state that no longer
> needs a filesystem to run. V1 plans and fingerprint protocols remain frozen, and a new rule
> joins the plan digest only when a contract uses it.

```bash
cargo add proofframe@0.7.1
```

The default crate has no Python dependency and forbids unsafe code. Enable the `python` feature only
when building the PyO3 extension.

## Why ProofFrame

- Contracts are compiled against the physical Arrow schema before rows are scanned.
- Exact violation counts are separate from bounded finding samples.
- Fingerprints use canonical encodings rather than display formatting.
- Exact uniqueness and keyed diff operate under explicit memory and temporary-storage limits.
- Evidence binds the dataset, contract, plan, schema, engine, limits, and result.
- Receipt verification separates signature validity from signer trust.

## Compiled contract API

```rust
use proofframe::{CompiledContract, ContractDocument, ExecutionOptions, execute_reader};

let schema = reader.schema();
let document = ContractDocument::from_json(r#"{
  "version": "proofframe.contract.v2",
  "columns": {},
  "row_rules": [{
    "name": "total_covers_subtotal",
    "compare": {
      "left": {"column": "total"},
      "op": "gte",
      "right": {"column": "subtotal"}
    }
  }],
  "dataset_rules": {
    "distinct_ratio": {"order_id": {"min": 1.0}}
  }
}"#)?;
let plan = CompiledContract::compile_document(&document, schema.as_ref())?;
let report = execute_reader(reader, &plan, &ExecutionOptions::default())?;

assert_eq!(report.valid, report.violation_count == 0);
# Ok::<(), proofframe::ProofFrameError>(())
```

Compilation resolves columns and operands, converts exact bounds to Arrow-native values, compiles
regular expressions, and selects row and dataset kernels once. Unknown fields, invalid ratios,
duplicate rule names, missing columns, and incompatible operands fail before execution.

## Deterministic partitions

`check_partition_readers` accepts ordered `PartitionReader` streams and a worker count in
`ExecutionOptions`. Stateless work may execute concurrently, but reports merge in logical partition
order. Rules that require global exact state retain one ordered traversal so composite uniqueness
and distinct counts never become partition-local approximations.

`check_partition_readers_with_evidence` returns the same report plus a domain-separated partition
manifest. The manifest can be signed and verified without changing Receipt V2 bytes.

## Fingerprint protocols

```rust
use proofframe::{FingerprintOptions, FingerprintVersion, fingerprint_reader_with_options};

let options = FingerprintOptions::new(FingerprintVersion::V2);
let fingerprint = fingerprint_reader_with_options(reader, &options)?;
assert!(fingerprint.to_string().starts_with("pf-fp-v2:"));
# Ok::<(), proofframe::ProofFrameError>(())
```

V1 remains frozen for existing proofs. V2 is a distinct protocol that binds Arrow types and
nullability while remaining invariant to record-batch segmentation.

## Resource model

`ExecutionOptions` carries memory and temporary-storage limits, finding limits, spill policy, and
cooperative cancellation. Exact uniqueness spills sorted checksummed runs when needed. Keyed diff
uses a conservatively budgeted in-memory path for known-small inputs and checksummed partitions for
larger or unknown streams. `SpillPolicy::Never` fails rather than writing exact-state data.

Every length and allocation boundary is validated before allocation. Reports expose peak memory,
temporary bytes, spill bytes, and run or partition counts.

## Try it without installing anything

[**Drop a CSV into the browser linter →**](https://emirhuseyin.tech/proofframe/#linter)

The engine is compiled to WebAssembly and runs in the tab: it reads the file with Arrow, infers a
draft contract, refuses to execute that draft until you have reviewed it, and renders the same HTML
review `proofframe review` writes. Nothing is uploaded, and the page reports the engine time it
measured on your machine.

Uniqueness and the dataset-level exact rules run there too, in memory, failing closed on the
budget rather than on a missing folder. Only `references` is refused, because it resolves against a
second dataset the page cannot bind; a contract that uses it is still checked for everything else,
and that result is reported as incomplete and carries no evidence.

## Evidence and trust

Evidence V2 carries separate canonical contract, compiled plan, and Arrow schema digests. Receipt
V2 uses Ed25519 signatures and should be verified with `TrustPolicy::ExpectedKey` or `TrustStore`
when signer identity matters.

## 0.7.1

Eleven rules join `dataset_rules` and the column rules: `monotonicity`, `gap_detection`,
`mutually_exclusive`, `sum`, `mean`, `std_dev`, `conditional_unique`, `row_count_delta`,
`balance_equal`, `max_dominant_value_ratio`, and `min_length`/`max_length` on text. Date bounds accept `"2024-01-01"` as well as a day count. Each
rule hashes into `compiled_plan_digest` only when a plan carries it, so contracts written before
this release keep the plan identity their receipts record.

Totals are counted in `i128` for integers and with Neumaier compensation for floats: the Arrow
kernel wraps silently past `i64::MAX` and reduces floats in a tree whose shape follows the batch
length, and a verdict must not depend on how a reader chose to chunk the file.

`ExactState` no longer requires a spill directory, so uniqueness works on a read-only filesystem
and under `wasm32-unknown-unknown`. Without one, a full segment grows rather than ending the scan:
the segment size exists to decide when to write a run, and with no run to write it was stopping
scans that had most of their budget left. The memory budget is the only bound, charged before each
allocation. Columns are no longer rationed a slice of it either — a child account charges its
parent, so the root enforces the real total and the column that needs the room gets it.

`ColumnProfile` gains `distinct_limited`. A profile describes rather than decides, so a column
holding more distinct values than the budget can keep now loses its own count instead of ending the
whole scan, and the flag says that is why `distinct_count` is `None`. Rules that return a verdict
are unchanged: `unique` still fails closed, because there is no honest way to pass a rule you
stopped checking.

`review_html` and `review_markdown` render the offline HTML review and the CI Markdown summary from
a report, its evidence and the contract. They were Python-only, so a Rust caller had no review at
all; they are now part of this crate and the Python package calls them. Fixtures pin the output byte
for byte against the renderer they replaced. `evidence_for_check` assembles Evidence V2 for a
completed check, so publishing evidence does not mean copying that assembly.

## 0.7.0

Contract diagnostics name the field they are about: an invalid V2 column type or a missing or
numeric contract version now says which column and what was expected, instead of surfacing as an
untagged enum error.

Validation reports carry per-column evaluation counters, so a caller can tell a contract that
passed because nothing was wrong from one that passed because nothing was asked. The engine
reports schema indices rather than names, which keeps the per-row kernels and the allocation
contract unchanged; resolving them to names is the reader's job.

The Python package built on this core adds the explicit accepted/rejected/unknown file-acceptance
decision. It is a surface over this crate's existing scan; the fingerprint and evidence protocols
are unchanged.

## Compatibility

Version 0.7.1 preserves V1 fingerprints and established compatibility entry points. New Rust code
should use `ContractDocument`, `CompiledContract::compile_document`, explicit fingerprint versions,
and Receipt V2 or partition-manifest receipts with a trust policy.

The MSRV is Rust 1.85. Full API documentation is available on [docs.rs](https://docs.rs/proofframe),
and release history is recorded in the [changelog](CHANGELOG.md).
