# Changelog

## 0.4.0-alpha.2

- Added `pf-fp-v1` canonical proof fingerprints that do not depend on Arrow display formatting.
- Added disk-backed exact keyed diffs with hash partitions and changed-column evidence.
- Added privacy-preserving PII findings, leakage checks, and signed proof receipts.
- Added full-vs-fast differential Proptest coverage for validation verdict drift.
- Hardened release gates with `#![forbid(unsafe_code)]`, Clippy warnings-as-errors, coverage gates,
  `cargo package`, and cross-platform wheel builds.

## 0.4.0-alpha.1

- Initial 0.4 alpha with Arrow-native profiling, contracts, Python API, CLI, and benchmark harness.
