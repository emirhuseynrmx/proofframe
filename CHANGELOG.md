# Changelog

- Added `check_with_evidence`, which validates and fingerprints the same Arrow batches in one
  execution; the caller-supplied report assembler is now explicitly named
  `assemble_evidence_unchecked`.
- Evidence V2 now binds canonical result, full report, findings, and metrics digests and validates
  semantic consistency before signing.
- Exact spill runs now use bounded 32-way hierarchical compaction and close write handles before
  merge fan-in.
- Release source ZIP and sdist artifacts are checked for generated binaries, local paths, unsafe
  members, and incomplete source trees.

## 0.5.0

- Added a strict, versioned contract AST and schema compiler. Unknown fields, out-of-range bounds,
  invalid timestamp/decimal literals, and incompatible rule/type pairs fail before scanning.
- Added typed, column-specialized execution kernels and allocation contracts for primitive clean
  paths. Exact uniqueness uses hierarchical memory/temp accounts and checksummed sorted spill runs.
- Preserved `pf-fp-v1` and added explicit `pf-fp-v2` segmented canonical encoding with golden,
  batch-invariance, differential, and allocation tests.
- Hardened exact keyed diff with versioned partition headers, schema binding, checksums, pre-allocation
  length caps, bounded samples, atomic full-output sinks, and corruption tests.
- Added Evidence V2 and Receipt V2 as the default Python/CLI signing path. Dataset, canonical
  contract source, compiled plan, Arrow schema, engine, resource limits, and result are bound;
  cryptographic validity and signer trust are reported separately. V1 signing is explicit opt-in.
- Changed PII evidence to full 256-bit keyed BLAKE3 with unlinkable per-run and caller-keyed stable
  modes. Secrets are never serialized; reports expose only mode and key ID.
- Routed exact profile distinct state through the bounded spill operator and changed the profile
  default to `distinct="none"`.
- Capped validation findings by both contract and resource sample limits, charged retained finding
  memory, rejected schema-changing Arrow batches, and made missing required columns compile errors.
- Split Python I/O, Arrow, and persisted-corruption exceptions while preserving stable error codes.
- Gated publishing on exact-tag CI evidence and added `cargo publish --dry-run --locked`.
- Replaced the Python JSON-string round trip with native dictionaries, stable typed exceptions, Arrow
  C Stream preference, and GIL-detached native scans.
- Made CSV/Parquet CLI paths streaming, added atomic output, and fixed exit codes 0–4.
- Added subprocess RSS and native allocation/spill benchmark gates, pinned dataset manifests, artifact
  comparison guards, Miri-compatible tests, three fuzz targets, immutable CI action SHAs, Python
  3.10–3.13 cross-platform wheel tests, and release metadata validation.
- Retained 0.4 Python and Rust compatibility entry points for the 0.5 migration window.

This release is classified Beta until the pinned 7,645,034-row Bitcoin fixture and the 0.4 baseline
are captured on the same dedicated runner. Synthetic smoke results are not presented as real-data
performance claims.

## 0.4.0-alpha.5

- Prepared PyPI Trusted Publishing for portable ABI3 wheels across Linux x86_64, Linux ARM64,
  macOS universal2, and Windows.
- Forced the publish and wheel workflows to build Linux artifacts with manylinux2014 / PyPI
  compatibility so Python 3.10+ consumers can install wheels without compiling Rust locally.
- Added manual `workflow_dispatch` support to the PyPI publish workflow.

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
