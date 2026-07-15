<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame" width="100%" />
</div>

# ProofFrame

[![Crates.io](https://img.shields.io/crates/v/proofframe.svg)](https://crates.io/crates/proofframe)
[![docs.rs](https://img.shields.io/docsrs/proofframe)](https://docs.rs/proofframe)
[![CI](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml/badge.svg)](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/emirhuseynrmx/proofframe/blob/main/LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange)]()

**Arrow-native data contracts, canonical fingerprints, and proof receipts for Rust.**

ProofFrame is a Rust crate for checking Arrow `RecordBatchReader` streams and producing deterministic evidence:

- versioned canonical BLAKE3 dataset fingerprints (`pf-fp-v1`);
- typed validation reports with `valid`, `violation_count`, `truncated`, and bounded row findings;
- exact keyed diffs with added, removed, and changed-column evidence;
- high-signal PII scanning and train/test leakage checks;
- Ed25519 signed proof receipts;
- `#![forbid(unsafe_code)]`;
- optional Python bindings behind the `python` feature, not enabled by default.

The default crates.io build is a normal Rust library. It does not require PyO3, does not enable
`pyo3/extension-module`, and does not package Python source into the crate artifact.

## Install

```bash
cargo add proofframe@0.4.0-alpha.3
```

## Quick use

```rust
use proofframe::{profile_reader, validate_reader, ColumnContract, Contract};
use std::collections::HashMap;

let profile = profile_reader(reader)?;

let contract = Contract {
    columns: HashMap::from([
        ("id".to_string(), ColumnContract {
            required: true,
            unique: true,
            not_null: true,
            ..ColumnContract::default()
        }),
    ]),
    max_findings: 100,
};

let report = validate_reader(reader_again, &contract)?;

assert!(profile.fingerprint.starts_with("pf-fp-v1:"));
assert_eq!(report.valid, report.violation_count == 0);
```

## Public Rust API

The core functions accept Arrow readers and return typed Rust structs:

- `profile_reader(reader) -> Result<Profile, String>`
- `validate_reader(reader, &contract) -> Result<ValidationReport, String>`
- `validate_fast_reader(reader, &contract) -> Result<FastValidationReport, String>`
- `diff_readers(before, after, &keys) -> Result<DiffReport, String>`
- `scan_pii_reader(reader, max_findings) -> Result<PiiReport, String>`
- `detect_leakage_readers(train, test, &keys, max_samples) -> Result<LeakageReport, String>`
- `receipt::generate_keypair_json()`
- `receipt::sign_json(report_json, private_key)`
- `receipt::verify_json(receipt_json)`

## Features

```toml
[dependencies]
proofframe = "0.4.0-alpha.3"
```

The default feature set is intentionally empty. Enable `python` only when building the Python
extension path:

```toml
proofframe = { version = "0.4.0-alpha.3", features = ["python"] }
```

The PyPI package enables `python` plus `pyo3/extension-module` through maturin. Plain Rust tests and
`cargo package` should not need Python linker symbols, including on macOS.

## Why ProofFrame

Most data checks answer one narrow question: did this table pass? ProofFrame is built for the
production question: **what exactly was checked, why did it fail, and can we prove that later?**

It keeps validation evidence bounded while still counting every violation, fingerprints the ordered
Arrow stream with canonical bytes rather than display formatting, and emits deterministic reports
that can be stored in CI, data contracts, and release artifacts.

## Python

Python users should install the wheel from PyPI:

```bash
pip install proofframe==0.4.0a3
```

The Python README and CLI documentation live in the repository `README.md`.
