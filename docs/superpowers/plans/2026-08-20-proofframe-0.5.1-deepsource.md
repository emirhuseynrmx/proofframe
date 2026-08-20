# ProofFrame 0.5.1 DeepSource Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship ProofFrame 0.5.1 from current `main` with the reported DeepSource Python, Rust, and Secrets gates green and with verified PyPI/crates.io release artifacts.

**Architecture:** Preserve all 0.5 public behavior while decomposing the five high-complexity functions into private, single-purpose helpers. Treat DeepSource as the structural regression test, existing Rust/Python suites as behavioral characterization, and the tag-bound CI workflow as the release authority.

**Tech Stack:** Rust 2024 / Rust 1.85, Arrow 59.1, PyO3 0.29, Python 3.10–3.13, maturin, pytest, Ruff, Clippy, DeepSource, GitHub Actions, PyPI, crates.io.

## Global Constraints

- Work only in `fix/0.5.1-deepsource`, based on `main`; never release from divergent branch `a`.
- Do not add `skipcq`, analyzer exclusions, lint allows, or blanket suppressions.
- Preserve the ProofFrame 0.5 public Python and Rust APIs and serialized proof formats.
- Keep benchmark claims unchanged and evidence-based.
- Publish only after CI and DeepSource are green on the exact `v0.5.1` commit.
- Never print or commit PyPI/crates.io credentials.

---

### Task 1: Establish the failing quality baseline

**Files:**
- Inspect: `.github/workflows/ci.yml`
- Inspect: `.github/workflows/publish.yml`
- Inspect: DeepSource run `14cc5967-ed89-4f6d-be5e-4fe5b7c50893`

**Interfaces:**
- Consumes: current `main` at `8b77b7f` and the user-reported DeepSource findings.
- Produces: a recorded baseline showing Python 10, Rust 13, and Secrets 2 findings before remediation.

- [ ] **Step 1: Run the current local quality gates**

Run:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
ruff check python tests benchmarks scripts
python -m pytest -q --basetemp target/pytest-baseline
```

Expected: local behavior gates pass; they do not replace the failing DeepSource structural baseline.

- [ ] **Step 2: Record exact DeepSource finding codes**

Confirm the run contains `PTC-W0043`, `PYL-W0714`, `PY-R1000`, `BAN-B607`, `PYL-R0201`, `RS-W1069`, `RS-R1000`, `RS-W1046`, `RS-W1049`, and `SCT-A000` before changing production code.

---

### Task 2: Correct the direct Python findings

**Files:**
- Modify: `python/proofframe/api.py`
- Modify: `python/proofframe/cli.py`
- Modify: `tests/test_python_surface.py`
- Test: `tests/test_api.py`

**Interfaces:**
- Consumes: `validate(data, contract, *, include_profile=True, **options)` and Arrow protocol fixtures.
- Produces: unchanged compatibility behavior without unnecessary deletion, overlapping exception types, or unbound fixture methods.

- [ ] **Step 1: Strengthen the compatibility regression**

In `tests/test_api.py`, ensure the existing validate compatibility test explicitly calls both `include_profile=True` and `include_profile=False` and asserts identical validation results.

- [ ] **Step 2: Verify the regression test before refactoring**

Run:

```powershell
python -m pytest tests/test_api.py -q -k "validate"
```

Expected: PASS, establishing preserved behavior for the structural cleanup.

- [ ] **Step 3: Apply the minimal direct fixes**

Keep the existing `validate` signature and warning, and make its body end with:

```python
return check(data, contract, **options)
```

Keep the keyword in the signature; remove only `del include_profile`. In `cli.py`, change the final tuple to:

```python
except (OSError, ValueError, TypeError) as error:
```

Decorate `ArrowConvertible.to_arrow`, both `__arrow_c_stream__` fixtures, and `BothArrowProtocols.to_arrow` with `@staticmethod`; retain `requested_schema=None` where the protocol accepts it.

- [ ] **Step 4: Run targeted Python tests and Ruff**

Run:

```powershell
python -m pytest tests/test_api.py tests/test_python_surface.py -q
ruff check python/proofframe/api.py python/proofframe/cli.py tests/test_python_surface.py
```

Expected: PASS with no Ruff findings.

- [ ] **Step 5: Commit the direct Python fixes**

```powershell
git add -- python/proofframe/api.py python/proofframe/cli.py tests/test_api.py tests/test_python_surface.py
git commit -m "fix: clean direct Python analyzer findings"
```

---

### Task 3: Decompose and secure source archive handling

**Files:**
- Modify: `scripts/source_artifacts.py`
- Modify: `tests/test_source_artifacts.py`

**Interfaces:**
- Consumes: `_members(path) -> list[tuple[str, bytes]]` and `verify_source_archive(path: Path, *, expected_root: str)`.
- Produces: `_validate_member_path`, `_validate_member_content`, `_validate_required_paths`, and `_git_executable` helpers; public archive functions and errors remain unchanged.

- [ ] **Step 1: Add a failing Git-resolution test**

Add:

```python
def test_build_source_zip_fails_closed_when_git_is_unavailable(tmp_path, monkeypatch):
    monkeypatch.setattr("scripts.source_artifacts._project_version", lambda _: "0.5.1")
    monkeypatch.setattr("scripts.source_artifacts.shutil.which", lambda _: None)
    with pytest.raises(ArchiveHygieneError, match="Git executable is unavailable"):
        build_source_zip(tmp_path, tmp_path / "dist")
```

Import `build_source_zip`. Run the test and expect failure because `shutil.which` is not yet used:

```powershell
python -m pytest tests/test_source_artifacts.py::test_build_source_zip_fails_closed_when_git_is_unavailable -q
```

- [ ] **Step 2: Add path/content characterization cases**

Extend parametrization to cover a backslash path, `..` escape, wrong release root, duplicate member, forbidden segment, forbidden suffix, local absolute path, and missing required path. Each must assert the current `ArchiveHygieneError` category text.

- [ ] **Step 3: Extract focused validators**

Implement these private interfaces and move the current checks into them without changing messages:

```python
def _validate_member_path(raw_name: str, expected_root: str) -> PurePosixPath:
    if "\\" in raw_name:
        raise ArchiveHygieneError(f"archive path is not POSIX-normalized: {raw_name}")
    member = PurePosixPath(raw_name)
    if member.is_absolute() or ".." in member.parts:
        raise ArchiveHygieneError(f"archive path escapes its root: {raw_name}")
    if not member.parts or member.parts[0] != expected_root:
        raise ArchiveHygieneError(f"archive member is outside {expected_root}: {raw_name}")
    return PurePosixPath(*member.parts[1:])


def _validate_member_content(raw_name: str, relative: PurePosixPath, content: bytes) -> str:
    if any(part in _FORBIDDEN_SEGMENTS for part in relative.parts):
        raise ArchiveHygieneError(f"archive contains generated directory: {raw_name}")
    if relative.name in _FORBIDDEN_NAMES or relative.suffix.lower() in _FORBIDDEN_SUFFIXES:
        raise ArchiveHygieneError(f"archive contains generated/native file: {raw_name}")
    normalized = relative.as_posix()
    if normalized not in _LOCAL_PATH_SCAN_EXEMPT and any(pattern.search(content) for pattern in _LOCAL_PATHS):
        raise ArchiveHygieneError(f"archive contains a local absolute path: {raw_name}")
    return normalized


def _validate_required_paths(relative_paths: set[str]) -> None:
    missing = sorted(_REQUIRED_PATHS - relative_paths)
    if missing:
        raise ArchiveHygieneError(f"source archive is incomplete: missing {', '.join(missing)}")


def _git_executable() -> str:
    executable = shutil.which("git")
    if executable is None:
        raise ArchiveHygieneError("Git executable is unavailable")
    return str(Path(executable).resolve())
```

`verify_source_archive` must only orchestrate member loading, helper calls, duplicate tracking, and required-path validation. `_git_executable` must call `shutil.which("git")`, return the absolute resolved path, or raise `ArchiveHygieneError`.

- [ ] **Step 4: Use the resolved executable**

Replace the partial executable call with:

```python
tracked = subprocess.run(
    [_git_executable(), "ls-files", "-z"],
    cwd=root,
    check=True,
    capture_output=True,
).stdout.split(b"\0")
```

- [ ] **Step 5: Run source-artifact tests and quality gates**

```powershell
python -m pytest tests/test_source_artifacts.py -q
ruff check scripts/source_artifacts.py tests/test_source_artifacts.py
```

Expected: PASS; the Git-unavailable test has completed a red-green cycle.

- [ ] **Step 6: Commit archive remediation**

```powershell
git add -- scripts/source_artifacts.py tests/test_source_artifacts.py
git commit -m "refactor: harden source archive validation"
```

---

### Task 4: Correct the direct Rust findings

**Files:**
- Modify: `src/distinct/mod.rs`
- Modify: `src/distinct/run.rs`
- Modify: `src/pii.rs`

**Interfaces:**
- Consumes: byte-segment capacity calculation, `RunCursor::open`, and phone candidate detection.
- Produces: identical values using standard-library idioms accepted by DeepSource.

- [ ] **Step 1: Run behavioral characterization tests**

```powershell
cargo test --locked --test distinct_spill
cargo test --locked --test leakage_contract
```

Expected: PASS before the mechanical refactor.

- [ ] **Step 2: Apply the three equivalent transformations**

Use:

```rust
.clamp(minimum_bytes, u32::MAX as usize)
```

Use `file.rewind()?` after retaining `use std::io::Seek;`, and use:

```rust
let digit_count = trimmed.matches(char::is_ascii_digit).count();
```

- [ ] **Step 3: Verify Rust formatting, Clippy, and targeted behavior**

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --test distinct_spill --test leakage_contract
```

Expected: PASS.

- [ ] **Step 4: Commit direct Rust fixes**

```powershell
git add -- src/distinct/mod.rs src/distinct/run.rs src/pii.rs
git commit -m "fix: adopt analyzer-safe Rust idioms"
```

---

### Task 5: Decompose exact-run compaction

**Files:**
- Modify: `src/distinct/run.rs`
- Test: `tests/distinct_spill.rs`
- Test: `tests/resource_limits.rs`

**Interfaces:**
- Consumes: `compact(runs, account, directory, cancellation)`.
- Produces: `CompactionSpec`, `compaction_spec`, `open_merge_cursors`, `merge_compacted_payload`, and `finish_compacted_run` private units.

- [ ] **Step 1: Add an edge-case characterization for compacted ordering**

Add the test at the nearest existing test seam so a compacted exact profile containing repeated and out-of-order values asserts exact distinct count and deterministic fingerprint. The already-failing DeepSource `RS-R1000` check is the structural red test; this characterization protects behavior during the refactor.

- [ ] **Step 2: Introduce the compaction boundaries**

Implement:

```rust
struct CompactionSpec {
    kind: RunKind,
    records: u64,
    payload_bytes: u64,
    max_record_bytes: u64,
    total_bytes: u64,
}

fn compaction_spec(runs: &[RunMeta], cancellation: &CancellationToken)
    -> Result<CompactionSpec, ProofFrameError>;
fn open_merge_cursors(runs: &[RunMeta])
    -> Result<(Vec<RunCursor>, BinaryHeap<Reverse<MergeEntry>>), ProofFrameError>;
fn merge_compacted_payload(
    writer: &mut BufWriter<File>, cursors: &mut [RunCursor],
    heap: &mut BinaryHeap<Reverse<MergeEntry>>, cancellation: &CancellationToken,
) -> Result<(u64, u64, [u8; 32]), ProofFrameError>;
```

Keep `compact` responsible for resource reservations, file creation, header writing, helper orchestration, count validation, checksum patching, and `RunMeta` construction.

- [ ] **Step 3: Run exact-state suites**

```powershell
cargo test --locked --test distinct_spill --test resource_limits
cargo test --locked --release --test allocation_contract -- --test-threads=1
```

Expected: PASS with the same resource and exact-count behavior.

- [ ] **Step 4: Commit compaction refactor**

```powershell
git add -- src/distinct/run.rs tests/distinct_spill.rs tests/resource_limits.rs
git commit -m "refactor: decompose exact run compaction"
```

---

### Task 6: Decompose Arrow encoder selection and scan dispatch

**Files:**
- Modify: `src/encoding/plan.rs`
- Modify: `src/execution/kernels.rs`
- Test: `tests/fingerprint_versions.rs`
- Test: `tests/execution_contract.rs`

**Interfaces:**
- Consumes: `ValueEncoder::for_data_type` and `scan_column`.
- Produces: encoder-family helpers and typed scan-family helpers without changing fingerprints, row numbers, or violation ordering.

- [ ] **Step 1: Add nested-encoder and mixed-rule regressions**

Add a fingerprint test containing list, struct, and map fields and assert the current V2 digest remains batch-invariant. Add an execution test mixing null, range, pattern/allowlist, and unique rules and assert the exact ordered `(rule, column, row)` tuples.

- [ ] **Step 2: Establish the behavior characterization**

Run the new fingerprint and execution tests before refactoring and record the stable digest/order outputs. The already-failing DeepSource `RS-R1000` checks are the structural red tests.

- [ ] **Step 3: Split encoder mapping by family**

Implement:

```rust
fn scalar_encoder(data_type: &DataType) -> Option<ValueEncoder>;
fn temporal_encoder(data_type: &DataType) -> Option<ValueEncoder>;
fn nested_encoder(data_type: &DataType) -> Result<Option<ValueEncoder>, ProofFrameError>;
```

`for_data_type` tries those helpers and returns `UnsupportedType` only if all return no encoder.

- [ ] **Step 4: Split scan dispatch by value family**

Extract `scan_signed`, `scan_unsigned`, `scan_float32`, `scan_float64`, `scan_timestamp`, `scan_decimal`, and `scan_boolean`. Each helper accepts the plan, typed array, row offset, validation state, and optional exact state, and owns its current row loop. `scan_column` becomes a `match`/dispatch layer plus the existing string/binary/fallback calls.

- [ ] **Step 5: Run fingerprint and execution suites**

```powershell
cargo test --locked --test fingerprint_versions --test execution_contract
cargo test --locked --test contract_compile
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS with stable fingerprints and finding order.

- [ ] **Step 6: Commit encoder/kernel refactor**

```powershell
git add -- src/encoding/plan.rs src/execution/kernels.rs tests/fingerprint_versions.rs tests/execution_contract.rs
git commit -m "refactor: split encoder and scan dispatch"
```

---

### Task 7: Decompose batch inspection and Python registration

**Files:**
- Modify: `src/lib.rs`
- Modify: `src/python.rs`
- Test: `tests/execution_contract.rs`
- Test: `tests/test_python_surface.py`

**Interfaces:**
- Consumes: legacy `inspect_batches` profile/validation path and `python::register`.
- Produces: `InspectionContext` plus setup/process/finalize helpers and grouped Python registration helpers.

- [ ] **Step 1: Add output-equivalence regressions**

Extend execution coverage so a segmented reader and one-batch reader produce the same profile fingerprint, column metrics, validation verdict, and findings. Extend Python surface coverage to assert every exported function, exception, and `__version__` remains present.

- [ ] **Step 2: Verify tests detect missing registrations**

Temporarily add one nonexistent required export to the expected-name set, run the Python surface test to observe failure, then restore it.

- [ ] **Step 3: Extract batch inspection state**

Create a private `InspectionContext` holding resource account, column states, unique sets, compiled regex patterns, validation state, row count, and hasher. Keep `inspect_batches` as the typed orchestration layer:

```rust
let mut context = InspectionContext::new(
    schema.clone(),
    contract,
    distinct_mode,
    resources,
    row_count_hint,
)?;
for maybe_batch in reader {
    context.inspect_batch(maybe_batch?)?;
}
context.finish()
```

Keep schema-change rejection at the batch boundary and preserve row numbering.

- [ ] **Step 4: Group Python registration**

Implement `register_data_functions`, `register_receipt_functions`, and `register_exceptions`. Keep `register` responsible only for invoking those helpers and adding `__version__`.

- [ ] **Step 5: Run Rust and Python surface suites**

```powershell
cargo test --locked --test execution_contract --test fingerprint_versions
maturin develop --release --locked
python -m pytest tests/test_python_surface.py tests/test_api.py -q
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Expected: PASS with the same exported module surface.

- [ ] **Step 6: Commit inspection/registration refactor**

```powershell
git add -- src/lib.rs src/python.rs tests/execution_contract.rs tests/test_python_surface.py
git commit -m "refactor: split inspection and Python registration"
```

---

### Task 8: Prepare the 0.5.1 release identity and documentation

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `pyproject.toml`
- Modify: `README.md`
- Modify: `README-crates.md`
- Modify: `CHANGELOG.md`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/publish.yml`
- Modify: `tests/test_source_artifacts.py`
- Modify: `tests/test_release_metadata.py`
- Modify: `tests/test_readme_examples.py`

**Interfaces:**
- Consumes: 0.5.0 package metadata and tag-bound release workflow.
- Produces: synchronized 0.5.1 metadata, runnable secret-safe examples, and workflows that verify 0.5.1 artifacts.

- [ ] **Step 1: Update release-metadata tests first**

Change expected versions/roots to `0.5.1`, add assertions that CI and publish workflows contain `proofframe-0.5.1`, and add a README test that executes the receipt example after assigning:

```python
_, signing_key, verification_key = pf.generate_keypair().values()
```

Pass `private_key=signing_key` and `expected_public_key=verification_key`; never place credential-looking mapping-key literals in the README source.

- [ ] **Step 2: Run tests and observe the version mismatch failures**

```powershell
python -m pytest tests/test_release_metadata.py tests/test_source_artifacts.py tests/test_readme_examples.py -q
```

Expected: FAIL while manifests, workflows, and README still say 0.5.0.

- [ ] **Step 3: Synchronize all 0.5.1 version surfaces**

Set `Cargo.toml` and `pyproject.toml` to 0.5.1, run `cargo check` to refresh the root package in `Cargo.lock`, update CI import assertions and artifact paths, and update publish workflow sdist verification.

- [ ] **Step 4: Rewrite release-facing documentation**

Update install commands to 0.5.1. Replace the beta paragraph with a maintenance-release statement. Add a top changelog section containing only the analyzer refactors, source archive hardening, secret-safe example, and unchanged API compatibility. Keep benchmark methodology unchanged.

- [ ] **Step 5: Verify metadata and README examples**

```powershell
python scripts/verify_release_version.py --tag v0.5.1
python -m pytest tests/test_release_metadata.py tests/test_source_artifacts.py tests/test_readme_examples.py -q
ruff check python tests benchmarks scripts
```

Expected: PASS.

- [ ] **Step 6: Commit release identity**

```powershell
git add -- Cargo.toml Cargo.lock pyproject.toml README.md README-crates.md CHANGELOG.md .github/workflows/ci.yml .github/workflows/publish.yml tests/test_source_artifacts.py tests/test_release_metadata.py tests/test_readme_examples.py
git commit -m "release: prepare proofframe 0.5.1"
```

---

### Task 9: Run full local release verification

**Files:**
- Generated only: `target/**`, `dist/**`

**Interfaces:**
- Consumes: the complete 0.5.1 branch.
- Produces: fresh local evidence that behavior, formatting, package contents, and metadata are releasable.

- [ ] **Step 1: Run all Rust gates**

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --release --test allocation_contract -- --test-threads=1
cargo test --locked --doc --all-features
cargo publish --dry-run --locked
```

- [ ] **Step 2: Build and test Python artifacts**

```powershell
maturin build --release --locked --out dist --compatibility pypi
maturin sdist --out dist
python -m pip install --no-deps --force-reinstall (Get-ChildItem dist\*.whl | Select-Object -First 1).FullName
python -m pytest -q --basetemp target/pytest-0.5.1 --cov=proofframe --cov-report=term-missing --cov-fail-under=85
python -m twine check dist\*
python scripts/source_artifacts.py --output target/source-artifacts --verify dist/proofframe-0.5.1.tar.gz
```

- [ ] **Step 3: Inspect the final tree**

```powershell
git diff --check origin/main...HEAD
git status --short
git log --oneline origin/main..HEAD
```

Expected: only intentional commits; generated artifacts remain untracked/ignored.

---

### Task 10: Push, clear DeepSource, and release 0.5.1

**Files:**
- No additional source changes unless CI or DeepSource identifies a reproducible root cause.

**Interfaces:**
- Consumes: verified `fix/0.5.1-deepsource` head.
- Produces: green GitHub/DeepSource checks, merged release commit, tag `v0.5.1`, PyPI 0.5.1, and crates.io 0.5.1.

- [ ] **Step 1: Push the feature branch**

```powershell
git push -u origin fix/0.5.1-deepsource
```

- [ ] **Step 2: Verify authoritative checks**

Use `gh run list`, `gh run view`, and the DeepSource run page to require all GitHub Actions jobs and Python/Rust/Secrets analyzers green. Do not merge while any check is pending or failing.

- [ ] **Step 3: Merge the verified branch**

Create one non-draft PR to `main`, merge only after checks pass, then verify `origin/main` contains the exact reviewed tree.

- [ ] **Step 4: Confirm registry availability before the irreversible publish**

Check `https://pypi.org/pypi/proofframe/0.5.1/json` and `https://crates.io/api/v1/crates/proofframe/0.5.1`. Both must report that 0.5.1 is absent. Crates.io publication is permanent; use the existing Cargo credential provider without displaying its token. If no Cargo credential exists, stop and request that the maintainer run `cargo login` locally.

- [ ] **Step 5: Tag the exact main commit**

```powershell
git switch main
git pull --ff-only origin main
python scripts/verify_release_version.py --tag v0.5.1
git tag -a v0.5.1 -m "ProofFrame 0.5.1"
git push origin v0.5.1
```

The tag push triggers exact-tag CI and the trusted PyPI publishing workflow.

- [ ] **Step 6: Publish crates.io only after tag CI succeeds**

```powershell
cargo publish --locked
```

Do not retry blindly if Cargo reports a timeout; query crates.io first because the upload may already have succeeded.

- [ ] **Step 7: Verify both registries and GitHub release state**

Confirm PyPI and crates.io both serve exactly 0.5.1, the GitHub Actions publish workflow succeeded, and `pip install proofframe==0.5.1` plus `cargo info proofframe@0.5.1` resolve the new release.
