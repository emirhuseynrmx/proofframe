# ProofFrame 0.6.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship ProofFrame 0.6.0 with reviewable, resource-bounded contract suggestions plus complete adoption and release assets.

**Architecture:** A new Rust one-pass suggestion scanner owns exact typed extrema, fingerprinting, optional exact-distinct state, bounded category candidates, and monotonicity. V2 contract metadata stays operational: central compilation rejects drafts, compiled-plan identity excludes it, and canonical source identity preserves it.

**Tech Stack:** Rust 1.85, Arrow 59, PyO3, Python 3.10+, PyArrow, pytest, GitHub Actions.

## Global Constraints

- The release version and tag are exactly `0.6.0` and `v0.6.0`.
- Existing V1/V2 contracts remain valid; absent V2 `status` means `active`.
- `status: "draft"` fails before record-batch consumption with `PF_DRAFT_CONTRACT`.
- `compiled_plan_digest` excludes `status` and `suggested_from`; `contract_source_digest` includes both.
- Existing 0.5.1 source-digest golden vectors remain byte-identical.
- Suggestion defaults: uniqueness, categories, and required columns are opt-in.
- Exact-distinct inference obeys existing memory, temporary-storage, and spill limits.
- Every example and documentation snippet is tested in CI.

---

### Task 1: Add V2 draft metadata and a central activation guard

**Files:**
- Modify: `src/contract/v2.rs`
- Modify: `src/contract/document.rs`
- Modify: `src/contract/compile.rs`
- Modify: `src/contract/mod.rs`
- Modify: `src/error.rs`
- Modify: `src/python.rs`
- Test: `tests/contract_compile.rs`, `tests/test_contract_v2.py`, `tests/test_python_surface.py`

**Interfaces:**
- Produces `ContractStatus::{Active,Draft}`, `SuggestedFromAst`, `SuggestedReviewAst`, and `ContractDocument::ensure_active() -> Result<(), ProofFrameError>`.
- Produces `ErrorCode::ContractDraft` with external code `PF_DRAFT_CONTRACT`.

- [ ] **Step 1: Add failing API coverage for every public validation entry point**

~~~python
@pytest.mark.parametrize("run", [
    lambda d, c: proofframe.check(d, c),
    lambda d, c: proofframe.check_with_evidence(d, c),
    lambda d, c: proofframe.check_partitions([d], c),
    lambda d, c: proofframe.check_partitions_with_evidence([d], c),
    lambda d, c: proofframe.validate(d, c),
])
def test_draft_contract_is_rejected(run):
    with pytest.raises(proofframe.ContractError) as error:
        run(pa.table({"id": [1]}), {"version": "proofframe.contract.v2", "status": "draft"})
    assert error.value.code == "PF_DRAFT_CONTRACT"
~~~

- [ ] **Step 2: Run it to prove the boundary is missing**

Run: `python -m pytest tests/test_contract_v2.py tests/test_python_surface.py -q`

Expected: FAIL because V2 has no recognized draft status or code.

- [ ] **Step 3: Add strict metadata parsing with compatible defaults**

~~~rust
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    #[default]
    Active,
    Draft,
}
~~~

Add `status: ContractStatus` and optional `suggested_from` to `ContractAstV2`; add both root fields to V2 known-field validation. Make provenance contain engine version, fingerprint, observed rows, uniqueness inference, and sorted review records.

- [ ] **Step 4: Enforce the guard at the single compile boundary**

~~~rust
pub fn compile_document(document: &ContractDocument, schema: &Schema) -> Result<Self, ProofFrameError> {
    document.ensure_active()?;
    match document { /* existing V1/V2 compilation */ }
}
~~~

Use a structured contract error with `ErrorCode::ContractDraft` and text telling users to review rules and set `status` to `active`.

- [ ] **Step 5: Map the error consistently and verify**

Add `PF_DRAFT_CONTRACT` in `ErrorCode::as_str` and map it to Python `ContractError`.

Run: `cargo test --locked --test contract_compile && python -m pytest tests/test_contract_v2.py tests/test_api.py tests/test_python_surface.py tests/test_cli_streaming.py -q`

Expected: PASS; absent and active statuses execute, every draft route fails before scanning.

- [ ] **Step 6: Commit**

~~~bash
git add src/contract src/error.rs src/python.rs tests/contract_compile.rs tests/test_contract_v2.py tests/test_api.py tests/test_python_surface.py tests/test_cli_streaming.py
git commit -m "feat: reject unreviewed draft contracts"
~~~

### Task 2: Freeze source and plan digest behavior

**Files:**
- Modify: `tests/contract_compile.rs`
- Modify: `tests/evidence_receipt.rs`
- Modify: `tests/test_contract_v2.py`

**Interfaces:**
- Consumes `contract_source_digest` and `CompiledContract::compiled_plan_digest`.
- Produces fixed 0.5.1 source-digest and draft-to-active invariants.

- [ ] **Step 1: Write literal golden-vector tests**

~~~rust
#[test]
fn v051_contract_source_digest_is_frozen() {
    assert_eq!(contract_source_digest(LEGACY_V2_SOURCE).unwrap(), LEGACY_V2_DIGEST);
}

#[test]
fn activation_changes_source_not_plan_identity() {
    assert_eq!(draft_plan_digest, active_plan_digest);
    assert_ne!(draft_source_digest, active_source_digest);
}
~~~

Use an independently recorded 0.5.1 V2 contract and digest; never compute the expected vector from the test subject.

- [ ] **Step 2: Run the focused tests**

Run: `cargo test --locked --test contract_compile --test evidence_receipt`

Expected: FAIL until the constants and intended metadata boundary are in place.

- [ ] **Step 3: Preserve both digest contracts**

Do not change `contract_source_digest()`, its RFC 8785 canonicalization, or its V1/V2 domain separators. Do not add operational metadata to `compiled_plan_digest_v2()`.

- [ ] **Step 4: Prove both identities are embedded in evidence**

~~~python
checked = proofframe.check_with_evidence(table, active_contract)
assert checked["evidence"]["contract_source_digest"].startswith("pf-contract-v2:")
assert checked["evidence"]["compiled_plan_digest"].startswith("pf-plan-v2:")
~~~

Run: `cargo test --locked --test contract_compile --test evidence_receipt && python -m pytest tests/test_contract_v2.py -q`

Expected: PASS.

- [ ] **Step 5: Commit**

~~~bash
git add tests/contract_compile.rs tests/evidence_receipt.rs tests/test_contract_v2.py
git commit -m "test: freeze contract digest compatibility"
~~~

### Task 3: Build the native bounded suggestion scanner

**Files:**
- Create: `src/suggest.rs`
- Modify: `src/lib.rs`
- Modify: `src/python.rs`
- Test: `tests/execution_contract.rs`, `tests/resource_limits.rs`, `tests/distinct_spill.rs`

**Interfaces:**
- Produces `SuggestOptions` and `suggest_reader_with_options(reader, options, row_count_hint) -> Result<serde_json::Value, ProofFrameError>`.
- Produces native PyO3 function `suggest_arrow(...)`.

- [ ] **Step 1: Add failing exact-bound and safe-omission tests**

~~~rust
assert_eq!(suggested["columns"]["id"]["min"], json!(9_007_199_254_740_993_i64));
assert!(suggested["columns"]["event_at"].get("max").is_none());
assert!(suggested["suggested_from"]["review"].as_array().unwrap()
    .iter().any(|item| item["reason"] == "timestamp_range_omitted"));
~~~

Cover exact int64 above 2^53, uint64, decimals, timestamps, cross-batch monotonicity, null-only columns, zero-width ranges, and resource exhaustion.

- [ ] **Step 2: Run the missing-engine tests**

Run: `cargo test --locked --test execution_contract --test resource_limits --test distinct_spill`

Expected: FAIL because `SuggestOptions` and scanner do not exist.

- [ ] **Step 3: Define the resource-safe interface**

~~~rust
pub struct SuggestOptions {
    pub infer_uniqueness: bool,
    pub infer_categories: bool,
    pub max_categories: usize,
    pub infer_required: bool,
    pub infer_ranges: bool,
    pub range_tolerance: f64,
    pub infer_row_count: bool,
    pub resources: ResourceLimits,
}
~~~

Reject zero `max_categories`, negative tolerance, and non-finite tolerance before consuming a batch.

- [ ] **Step 4: Implement one-pass typed observation**

Track schema, canonical V1 fingerprint, rows, null counts, exact typed extrema, and non-null non-decreasing order across batches. Allocate `ExactState` only for `infer_uniqueness`. Retain only UTF-8 candidates up to `max_categories + 1`, then permanently discard that candidate state.

- [ ] **Step 5: Emit deterministic draft V2 JSON**

~~~json
{
  "version": "proofframe.contract.v2",
  "status": "draft",
  "columns": {},
  "dataset_rules": {},
  "suggested_from": {"proofframe_version": "0.6.0"}
}
~~~

Emit physical `type`, observed `not_null`, opt-in `required`, opt-in `allowed`, opt-in `distinct_ratio.min = 1.0`, optional `row_count.min`, and sorted review entries. Omit ranges for timestamps and monotonic numeric columns; otherwise expand bounds by `(max - min) * range_tolerance`.

- [ ] **Step 6: Register the PyO3 hook and verify**

Register `suggest_arrow` beside `profile_arrow` and serialize its JSON value with `serialize_to_python`.

Run: `cargo test --locked --all-targets --all-features`

Expected: PASS; suggestion state spills or fails closed under existing limits and no f64 conversion is used for generated bounds.

- [ ] **Step 7: Commit**

~~~bash
git add src/suggest.rs src/lib.rs src/python.rs tests/execution_contract.rs tests/resource_limits.rs tests/distinct_spill.rs
git commit -m "feat: add bounded native contract suggestions"
~~~

### Task 4: Publish the Python API and CLI command

**Files:**
- Modify: `python/proofframe/api.py`
- Modify: `python/proofframe/__init__.py`
- Modify: `python/proofframe/cli.py`
- Test: `tests/test_api.py`, `tests/test_python_surface.py`, `tests/test_cli_streaming.py`

**Interfaces:**
- Produces `proofframe.suggest_contract()`, the fifteenth public Python function.
- Produces `proofframe suggest DATA` with matching policy/resource arguments.

- [ ] **Step 1: Write failing Python defaults coverage**

~~~python
draft = proofframe.suggest_contract(pa.table({"id": [1, 2], "state": ["new", "paid"]}))
assert draft["status"] == "draft"
assert draft["suggested_from"]["uniqueness_inferred"] is False
assert "distinct_ratio" not in draft["dataset_rules"]
~~~

- [ ] **Step 2: Implement the wrapper and export**

~~~python
def suggest_contract(data: Any, *, infer_uniqueness: bool = False,
                     infer_categories: bool = False, max_categories: int = 20,
                     infer_required: bool = False, infer_ranges: bool = True,
                     range_tolerance: float = 0.0, infer_row_count: bool = True,
                     max_memory: int = 512 * 1024 * 1024,
                     max_temp: int = 4 * 1024 * 1024 * 1024,
                     spill: str = "auto") -> dict[str, Any]:
    ...
~~~

Validate policy values before `_as_reader`, pass the existing row-count hint, and export it through `__init__.__all__`.

- [ ] **Step 3: Add parser, command dispatch, and stdout test**

~~~python
cli.main(["suggest", str(data_path), "--infer-uniqueness", "--max-memory", "64MiB"])
draft = json.loads(capsys.readouterr().out)
assert draft["status"] == "draft"
~~~

Add `--infer-uniqueness`, `--infer-categories`, `--max-categories`, `--infer-required`, `--no-infer-ranges`, `--range-tolerance`, `--no-infer-row-count`, stream options, and resource options. Leave stdout as contract JSON and stderr as diagnostics.

- [ ] **Step 4: Verify all public routes**

Run: `python -m pytest tests/test_api.py tests/test_python_surface.py tests/test_cli_streaming.py -q`

Expected: PASS for Parquet, CSV, policy errors, stderr errors, and draft JSON output.

- [ ] **Step 5: Commit**

~~~bash
git add python/proofframe/api.py python/proofframe/__init__.py python/proofframe/cli.py tests/test_api.py tests/test_python_surface.py tests/test_cli_streaming.py
git commit -m "feat: expose contract suggestions in Python and CLI"
~~~

### Task 5: Align version, license, package metadata, and banner

**Files:**
- Modify: `Cargo.toml`, `pyproject.toml`, `CHANGELOG.md`, `LICENSE`, `assets/banner.png`
- Modify: `README.md`, `README-crates.md`, `.github/workflows/ci.yml`
- Modify: `tests/test_release_metadata.py`, `tests/evidence_receipt.rs`

**Interfaces:**
- Produces fully aligned `0.6.0` release metadata and v0.6.0 visual asset.

- [ ] **Step 1: Add failing release assertions**

~~~python
assert read_versions(ROOT) == ("0.6.0", "0.6.0")
verify_versions(ROOT, "v0.6.0")
assert "TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION" in (ROOT / "LICENSE").read_text()
~~~

- [ ] **Step 2: Update every release-bound string**

Set Cargo and Python versions to `0.6.0`; update CI wheel/sdist names, install snippets, changelog, release tests, and engine-version evidence fixtures. Align package descriptions and keywords around Arrow, contracts, quality, validation, fingerprints, evidence, pandas, polars, and parquet.

- [ ] **Step 3: Replace immutable release assets**

Replace the abbreviated notice with the official full Apache-2.0 text. Replace `assets/banner.png` with the supplied v0.6.0 banner; retain the existing raw-GitHub absolute image URLs in both READMEs.

- [ ] **Step 4: Verify packaging**

Run: `python -m pytest tests/test_release_metadata.py tests/test_source_artifacts.py -q && cargo package --locked`

Expected: PASS; no stale release artifact names and full license is packaged.

- [ ] **Step 5: Commit**

~~~bash
git add Cargo.toml pyproject.toml CHANGELOG.md LICENSE assets/banner.png README.md README-crates.md .github/workflows/ci.yml tests/test_release_metadata.py tests/evidence_receipt.rs
git commit -m "release: prepare ProofFrame 0.6.0"
~~~

### Task 6: Add reference documentation, six executable examples, and positioning

**Files:**
- Create: `docs/getting-started.md`, `docs/contracts.md`, `docs/api.md`, `docs/concepts.md`
- Create: `examples/pandas_validation.py`, `examples/polars_parquet_budget.py`, `examples/keyed_diff.py`, `examples/pii_and_leakage.py`, `examples/evidence_and_receipt.py`, `examples/suggest_review_check.py`
- Modify: `README.md`, `README-crates.md`, `tests/test_readme_examples.py`

**Interfaces:**
- Produces complete docs for all fifteen Python functions and six CI-executed workflows.

- [ ] **Step 1: Add failing documentation and example manifest tests**

~~~python
for name in ("getting-started.md", "contracts.md", "api.md", "concepts.md"):
    assert (ROOT / "docs" / name).is_file()
for example in ("pandas_validation.py", "polars_parquet_budget.py", "keyed_diff.py",
                "pii_and_leakage.py", "evidence_and_receipt.py", "suggest_review_check.py"):
    subprocess.run([sys.executable, ROOT / "examples" / example], check=True)
~~~

- [ ] **Step 2: Write user-path documentation**

Write a five-minute first check and suggest-review-check guide; a field-complete V1/V2 contract reference; a fifteen-function API reference with signatures and errors; and concepts covering fingerprint, plan/source digests, evidence, receipt, partitions, spill, and limits.

- [ ] **Step 3: Implement six isolated examples**

Use temporary directories and assertions. Cover pandas validation; Polars plus Parquet/resource budgets; keyed diff; PII/leakage; evidence plus Ed25519 receipt verification; and a draft rejected before activation followed by a successful check.

- [ ] **Step 4: Add honest competitor positioning**

State ProofFrame strengths as exact resource-bounded contracts and verifiable evidence. State that Pandera, Great Expectations, Soda, and Deequ have broader established ecosystems, integrations, and familiar workflows in their strongest areas; do not claim feature parity.

- [ ] **Step 5: Execute docs/examples**

Run: `python -m pytest tests/test_readme_examples.py -q`

Expected: PASS; every listed script and README snippet runs.

- [ ] **Step 6: Commit**

~~~bash
git add docs examples README.md README-crates.md tests/test_readme_examples.py
git commit -m "docs: add ProofFrame 0.6.0 adoption guides"
~~~

### Task 7: Add integrations and community hygiene

**Files:**
- Create: `integrations/airflow/proofframe_validation_dag.py`
- Create: `integrations/dbt/macros/proofframe_validate.sql`, `integrations/dbt/README.md`
- Create: `integrations/github-actions/proofframe-validate.yml`
- Create: `CONTRIBUTING.md`, `.github/ISSUE_TEMPLATE/bug_report.yml`, `.github/ISSUE_TEMPLATE/feature_request.yml`
- Modify: `tests/test_readme_examples.py`

**Interfaces:**
- Produces copyable Airflow, dbt, and PR-validation integrations plus actionable contribution forms.

- [ ] **Step 1: Add static integration assertions**

~~~python
assert "proofframe check" in (ROOT / "integrations/github-actions/proofframe-validate.yml").read_text()
assert "cargo test --locked --all-targets --all-features" in (ROOT / "CONTRIBUTING.md").read_text()
~~~

- [ ] **Step 2: Implement the three integrations**

Airflow invokes `proofframe check` on a produced Parquet path and fails on a nonzero result. dbt provides a macro/operation that invokes the CLI against a configured model artifact. GitHub Actions installs ProofFrame, checks fixture data on pull requests, and uploads success evidence.

- [ ] **Step 3: Write contribution and issue guidance**

Document supported platforms, format/test/release commands, benchmark policy, security channel, and no-secrets rule. Require reproducible data/schema/contract/environment details for bugs and problem/acceptance criteria for features.

- [ ] **Step 4: Verify paths and commit**

Run: `python -m pytest tests/test_readme_examples.py tests/test_release_metadata.py -q`

Expected: PASS.

~~~bash
git add integrations CONTRIBUTING.md .github/ISSUE_TEMPLATE tests
git commit -m "docs: add integrations and contribution guidance"
~~~

### Task 8: Record current benchmark evidence and release verification

**Files:**
- Create: `benchmarks/results/v0.6.0-release-gate.json`
- Modify: `docs/testing.md`, `README.md`, `tests/test_release_gate.py`

**Interfaces:**
- Produces a seven-sample 0.6.0 artifact with exact environment and dataset provenance.

- [ ] **Step 1: Add a failing release-artifact test**

~~~python
artifact = json.loads((ROOT / "benchmarks/results/v0.6.0-release-gate.json").read_text())
validate_artifact(artifact)
assert artifact["runtime"]["proofframe"] == "0.6.0"
~~~

- [ ] **Step 2: Build and install the candidate**

Run: `maturin build --release --locked --out target/benchmark-wheel --compatibility pypi`

Run: `python -m pip install --no-deps --force-reinstall target/benchmark-wheel/*.whl`

Expected: installed package reports `0.6.0`.

- [ ] **Step 3: Produce and commit the seven-run artifact**

Run: `python benchmarks/release_gate.py --rows 1000000 --runs 7 --warmups 1 --output benchmarks/results/v0.6.0-release-gate.json`

Document whether it used synthetic or pinned real data, SHA-256 identity, host/compiler metadata, and that it is a release-gate result rather than a cross-machine claim.

- [ ] **Step 4: Run the complete release verification**

Run: `cargo fmt --all -- --check && cargo clippy --locked --all-targets --all-features -- -D warnings && cargo test --locked --all-targets --all-features`

Run: `maturin build --release --locked --out target/final-wheel --compatibility pypi && python -m pip install --no-deps --force-reinstall target/final-wheel/*.whl`

Run: `ruff check python tests benchmarks scripts && python -m pytest -q --basetemp target/pytest-final --cov=proofframe --cov-report=term-missing --cov-fail-under=85`

Run: `maturin sdist --out target/final-sdist && python scripts/source_artifacts.py --output target/final-sdist --verify target/final-sdist/proofframe-0.6.0.tar.gz && python -m twine check target/final-sdist/proofframe-0.6.0.tar.gz && cargo package --locked`

Expected: all commands PASS and the worktree is clean after committed results.

- [ ] **Step 5: Commit benchmark evidence**

~~~bash
git add benchmarks/results/v0.6.0-release-gate.json docs/testing.md README.md tests/test_release_gate.py
git commit -m "bench: record ProofFrame 0.6.0 release gate"
~~~

