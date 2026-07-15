# Changelog

## 0.4.0-alpha.4

- Added standalone `fingerprint_reader` / `pf.fingerprint(data)` so callers can compute only the
  canonical `pf-fp-v1` hash without profile, min/max, or exact distinct state.
- Added `profile(..., distinct="none"|"exact")`; exact distinct remains the default for backward
  compatibility, while large datasets can opt out of exact cardinality work.
- Reworked validation hot paths to avoid string rendering for clean numeric and unique checks.
  Finding messages render values only when a violation needs user-facing evidence.
- Expanded typed validation and unique checks for signed integers, unsigned integers, floats, UTF-8,
  and timestamp arrays. Float uniqueness uses `to_bits()` semantics; nulls are ignored for unique.
- Removed string-parse numeric min/max fallback from validation; unsupported numeric-like types such
  as decimal are not coerced through display text.
- Added a rule-by-rule benchmark harness covering required/not-null, min/max, unique, full contract,
  fingerprint-only, and exact-distinct profile cases with raw timings, rows/sec, peak RSS, rule
  metadata, Arrow schema, and package versions.
- Recorded the historical 7.6M-row Windows baseline: validation 8.82s, Arrow validation 8.66s, full
  profile 47.47s.
- Added a fresh local Windows 7.6M-row a4 rule-matrix result under
  `benchmarks/results/windows-7_6m-a4.json`.
- Removed the `roaring` dependency from unique validation state.

## 0.4.0-alpha.3

- **Breaking:** the Rust API now returns a typed `ProofFrameError` (with `Arrow`, `Io`, `Regex`,
  `Json`, `InvalidContract`, `MissingColumn`, `UnsupportedType`, `SchemaMismatch`, `DuplicateKey`,
  `NoKeyColumns`, `CorruptData`, and `InvalidReceipt` variants) instead of `String`.
- Extended canonical fingerprinting to nested list, large-list, fixed-size-list, struct, and map
  columns using recursive, domain-separated encodings so nested data no longer fails closed.
- Added a pinned golden fingerprint test plus batch-invariance and data-sensitivity property tests
  that lock the `pf-fp-v1` contract.
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
