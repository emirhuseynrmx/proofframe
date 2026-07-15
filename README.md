# ProofFrame

**Ruff for data. Git-style evidence for DataFrames.**

ProofFrame is a Rust-native Python library for answering three questions before bad data ships:

1. **What is this dataset?** Profile it and produce a deterministic BLAKE3 fingerprint.
2. **Does it satisfy its contract?** Return bounded, row-level evidence—not a vague pass/fail.
3. **What changed?** Diff two datasets by business key and name the changed columns.

It accepts PyArrow tables and streams directly through the Arrow C Stream interface. Pandas and
Polars DataFrames use the same Arrow-native path. The validation engine processes record batches in
one pass without converting rows into Python objects.

> Alpha software: `0.4.0a1` adds privacy-preserving PII findings, train/test leakage checks, and
> signed proof receipts. The receipt schema and detector taxonomy may still change before 0.4 stable.

## The 30-second demo

```bash
pip install proofframe
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
row numbers. The same pass returns a content fingerprint and per-column profile.

## Dataset fingerprints

```python
snapshot = pf.profile(users)

print(snapshot["fingerprint"])
print(snapshot["rows"])
print(snapshot["columns"])
```

The fingerprint is deterministic for the ordered Arrow data and distinguishes nulls, column
positions, and value boundaries. Store it in CI metadata to prove exactly which data was checked.

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
silently producing a misleading diff.

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
formats should be covered by explicit contracts too.

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

`max_findings` bounds report size while the profile and fingerprint still cover the full stream.

## Why Rust + Arrow

Python data-quality libraries often materialize Python rows or bind themselves to one DataFrame
implementation. ProofFrame accepts the Arrow C Stream protocol, so PyArrow, Pandas, Polars, and any
compatible producer can feed the same native engine.

```text
Pandas / Polars / PyArrow / Arrow C Stream
                    │
                    ▼
           Arrow record batches
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
     profile     contracts     keyed diff
        │           │           │
        └───────────┼───────────┘
                    ▼
       deterministic JSON evidence
```

The Rust core currently provides:

- single-pass profiling and validation;
- BLAKE3 dataset fingerprints;
- null, distinct, numeric min/max profiles;
- required, not-null, unique, numeric range, regex, and allowlist rules;
- key-based added/removed/changed row detection;
- bounded evidence reports;
- Python 3.10+ ABI-stable wheels through PyO3/maturin.

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

Run the same-rule comparative benchmark:

```bash
pip install -e ".[benchmark]"
python benchmarks/compare_frameworks.py --rows 1000000 --repeats 5
```

The script runs the same non-null, uniqueness, and range predicates in ProofFrame, Pandera, and
Great Expectations; excludes setup/import time; and records raw samples plus exact package versions.
ProofFrame currently also computes its full profile and fingerprint during validation, so the result
measures the public APIs as shipped rather than isolated predicate kernels. See `docs/testing.md`.

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
- **0.5:** reversible dataset patches and partition-aware Parquet diff.
- **1.0:** stable contract schema and cross-language Rust/Python compatibility.

## License

Apache-2.0
