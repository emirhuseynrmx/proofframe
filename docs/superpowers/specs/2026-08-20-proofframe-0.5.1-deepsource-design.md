# ProofFrame 0.5.1 DeepSource Remediation Design

## Objective

Release ProofFrame 0.5.1 from the current `main` history with the reported DeepSource Python, Rust, and Secrets checks green. The release must feel deliberate: corrected implementation quality, accurate public documentation, an explicit changelog entry, and registry-ready artifacts. Calybris and new ProofFrame features are outside this scope.

## Branch and release boundaries

- Base all work on `main`, which contains the published `v0.5.0` history.
- Work on `fix/0.5.1-deepsource`; do not release from the divergent `a` branch.
- Port no code from `a` wholesale. Reuse wording or configuration only when it matches the current 0.5 code and this specification.
- Set the Rust crate, Python package, and lockfile version to `0.5.1` together.
- Publish only after CI and DeepSource pass on the exact release commit. Use the repository's trusted-publishing workflow rather than local registry tokens.

## Remediation design

### Python

- Remove the unnecessary local `del include_profile` without changing the compatibility signature of `validate`.
- Remove `json.JSONDecodeError` from the overlapping exception tuple because `ValueError` already covers it.
- Split source-archive validation into focused helpers for member-name validation, root validation, and content-policy validation. Preserve every existing rejection condition and exception type.
- Resolve the Git executable to an absolute path before invoking it. Fail with a clear `ArchiveHygieneError` if Git is unavailable.
- Mark protocol test fixture methods as static where instance state is intentionally unused. Preserve their externally observed Arrow protocol behavior.

### Rust

- Replace the bounded capacity chain with `clamp`, the initial seek with `rewind`, and the digit filter/count with the equivalent `matches` operation.
- Reduce complexity in distinct-run compaction by separating input validation, cursor setup, merge iteration, and finalized-run construction.
- Reduce encoder-plan complexity by separating primitive, temporal, and nested Arrow type mapping while preserving unsupported-type errors.
- Reduce scan-kernel complexity by extracting rule-family evaluation helpers without changing row offsets, exact-state updates, or violation ordering.
- Reduce batch-inspection complexity by separating setup, per-batch processing, and finalization while preserving resource accounting and cancellation behavior.
- Reduce Python-module registration complexity by registering cohesive function groups through helpers while preserving the public module surface.

No `skipcq`, analyzer exclusion, blanket allow, or warning suppression may be used to make these findings disappear.

## Secrets finding

Rewrite the README receipt-signing example so generated key material is assigned to neutral local variables before use. Keep the example runnable, but avoid source text that resembles a hardcoded credential lookup. Do not add fake values, tokens, or scanner exclusions.

## 0.5.1 release presentation

- Update README installation commands and release language to 0.5.1.
- Replace the stale beta framing with a concise maintenance-release statement focused on analyzer cleanliness, release hygiene, and unchanged API compatibility.
- Add a 0.5.1 changelog entry grouping quality, security hygiene, and documentation changes.
- Keep claims evidence-based; do not introduce new benchmark or performance claims.
- Keep the crate-specific README consistent wherever version or release status is stated.

## Testing and evidence

- Add regression tests before behavior-affecting refactors. Tests must cover archive path/root/content rejection, resolved Git execution, Arrow fixture protocol calls, compacted distinct output, encoder mapping, scan violations, batch inspection, and Python registration surface as applicable.
- Run targeted tests during each red-green cycle.
- Before release, run the complete Rust and Python test suites, formatting checks, Clippy with warnings denied, Ruff checks, source-archive verification, `cargo publish --dry-run --locked`, Python wheel/sdist builds, and Twine validation.
- Push the feature branch and use the DeepSource run on the exact commit as the authoritative analyzer check. If DeepSource reports a remaining issue, fix its root cause and rerun the relevant local regression suite before pushing again.
- Create the 0.5.1 release tag only after all repository CI and DeepSource checks are green. Confirm PyPI and crates.io do not already contain 0.5.1 before triggering trusted publishing.

## Success criteria

- DeepSource Python, Rust, and Secrets report no blocking issues or failing metrics on the release commit.
- All existing public APIs remain compatible with 0.5.0.
- Rust and Python package metadata both report 0.5.1.
- README and changelog consistently describe 0.5.1 without unsupported claims.
- CI produces valid crates.io and PyPI artifacts, and trusted publishing completes for both registries.
