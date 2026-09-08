# Changelog

## 0.7.1

- Uniqueness no longer needs a writable directory to compare three rows. `ExactState`
  already had an in-memory path; the constructor demanded a spill target before a
  single value was read, which failed on read-only filesystems, locked-down
  containers and WebAssembly. The target is optional now: without one the segment
  claims as much of the memory budget as the allocator will give it and the scan
  stops with a resource limit rather than answering from partial state.
  `ExecutionOptions` and `SuggestOptions` carry the policy; `ExactState::new` takes a
  `PathBuf` or an `Option<PathBuf>`, so existing callers compile unchanged.

- Nine new rules. Each one hashes into `compiled_plan_digest` only when a plan
  carries it, so every contract written before today keeps the plan identity its
  receipts record, and each has a test for that.

  - `dataset_rules.monotonicity` — a column may not move against a declared
    direction. Ordering compares values, not the canonical byte form used for
    equality, which sorts negative integers and floats by their bit patterns. A null
    has no position in an order, so `skip` compares across the gap: `[5, null, 4]`
    still fails an increasing rule.
  - `dataset_rules.gap_detection` — consecutive values may not drift further apart
    than a declared step, with a tolerance and an allowance for how many gaps a
    dataset may contain. The step is stated in the column's own units and every
    finding names them, because `60` meaning nanoseconds when the author meant a
    minute is the mistake this rule exists to catch.
  - `dataset_rules.mutually_exclusive` — at most one, or exactly one, of a set of
    columns may be filled. Evaluated as bitmap algebra over Arrow's validity bits,
    which generalizes to any number of columns with no per-row counter.
  - `dataset_rules.sum` — a numeric total must stay inside declared bounds. Integers
    accumulate in `i128`, because `arrow::compute::sum` answers `i64::MAX + 1` with
    `-9223372036854775808`, silently. Floats accumulate in row order with Neumaier
    compensation, because the same function gives different totals for the same
    values depending on the batch size, and batch size is a reader setting rather
    than a property of the data.
  - `dataset_rules.mean` and `dataset_rules.std_dev` — bounds on a column's summary
    statistics, computed by Welford in one pass. A statistic of nothing is reported
    as having had no value rather than as zero, which would pass bounds it never
    earned.
  - `dataset_rules.conditional_unique` — uniqueness among the rows a condition
    selects. "Unique among the records that are not deleted" is a different claim
    from "unique", and stating it as the second one fails on every tombstone.
  - `columns.*.min_length` and `columns.*.max_length` — bounds on a text value's
    length in Unicode characters. Characters rather than bytes: a rule about an
    eight-character password should not depend on the alphabet it is written in.

- `dataset_rules.row_count_delta` bounds how far this dataset's row count may move
  from a reference dataset's, which is the check that catches a pipeline delivering
  half of yesterday. Only the count is read, so the reference is drained without
  building key state, and a reference with no rows is reported rather than divided
  by. An unbound dataset fails before the subject is scanned, because a comparison
  that never happened must not report as one that held.

- The WebAssembly build no longer cancels a whole analysis over one rule it cannot
  run. It runs the rest and says so: the result carries `status: "incomplete"` and
  the names of the rules that were skipped, it is never reported as valid, and it
  carries no evidence at all. Evidence is what a verifier trusts, and issuing it for
  a partial scan would be worse than refusing the run.

- Date bounds can be written as dates. `columns.d.min: "2024-01-01"` compiles against
  a `Date32` or `Date64` column; nobody knows that 2024-01-01 is day 19723. Day and
  millisecond counts still parse, so older contracts keep working, and an ambiguous
  spelling such as `2024-1-1` is refused rather than guessed at.

- The HTML review and the Markdown summary are rendered by the engine rather than by
  the Python package. They were Python-only, which meant a Rust or CLI user had no
  review at all, and any second surface that wanted one would have had to write a
  second renderer that could quietly disagree with the first. `review_html()` and
  `review_markdown()` are now part of the crate, Python calls them, and the copy in
  `proofframe/review_render.py` is gone. Fixtures covering escaping, non-ASCII,
  truncation and the disclosed contract pin the new output byte for byte against what
  the Python implementation produced.
- One deliberate difference comes with that move: the disclosed contract and the
  resource and metric rows are ordered by key rather than by whatever order the keys
  happened to be inserted in. The same contract now always produces the same document.
- `ProofFrameError::Review` reports a report or evidence document the renderer cannot
  use, instead of rendering a partial page around a missing field.

## 0.7.0

- Add `accept_file()` and `proofframe accept`: an explicit, versioned acceptance
  decision over the existing scan. The answer is `accepted`, `rejected` or
  `unknown`, and `unknown` is a real answer rather than a soft pass — a missing
  file, a parse failure or an exhausted resource limit can never become
  `accepted`. No second validation engine was added; this is the existing
  evidence, read as a decision an application can act on.
- The acceptance bundle binds four identities together: the contract, the
  acceptance policy, the reading settings and the evidence. A signature, when one
  is requested, covers the whole payload rather than a part of it, and
  `verify_acceptance()` (`proofframe verify-acceptance`) checks it offline
  without rescanning the data. Verification proves the bundle is intact and
  signed by the expected key; it does not prove the publisher was honest about
  the file they scanned.
- `verify_acceptance()` checks the bundle's shape before it trusts any digest. A
  hash proves that what is present was not edited and says nothing about what is
  absent, so a payload whose decision or evidence had been deleted and whose hash
  had been recomputed previously reported `valid: true`. Every required field must
  now be present and recognized, the status must be one of the three, and a
  decision that claims the data was examined must carry the scan it was drawn
  from, in the shape the scanner writes it: every report and Evidence V2 field
  present with its own type, counts that are non-negative and agree with what they
  count, and the evidence schema this release binds. A present field of the wrong
  type is rejected at least as hard as a missing one, because a reader who sees
  `valid` goes on to use it. Only `unknown` may have no report, and it must have
  none rather than something else. This closes a shape gap in unsigned bundles; a
  signed bundle was never forgeable this way, because the signature covers the
  whole payload and could not be recomputed.
- An acceptance policy can require that specific columns actually had values
  evaluated, so a file cannot be accepted because nothing was asked of it. The
  limit is stated plainly: these counters are per column, not proof that every
  rule ran.
- CSV reading settings are explicit and recorded. Delimiter, encoding, decimal
  separator, column types and null tokens are chosen by the caller and written
  into the bundle under their own identity, so two systems reading the same file
  either agree or disagree visibly. `1.234` is not silently guessed.
- The review now reports which columns had no value for their rules to check. A
  contract can pass because nothing was wrong or because nothing was asked, and only
  the first is a result. The counts come from the same scan: Arrow keeps the null
  count as batch metadata, so nothing was added to the per-row kernels and the
  allocation contract is unchanged. The engine reports schema indices rather than
  names for that reason; the review resolves them from the schema it already had.
  The HTML report carries the same block, so neither artifact is quietly greener
  than the other.

- The review summary no longer escapes ProofFrame's own sentences. The Markdown is
  written for a CI job summary or a pull request description, and a reader who copied
  the suggested `--max-samples 20` out of it received a command that did not run.
  Values taken from the data or the contract are still escaped, and `.` and `-` are no
  longer escaped anywhere: they carry meaning only at the start of a line, which a
  value cannot reach because whitespace is collapsed before rendering.

- Add Python `review()` and `proofframe review`: one native validation/fingerprint
  scan produces an offline HTML review, Markdown summary, report JSON and Evidence V2.
- Show an actionable first sampled finding and exact totals; default to zero samples
  with explicit `--max-samples 20` guidance. No per-rule verdicts are inferred from samples.
- Bound combined output bytes, escape HTML/Markdown, snapshot the displayed contract,
  and publish completed bundles into new directories only.
- Explain invalid V2 column types and missing/numeric contract versions with field context.
- `review` exits 1 for violations. Existing `evidence` exit codes and protocols are unchanged.

## 0.6.0

- Added `dataset_rules.references`: every key in the validated dataset must also appear in a
  named reference dataset the caller binds through `references=` (or `--reference NAME=PATH`).
  Keys pair by position so the two sides may name the same identity differently, Arrow key types
  must match, `nulls` is `"skip"` or `"reject"`, and findings report the first row carrying each
  absent key. Resolution uses the existing bounded, spilling exact-set machinery and stays within
  the supplied resource limits; under `check_partitions` every key resolves against the whole
  reference exactly once.
- A reference rule is checked or the run fails: a declared reference with no bound dataset, and a
  bound dataset no rule uses, both raise `PF_REFERENCE_UNBOUND` before the subject is scanned,
  because a foreign key that is never evaluated reports as one that held.
- Validation reports now carry `references`, recording for each rule the dataset it resolved
  against, that dataset's `pf-fp-v2` fingerprint and row count, and the distinct keys checked and
  missing. Reference rules are appended to `compiled_plan_digest` only when a plan carries them,
  so contracts written before this release keep the plan identity their receipts recorded.
- Added review-required V2 contract suggestions for Arrow, pandas, and Polars data, with typed
  schema rules, exact optional uniqueness, bounded optional categories, and safe range omissions.
- Added `proofframe suggest` plus complete getting-started, contract, API, and concepts guides.
- Added runnable workflows, integration examples, contributor guidance, and a refreshed release banner.
- A range is omitted only when a column never decreased *and* actually increased. Constant columns
  and single-row samples satisfy `value >= last` without any risk of outgrowing an observed bound,
  so they now keep their suggested range instead of being reported as monotonic.

## 0.5.1

- Added strict Contract V2 cross-column comparisons and conditional assertions compiled to typed
  Arrow kernels without Python row evaluation or implicit casts.
- Added exact dataset row-count, null-ratio, distinct-count, distinct-ratio, and composite-unique
  rules across record-batch boundaries. Ratio boundaries use integer comparison rather than rounded
  division; exact identities spill through resource-accounted checksummed runs.
- Added deterministic bounded partition validation with stable finding order, global row offsets,
  exact cross-partition state, Python/Rust parity, and ordered verifiable partition manifests.
- Added a release benchmark v2 matrix for V1 regression, relational, conditional, dataset,
  in-memory/spill composite, fingerprint, allocation, and Python/native timing evidence.
- Added deterministic checksums, SPDX JSON SBOMs, and build/SBOM attestations for wheel, sdist, and
  crate subjects before trusted publication.
- Promoted the 0.5 line to a stable release with project-focused PyPI, crates.io, and GitHub
  documentation.
- Decomposed exact-run compaction, typed encoder selection, column scanning, batch inspection, and
  Python module registration into focused paths without changing the public API or fingerprint
  protocols.
- Hardened source archive verification with explicit path, content, duplicate-member, required-file,
  and executable-resolution checks.
- Removed overlapping Python exception handling and other analyzer findings while preserving the
  0.5 compatibility surface.
- Synchronized Python, Rust, source-distribution, workflow, and release metadata for 0.5.1.

## 0.5.0

- Added `check_with_evidence`, which validates and fingerprints the same Arrow batches in one
  execution; the caller-supplied report assembler is now explicitly named
  `assemble_evidence_unchecked`.
- Evidence V2 now binds canonical result, full report, findings, and metrics digests and validates
  semantic consistency before signing.
- Exact spill runs now use bounded 32-way hierarchical compaction and close write handles before
  merge fan-in.
- Release source ZIP and sdist artifacts are checked for generated binaries, local paths, unsafe
  members, and incomplete source trees.
- Pandas and Polars now feed exact row/logical-byte hints into the native engine while retaining
  Arrow C Stream ingestion; Arrow `Utf8View` and `BinaryView` columns are supported directly.
- Timestamp contract bounds now accept exact offset-qualified ISO-8601/RFC 3339 values, normalize
  offsets to UTC, and reject sub-unit precision instead of rounding.
- Exact operations expose `spill="auto"|"never"`; known small keyed diffs avoid data partitions,
  while never-spill mode fails closed when the configured memory budget is insufficient.

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

### 0.7.0 acceptance completion

- Added `accept_file` and `verify_acceptance`, versioned acceptance policies, minimum native column evaluation requirements, and unknown outcomes for incomplete scans.
- Added explicit streaming CSV delimiter, encoding, decimal, type and null settings recorded in acceptance bundles.
- Added `accept` and `verify-acceptance` commands, exclusive atomic bundle publication, and optional whole-payload Ed25519 signatures through `proofframe[signing]`.
- Existing `review`, Evidence V2 and receipt APIs remain unchanged. Acceptance verification is an integrity/authentication operation, not a rescan.
