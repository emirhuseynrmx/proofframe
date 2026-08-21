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

> **Current release — 0.5.1**
>
> The current stable release preserves the 0.5 API and fingerprint protocols while simplifying
> execution internals and tightening release-package quality gates.

```bash
cargo add proofframe@0.5.1
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
use proofframe::{CompiledContract, ContractAst, ExecutionOptions, execute_reader};

let schema = reader.schema();
let ast = ContractAst::from_json(r#"{
  "version": "proofframe.contract.v1",
  "columns": {
    "order_id": {"required": true, "not_null": true, "unique": true},
    "amount": {"min": 0}
  },
  "max_findings": 100
}"#)?;
let plan = CompiledContract::compile(&ast, schema.as_ref())?;
let report = execute_reader(reader, &plan, &ExecutionOptions::default())?;

assert_eq!(report.valid, report.violation_count == 0);
# Ok::<(), proofframe::ProofFrameError>(())
```

Compilation resolves columns, converts exact bounds to Arrow-native values, compiles regular
expressions, and selects kernels once. Unknown fields, invalid bounds, missing required columns,
and incompatible rule/type pairs fail before execution.

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

## Evidence and trust

Evidence V2 carries separate canonical contract, compiled plan, and Arrow schema digests. Receipt
V2 uses Ed25519 signatures and should be verified with `TrustPolicy::ExpectedKey` or `TrustStore`
when signer identity matters.

## Compatibility

The 0.5 line preserves V1 fingerprints and established compatibility entry points. New Rust code
should use `ContractAst`, `CompiledContract`, `execute_reader`, explicit fingerprint versions, and
Receipt V2 with a trust policy.

The MSRV is Rust 1.85. Full API documentation is available on [docs.rs](https://docs.rs/proofframe),
and release history is recorded in the [changelog](CHANGELOG.md).
