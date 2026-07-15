# Changelog

## 0.4.0-alpha.3

- Extended canonical fingerprinting to nested list, large-list, fixed-size-list, struct, and map
  columns using recursive, domain-separated encodings so nested data no longer fails closed.
- Documented the public Rust API: every exported type, field, and function now carries rustdoc,
  and the `receipt` module has a module-level overview.
- Added a `documentation` link to crate metadata.
- Unpinned the `roaring` dependency and trimmed the published crate to Rust sources.

## 0.4.0-alpha.2

- Added `pf-fp-v1` canonical proof fingerprints that do not depend on Arrow display formatting.
- Added disk-backed exact keyed diffs with hash partitions and changed-column evidence.
- Added privacy-preserving PII findings, leakage checks, and signed proof receipts.
- Added full-vs-fast differential Proptest coverage for validation verdict drift.
- Added explicit `violation_count`/`truncated` validation reporting so bounded finding examples
  cannot hide contract failures.
- Hardened diff keys with canonical binary composite-key tuples and full schema signatures.
- Added CLI exit codes for valid data, contract violations, input/config errors, and internal
  failures.
- Hardened release gates with `#![forbid(unsafe_code)]`, Clippy warnings-as-errors, coverage gates,
  `cargo package`, and cross-platform wheel builds.

## 0.4.0-alpha.1

- Initial 0.4 alpha with Arrow-native profiling, contracts, Python API, CLI, and benchmark harness.
