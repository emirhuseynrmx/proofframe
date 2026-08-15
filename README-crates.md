<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame" width="100%" />
</div>

# ProofFrame for Rust

[![Crates.io](https://img.shields.io/crates/v/proofframe.svg)](https://crates.io/crates/proofframe)
[![docs.rs](https://img.shields.io/docsrs/proofframe)](https://docs.rs/proofframe)
[![CI](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml/badge.svg)](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange)](https://www.rust-lang.org/)

ProofFrame compiles strict, versioned contracts against Arrow schemas and executes typed validation
kernels over `RecordBatchReader` streams. Exact state and retained evidence are resource-bounded.

```bash
cargo add proofframe@0.5.0
```

The default crate has no Python dependency. Enable the `python` feature only when building the PyO3
extension.

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

Compilation resolves column indices, converts exact source bounds to Arrow-native values, compiles
regular expressions, and selects a kernel once. Unknown fields and rule/type mismatches fail before
the first batch is consumed. Timestamp bounds are signed integer ticks in the Arrow field's unit,
encoded as JSON numbers or decimal strings.

## Fingerprint protocol

```rust
use proofframe::{FingerprintOptions, FingerprintVersion, fingerprint_reader_with_options};

let options = FingerprintOptions::new(FingerprintVersion::V2);
let fingerprint = fingerprint_reader_with_options(reader, &options)?;
assert!(fingerprint.to_string().starts_with("pf-fp-v2:"));
# Ok::<(), proofframe::ProofFrameError>(())
```

V1 remains frozen for existing proofs. V2 is a distinct protocol; callers must persist the version
with the digest.

## Resource limits

`ExecutionOptions` carries memory and temporary-storage limits plus cooperative cancellation. Exact
uniqueness uses a hierarchical account, spills sorted checksummed runs when necessary, and returns
peak memory, peak temporary bytes, spill bytes, and run counts. Findings are sampled independently
from the exact violation count and capped by both contract `max_findings` and resource `max_samples`.

Keyed diff and leakage APIs expose their own options and share the same fail-closed resource model.
Partition headers and record lengths are validated before allocation.
Legacy profile exact-distinct state uses the same spill engine; `profile_reader` defaults to no
distinct state and callers opt in with `profile_reader_with_resources`.

Evidence V2 carries separate `pf-contract-v1`, `pf-plan-v1`, and `pf-schema-v1` digests. Receipt V2
signatures should be verified with `TrustPolicy::ExpectedKey` or `TrustStore` when signer identity is
security-relevant.

## Compatibility

The 0.4 `Contract`, `validate_reader`, `validate_fast_reader`, `profile_reader`, and V1 receipt
functions remain available for the 0.5 migration window. New Rust code should use `ContractAst`,
`CompiledContract`, `execute_reader`, explicit fingerprint versions, and receipt V2 with a
`TrustPolicy`.

The crate is `#![forbid(unsafe_code)]`. Its MSRV is Rust 1.85.
