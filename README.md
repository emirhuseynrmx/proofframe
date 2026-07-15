<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame" width="100%" />
</div>

# ProofFrame

[![CI](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml/badge.svg)](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/proofframe.svg)](https://crates.io/crates/proofframe)
[![docs.rs](https://img.shields.io/docsrs/proofframe)](https://docs.rs/proofframe)
[![codecov](https://codecov.io/gh/emirhuseynrmx/proofframe/graph/badge.svg)](https://codecov.io/gh/emirhuseynrmx/proofframe)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange)]()
[![Sponsor](https://img.shields.io/badge/Sponsor-%E2%9D%A4-db61a2)](https://github.com/sponsors/emirhuseynrmx)

**Ruff for data. Git-style evidence for DataFrames.**

## Why ProofFrame

Most data checks answer one narrow question: did this table pass? ProofFrame is built for the
harder production question: **what exactly was checked, why did it fail, and can we prove that later?**

- **Proof, not vibes:** every profile includes a versioned canonical BLAKE3 fingerprint
  (`pf-fp-v1`) so the checked dataset can be identified again.
- **Bounded evidence:** validation returns row-level findings, total violation counts, and
  truncation status instead of hiding failures behind a boolean.
- **One Arrow-native engine:** Rust core, Python bindings, PyArrow/Pandas/Polars input, no Python
  row materialization in the hot path.
- **CI-friendly contracts:** deterministic JSON, explicit CLI exit codes, signed proof receipts,
  PII scanning, leakage checks, and keyed diffs.
- **Fast Python package, real Rust core:** installable from PyPI with ABI-stable wheels, while the
  same engine is available to Rust users through crates.io.

ProofFrame is a Rust crate with Python bindings for answering three questions before bad data
ships:

1. **What is this dataset?** Profile it and produce a deterministic BLAKE3 fingerprint.
2. **Does it satisfy its contract?** Return bounded, row-level evidence—not a vague pass/fail.
3. **What changed?** Diff two datasets by business key and name the changed columns.

It accepts PyArrow tables and streams directly through the Arrow C Stream interface. Pandas and
Polars DataFrames use the same Arrow-native path. The validation engine processes record batches in
one pass without converting rows into Python objects.

> Alpha software: `0.4.0a3` adds nested list/struct/map fingerprinting and a typed `ProofFrameError`
> API on top of canonical proof fingerprints, disk-backed exact diffs, privacy-preserving PII
> findings, train/test leakage checks, and signed proof receipts. The receipt schema and detector
> taxonomy may still change before 0.4 stable.

## The 30-second demo

```bash
pip install proofframe==0.4.0a3
```

```python
import pyarrow as pa
import proofframe as pf

users = pa.table({
    "id": [1, 1, 3],
    "email": ["a@example.com", None, "not-an-email"],
    "score": [0.91, 1.40, 0.73],
})

report = pf.validate(users, {
    "columns": {
        "id": {"required": True, "unique": True},
        "email": {"not_null": True, "pattern": r"^[^@]+@[^@]+$"},
        "score": {"min": 0, "max": 1},
    }
})

assert not report["valid"]
for finding in report["findings"]:
    print(finding)
```

ProofFrame reports the duplicate ID, null email, malformed email, and out-of-range score with their
row numbers. The same pass returns `valid`, `violation_count`, `truncated`, a content fingerprint,
and a per-column profile.

For a rules-only pipeline gate that keeps bounded row evidence but skips profile and fingerprint
work, use `pf.validate(users, contract, include_profile=False)`.

## Dataset fingerprints

```python
snapshot = pf.profile(users)

print(snapshot["fingerprint"])
print(snapshot["rows"])
print(snapshot["columns"])
```

The fingerprint is emitted as `pf-fp-v1:<blake3>`. It is deterministic for the ordered Arrow data
and distinguishes nulls, column positions, schema fields, type tags, and value boundaries. The hash
input uses ProofFrame's canonical byte encoding rather than Arrow's display formatting, so upgrading
Arrow cannot silently change fingerprints through prettier string rendering. Store the fingerprint
in CI metadata to prove exactly which data was checked.

## Row-level diff

```python
before = pa.table({"id": [1, 2, 3], "plan": ["free", "pro", "pro"]})
after = pa.table({"id": [1, 2, 4], "plan": ["free", "team", "pro"]})

change = pf.diff(before, after, keys="id")

assert change["added_keys"] == ["4"]
assert change["removed_keys"] == ["3"]
assert change["changed"] == [{"key": "2", "columns": ["plan"]}]
```

Composite keys work too: `keys=["tenant_id", "user_id"]`. Duplicate keys fail loudly instead of
silently producing a misleading diff. The diff engine uses disk-backed hash partitions, so it keeps
exact changed-column output without materializing both full datasets in memory.

## PII and leakage checks

```python
pii = pf.scan_pii(users)
overlap = pf.detect_leakage(train, test, keys="user_id")
```

PII findings include the class, column, row, confidence, and a short domain-separated BLAKE3
fingerprint. They never include the matched value. Leakage reports support key overlap or exact
full-row overlap and expose only hashed sample identifiers.

The built-in detector recognizes email, IPv4, phone, payment-card (Luhn), and IBAN patterns. It is a
high-signal scanner, not a legal-compliance guarantee; structured identifiers and locale-specific
formats should be covered by explicit contracts too. Bare digit matches from numeric columns are
downgraded to low confidence so Luhn-valid order IDs are not reported as high-confidence payment
cards without context.

## Signed proof receipts

```python
keys = pf.generate_keypair()
report = pf.validate(users, contract)
receipt = pf.sign_receipt(report, private_key=keys["private_key"])
assert pf.verify_receipt(receipt)["valid"]
```

Receipts use Ed25519 signatures, RFC 8785 JSON canonicalization, and a BLAKE3 report hash. Private
keys are generated locally and are never embedded in receipts. Store them in a secret manager, not
in source control. To avoid ambiguous JSON number canonicalization, integers outside the I-JSON safe
range are rejected.

## CLI

ProofFrame reads CSV and Parquet files:

```bash
proofframe profile users.parquet
proofframe validate users.parquet --contract examples/contract.json
proofframe diff yesterday.parquet today.parquet --key tenant_id --key user_id
```

Every command prints stable JSON and uses exit-safe parsing, making it suitable for CI, agents, and
data pipeline gates.

`proofframe validate` exits with `0` for valid data, `1` for contract violations, `2` for input or
configuration errors, and `3` for unexpected internal failures.

## Contract format

```json
{
  "columns": {
    "user_id": { "required": true, "unique": true, "not_null": true },
    "email": { "not_null": true, "pattern": "^[^@]+@[^@]+$" },
    "score": { "min": 0.0, "max": 1.0 },
    "status": { "allowed": ["active", "paused", "deleted"] }
  },
  "max_findings": 100
}
```

`max_findings` bounds the number of finding examples, not correctness. `violation_count` still
counts every violation, `truncated` tells you whether examples were omitted, and the profile and
fingerprint still cover the full stream.

## Why Rust + Arrow

Python data-quality libraries often materialize Python rows or bind themselves to one DataFrame
implementation. ProofFrame accepts the Arrow C Stream protocol, so PyArrow, Pandas, Polars, and any
compatible producer can feed the same native engine.

```text
Pandas / Polars / PyArrow / Arrow C Stream
                    |
                    v
           Arrow record batches
                    |
        +-----------+-----------+
        v           v           v
     profile     contracts     keyed diff
        |           |           |
        +-----------+-----------+
                    v
       deterministic JSON evidence
```

The Rust core currently provides:

- single-pass profiling and validation;
- versioned canonical BLAKE3 dataset fingerprints;
- null, distinct, numeric min/max profiles;
- required, not-null, unique, numeric range, regex, and allowlist rules;
- disk-backed key diff with exact added/removed/changed-column evidence;
- bounded evidence reports;
- a crates.io package with a default Rust API and no required Python linkage;
- Python 3.10+ ABI-stable wheels through PyO3/maturin.
- `#![forbid(unsafe_code)]` in the ProofFrame crate.

Rust users get a separate crates.io-focused README via `README-crates.md`.

## Development

```bash
python -m venv .venv
.venv/Scripts/pip install -e ".[dev]"  # Windows
.venv/Scripts/maturin develop
.venv/Scripts/pytest -q
cargo test
```

Run the local throughput harness:

```bash
python benchmarks/profile.py --rows 1000000
```

The Python API and CLI coverage gate is 85%; the current local branch-and-line result is 98.23%.
Rust correctness is gated separately by unit tests, Proptest, and Clippy on the declared Rust 1.85
MSRV so Python coverage cannot hide a native-core failure.

Run the same-rule local benchmark harness:

```bash
pip install -e ".[benchmark]"
python benchmarks/compare_frameworks.py --rows 1000000 --repeats 5
```

The script runs the same non-null, uniqueness, and range predicates across a small set of validation
engines; excludes setup/import time; and records raw samples plus exact package versions. It uses
ProofFrame's `include_profile=False` rules-only path because benchmark peers are not asked to
compute a BLAKE3 dataset fingerprint or exact per-column profile. See `docs/testing.md`.

### Local 0.4 alpha benchmark snapshot

On the development machine (Windows 11, Python 3.12), 1,000,000 valid rows, one warmup, and seven
measured repetitions produced a ProofFrame median of **0.0162 s** (**61.65M rows/s**) for the
rules-only path. Raw peer samples and exact versions are committed in
`benchmarks/results/windows-1m-fast.json`; this is a reproducible machine-local result, not a
universal performance guarantee or a marketing claim.

The benchmark prints measured rows/second for the current machine; this README intentionally makes
no unverified performance claim.

### Local v0.1 baseline

On the development machine (Windows x86-64, Python 3.12, release wheel), profiling and fingerprinting
a generated Arrow table with **1,000,000 rows × 3 columns** took **2.257 seconds** (**443,060
rows/second**). The profile used one integer, one float, and one string column with exact distinct
counts. This is a reproducible baseline, not a cross-library comparison; run the harness on your own
hardware before drawing performance conclusions.

## Roadmap

- **0.4 stable:** validation-only fast path, extracted Miri-compatible core, fuzz targets, pinned
  cross-platform benchmarks, and a stabilized receipt schema.
- **0.5:** reversible dataset patches, Parquet predicate pushdown, and configurable diff partition
  tuning.
- **1.0:** stable contract schema and cross-language Rust/Python compatibility.

## License

Apache-2.0
