# ProofFrame 0.6.0 Design

## Goal

Make ProofFrame 0.6.0 the easiest version to evaluate, adopt, and operate without weakening its exact, resource-bounded, fail-closed model. The release adds an explicitly reviewable contract-suggestion workflow, complete user documentation and examples, practical integrations, current quality evidence, release hygiene, and the v0.6.0 banner.

## Release scope

0.6.0 contains all of the following:

- A complete Apache-2.0 `LICENSE` file so hosting and dependency scanners recognize the license.
- Python `suggest_contract()` and the `proofframe suggest` CLI subcommand.
- Contract, API, getting-started, and concepts documentation.
- Six runnable examples: pandas validation; Polars/Parquet with resource budgets; keyed diff; PII and leakage; evidence plus a signed receipt; and suggest-review-check.
- A candid README comparison with Pandera, Great Expectations, Soda, and Deequ.
- Airflow, dbt, and GitHub Actions integration examples.
- `CONTRIBUTING.md`, issue templates, and aligned PyPI/crates.io descriptions and keywords.
- Updated benchmarks, clearer finding output, and actionable error messages.
- The supplied v0.6.0 banner in `assets/banner.png`.

## Contract suggestion workflow

### Public API

The Python entry point is:

```python
suggest_contract(
    data,
    *,
    infer_uniqueness: bool = False,
    infer_categories: bool = False,
    max_categories: int = 20,
    infer_required: bool = False,
    infer_ranges: bool = True,
    range_tolerance: float = 0.0,
    infer_row_count: bool = True,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    spill: str = "auto",
) -> dict[str, Any]
```

`proofframe suggest DATA` exposes the same policy and resource options and writes only the generated JSON contract to stdout. Diagnostics and errors go to stderr; shell redirection therefore remains safe:

```bash
proofframe suggest data.parquet > contract.json
```

The command accepts the CLI's existing CSV and Parquet streaming inputs. It does not materialize a table or silently relax resource limits.

### Scanner and resource model

Suggestion uses a dedicated native Arrow scanner instead of deriving bounds from `profile()`. The existing profile serializes extrema as `f64`, which cannot safely represent all `int64`, `uint64`, or timestamp values. The native scanner preserves each suggested bound in the column's physical Arrow type and serializes an exact V2 contract literal.

The scanner processes record batches once and tracks schema, the canonical dataset fingerprint, observed row count, null counts, exact typed extrema, and monotonicity. It only allocates exact-distinct state when `infer_uniqueness=True`. That state uses the existing hard memory/temp budgets and spill behavior; the option is deliberately off by default because it may be expensive on large data.

For category inference, `infer_categories=True` tracks UTF-8 values only until the count exceeds `max_categories`. At that point it drops the candidate set and emits no `allowed` rule for that column. This is bounded and does not require a whole-dataset exact-distinct pass. `max_categories` must be a positive integer.

### Suggested rules

Every suggested document uses `proofframe.contract.v2` and records the physical Arrow type for each observed column.

- A column with no observed nulls receives `not_null: true`.
- `required` is emitted only when `infer_required=True`; it is otherwise absent.
- A uniqueness rule is emitted only when `infer_uniqueness=True` and the exact distinct count equals the observed row count. It is represented as `dataset_rules.distinct_ratio.<column>.min = 1.0`.
- A UTF-8 `allowed` set is emitted only when `infer_categories=True` and the exact bounded candidate set remains at or below `max_categories`.
- `dataset_rules.row_count.min` is emitted from the observed count when `infer_row_count=True`.
- Numeric ranges are emitted when `infer_ranges=True`, the values are finite, and the column is neither a timestamp nor monotonically increasing. Monotonicity means the non-null physical values never decrease in their source order, including across record-batch boundaries. Bounds are exact observed extrema when `range_tolerance=0.0`; otherwise the scanner expands each bound by `(observed_max - observed_min) * range_tolerance`. A zero-width observed range remains zero-width. Timestamps and monotonically increasing numeric columns receive no range rule and are recorded for review.

The resulting contract is a draft. It does not assert that the data are valid, and its rules are intentionally opt-in where they can be fragile across normal dataset evolution.

### Draft provenance and activation

Suggested contracts include:

```json
{
  "version": "proofframe.contract.v2",
  "status": "draft",
  "suggested_from": {
    "proofframe_version": "0.6.0",
    "dataset_fingerprint": "pf-fp-v1:...",
    "rows_observed": 7645034,
    "uniqueness_inferred": false,
    "review": [
      {"column": "created_at", "reason": "timestamp_range_omitted"}
    ]
  }
}
```

V2 accepts `status: "draft"` and `status: "active"`; an omitted status defaults to `active` for 0.5.1 compatibility. `suggested_from` is optional metadata with a strict schema. A draft is rejected before any data scan with a distinct, searchable `PF_DRAFT_CONTRACT` error. The user reviews the generated JSON and changes `status` to `active` before use.

The rejection must be enforced by the common V2 document-compile path so it covers `check`, `check_with_evidence`, `check_partitions`, `check_partitions_with_evidence`, `validate`, and their CLI routes without API-specific checks.

## Evidence identity and compatibility

ProofFrame keeps two intentional identities:

- `compiled_plan_digest` identifies the executable rule set. It excludes `status` and `suggested_from`, so changing a reviewed document from draft to active does not change the rules identity.
- `contract_source_digest` identifies the exact RFC 8785-canonical source document. It includes operational status and provenance, so it changes when the reviewed source changes.

Both digests remain in reports and evidence. This lets a verifier establish that two runs executed the same rules while also seeing whether they used different reviewed source documents.

The pre-0.6 canonicalization routine is frozen. A golden vector from a 0.5.1 contract must produce byte-identical `contract_source_digest` values in 0.6.0. Tests also prove that an omitted `status` remains compatible, `draft` is rejected on every entry point before consumption, and draft-to-active preserves the compiled-plan digest while changing the source digest.

## Documentation and examples

The documentation is organized around a new user path rather than internal modules:

1. **Getting started:** installation, a five-minute first check, interpreting valid/invalid output, and the suggest-review-check workflow.
2. **Contract reference:** complete V1/V2 field tables, allowed values, physical type syntax, row and dataset rules, null behavior, limits, source metadata, and draft activation.
3. **API reference:** all fifteen public Python functions, signatures, input forms, return shapes, exceptions, resource behavior, and short examples.
4. **Concepts:** fingerprints, compiled plans, contract-source digests, evidence, receipts, partitions, spill, and the two-digest distinction.

Every example is executable in CI and uses only declared project dependencies. The README links to the documentation, includes the suggest workflow, and compares ProofFrame honestly: its strengths are exact resource-bounded checks and verifiable evidence; competing tools have broader ecosystems, more integrations, or easier familiar workflows in their established domains.

## Integrations and community files

The release contains small, copyable integrations rather than framework abstractions:

- An Airflow DAG that validates a produced Parquet artifact and fails the task on a contract violation or engine error.
- A dbt project/example macro or operation that invokes ProofFrame against a model artifact and exposes the JSON result in logs.
- A GitHub Actions workflow that installs ProofFrame, validates checked-in fixture data on pull requests, and uploads evidence as an artifact.

`CONTRIBUTING.md` documents supported environments, local checks, test commands, issue expectations, and how to report security issues. Bug and feature issue templates capture reproducible inputs, observed behavior, environment, and desired outcome. Package descriptions and keywords describe the same validated capabilities on PyPI and crates.io.

## Quality, errors, and release assets

The existing benchmark harness is rerun against the 0.6.0 release candidate with its dataset identity, machine metadata, package versions, and raw result retained. Published text must identify whether a result is a smoke or dedicated-runner comparison and may not reuse the old alpha benchmark as a current release claim.

Finding output is reviewed for stable ordering, bounded samples plus exact counts, rule/path clarity, and JSON usability. Error messages name the error code, failed operation, relevant contract path or input, and a direct corrective action while preserving the existing exit-code contract.

`assets/banner.png` is replaced with the supplied v0.6.0 image. Both `README.md` and `README-crates.md` already reference the same absolute GitHub raw URL, so no README image-link change is required.

All version-bearing release metadata moves from 0.5.1 to 0.6.0, including Cargo, Python package metadata, release tests, documentation, changelog, and release workflow expectations. The complete Apache-2.0 text replaces the abbreviated license notice.

## Testing and acceptance criteria

- Native and Python tests cover every suggestion flag, resource failure, type output, exact extrema above `2^53`, timestamp omission, monotonic numeric omission, category cut-off, zero-null behavior, and exact uniqueness.
- CLI tests prove stdout is valid contract JSON, diagnostics remain on stderr, CSV and Parquet work, and invalid options return the established usage code.
- Contract parser and execution tests cover missing/active/draft status across every entry point, before the first batch is consumed.
- Golden tests freeze legacy source digests and prove the two-digest behavior for draft-to-active conversion.
- Documentation snippets and all six examples run in CI.
- Existing Rust, Python, release, artifact, and benchmark gates pass with the 0.6.0 metadata.
