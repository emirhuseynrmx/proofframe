<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame" width="100%" />
</div>

# ProofFrame

[![PyPI](https://img.shields.io/pypi/v/proofframe.svg)](https://pypi.org/project/proofframe/)
[![Crates.io](https://img.shields.io/crates/v/proofframe.svg)](https://crates.io/crates/proofframe)
[![docs.rs](https://img.shields.io/docsrs/proofframe)](https://docs.rs/proofframe)
[![CI](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml/badge.svg)](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/emirhuseynrmx/proofframe/graph/badge.svg)](https://codecov.io/gh/emirhuseynrmx/proofframe)
[![DeepSource](https://app.deepsource.com/gh/emirhuseynrmx/proofframe.svg/?label=active+issues&show_trend=true)](https://app.deepsource.com/gh/emirhuseynrmx/proofframe/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange)](https://www.rust-lang.org/)

**Relational Arrow contracts, exact dataset rules, and verifiable evidence.**

ProofFrame is a Rust-native data quality engine for PyArrow, Pandas, Polars, CSV, Parquet, and Arrow
streams. It compiles strict contracts against the physical schema, scans record batches without
turning rows into Python objects, and produces evidence that can be stored, compared, and signed.

## Why ProofFrame

Most data checks answer one question: did the table pass? Production systems usually need three:

1. **What exactly was checked?** Versioned BLAKE3 fingerprints identify the ordered dataset.
2. **Why did it fail?** Exact violation counts and bounded row-level findings explain the verdict.
3. **What changed?** Keyed diffs report added, removed, and changed records without loading both
   datasets into memory.

ProofFrame keeps these answers deterministic and resource-bounded. Exact uniqueness, diff, and
leakage operations have explicit memory, temporary-storage, sample, and output limits. Corrupt
temporary data, incompatible schemas, ambiguous contracts, and exceeded limits fail closed.

> **Current release — 0.6.0**
>
> 0.6.0 adds review-required contract suggestions, complete contract and API guides, runnable
> workflows, and practical integration examples. Existing V1/V2 contract and fingerprint protocols
> remain compatible.

## Install

Python 3.10–3.13:

```bash
pip install proofframe==0.6.0
```

Rust 1.85 or newer:

```bash
cargo add proofframe@0.6.0
```

## The 30-second demo

```python
import pyarrow as pa
import proofframe as pf

orders = pa.table({
    "order_id": [101, 102, 103],
    "subtotal": [12.50, 8.00, 10.00],
    "total": [12.50, 7.50, 10.00],
})

contract = {
    "version": "proofframe.contract.v2",
    "columns": {},
    "row_rules": [{
        "name": "total_covers_subtotal",
        "compare": {
            "left": {"column": "total"},
            "op": "gte",
            "right": {"column": "subtotal"},
        },
    }],
    "dataset_rules": {
        "row_count": {"min": 1},
        "distinct_ratio": {"order_id": {"min": 1.0}},
    },
}

report = pf.check(
    orders,
    contract,
    max_memory=64 << 20,
    max_temp=512 << 20,
    max_samples=20,
)

assert report["valid"] is False
assert report["violation_count"] == 1
```

The contract is compiled before scanning. Unknown fields, missing required columns, invalid bounds,
and rules that do not match the Arrow type are rejected before the first row is processed.
`violation_count` remains exact even when the retained `findings` sample is truncated.

## Start from a reviewable draft

Use the native suggestion scanner to create a V2 draft, then inspect it before activation:

```bash
proofframe suggest data.parquet > contract.json
# Review contract.json and set "status" to "active" before checking it.
proofframe check data.parquet --contract contract.json
```

`suggest` performs its own Arrow scan so it can preserve exact integer bounds. It infers
types and non-null columns by default; uniqueness, required columns, and category allowlists
are explicit opt-ins. Timestamp and monotonically increasing numeric ranges are deliberately
omitted and recorded in `suggested_from.review`. A `draft` is rejected with
`PF_DRAFT_CONTRACT` until a reviewer changes its status to `active`. See the
[five-minute guide](docs/getting-started.md) and [contract reference](docs/contracts.md).

## Cross-column and conditional rules

V2 compares Arrow values in their physical type. It does not cast through Python objects or parse
an expression language at runtime.

```python
shipments = pa.table({
    "ordered_at": [1, 3],
    "delivered_at": [2, 2],
    "status": ["delivered", "pending"],
    "tracking_id": ["TR-1", None],
})

contract = {
    "version": "proofframe.contract.v2",
    "columns": {},
    "row_rules": [
        {
            "name": "delivery_window",
            "compare": {
                "left": {"column": "ordered_at"},
                "op": "lte",
                "right": {"column": "delivered_at"},
            },
        },
        {
            "name": "delivered_has_tracking",
            "when": {
                "left": {"column": "status"},
                "op": "eq",
                "right": {"literal": "delivered"},
            },
            "assert": {"column": "tracking_id", "not_null": True},
        },
    ],
}

report = pf.check(shipments, contract)
```

Comparisons support signed and unsigned integers, floats, booleans, UTF-8, dates, timestamps, and
decimal128 where the Arrow types are compatible. Null behavior is explicit. Conditional assertions
cover nullability, numeric bounds, allowlists, patterns, and NaN policy without building a row mask.

## Dataset-level rules and partitions

Dataset rules keep exact state across record-batch and partition boundaries. Distinct and composite
keys use canonical values, not hash-only identity. When the memory budget is reached, sorted,
checksummed runs spill under the configured temporary-storage limit.

```python
partitions = [
    pa.table({"order_id": [101, 101], "line_id": [1, 2]}),
    pa.table({"order_id": [102], "line_id": [1]}),
]

contract = {
    "version": "proofframe.contract.v2",
    "columns": {},
    "dataset_rules": {
        "row_count": {"min": 3},
        "distinct_ratio": {"order_id": {"min": 0.5}},
        "composite_unique": [{
            "name": "line_key",
            "columns": ["order_id", "line_id"],
        }],
    },
}

report = pf.check_partitions(partitions, contract, threads=2)
assert report["valid"] is True
```

`pf.check_partitions_with_evidence` additionally returns an ordered manifest binding every
partition's V2 fingerprint, row count, schema, contract, compiled plan, result contribution, global
result, and resource settings. Reordering, omission, duplication, or mixed identities fails
verification.

## One engine, several proof operations

### Fingerprint a dataset

```python
legacy = pf.fingerprint(orders, version="v1")
current = pf.fingerprint(orders, version="v2")
```

Fingerprints bind schema, row and column order, nulls, type tags, and canonical values. They do not
depend on Arrow display formatting and remain stable across record-batch boundaries. V1 is frozen
for existing proofs; V2 is a separate protocol for new evidence.

### Diff by business key

```python
changes = pf.diff(
    before,
    after,
    keys="order_id",
    max_memory=256 << 20,
    max_temp=2 << 30,
    max_samples=100,
    output="changes.jsonl",
    spill="auto",
)
```

Counts are exact. Samples stay bounded, while full change records can be written atomically as JSON
Lines or Arrow IPC. Duplicate keys and schema mismatches fail loudly.

### Create verifiable evidence

```python
checked = pf.check_with_evidence(orders, contract, max_samples=20)
report = checked["report"]
evidence = checked["evidence"]

assert report["valid"] is False
assert evidence["schema"] == "proofframe.evidence.v2"
```

Evidence V2 binds the dataset fingerprint, canonical contract source, compiled plan, Arrow schema,
engine version, resource limits, and result. Signed receipts use Ed25519 and separate cryptographic
validity from signer trust. Keep signing material in a secret manager and verify against a public
key obtained independently from the receipt.

### Find PII and train/test leakage

```python
pii = pf.scan_pii(customers)
overlap = pf.detect_leakage(train, test, keys="user_id")
```

PII findings contain the class, column, row, confidence, and a keyed fingerprint—not the matched
value. Leakage reports support business keys or full-row identity and expose only bounded hashed
samples.

## Arrow-native by design

```text
Pandas / Polars / PyArrow / Arrow C Stream / CSV / Parquet
                           |
                           v
                  Arrow record batches
                           |
          +----------------+----------------+
          |                |                |
       contracts       fingerprints      keyed diff
          |                |                |
          +----------------+----------------+
                           |
                           v
              deterministic JSON evidence
```

Known DataFrame containers provide exact row and logical-byte hints. Stream-only inputs remain
streaming. The Rust scan releases the Python GIL, and the ProofFrame crate itself uses
`#![forbid(unsafe_code)]`.

## Where ProofFrame fits

ProofFrame is strongest when you need Arrow-native, exact checks with bounded resources and
evidence that records both the contract source and executable plan. It is deliberately smaller
than established data-quality platforms.

| Tool | Prefer it when | ProofFrame trade-off |
| --- | --- | --- |
| [Pandera](https://pandera.readthedocs.io/) | You want Python-first dataframe schemas, typing, and familiar pandas workflows. | ProofFrame prioritizes Arrow streams, exact global rules, and evidence over dataframe typing ergonomics. |
| [Great Expectations](https://docs.greatexpectations.io/) | You need a large expectation library, data docs, and broad orchestration connectors. | ProofFrame has a narrower rule surface and fewer integrations, but keeps validation and proof artifacts compact. |
| [Soda](https://docs.soda.io/) | You want monitors, alerting, and a mature data-observability workflow. | ProofFrame is a library/CLI for deterministic checks; it does not replace an observability platform. |
| [Deequ](https://github.com/awslabs/deequ) | Your data platform is Spark/Scala and you value its constraint-suggestion ecosystem. | ProofFrame avoids a Spark dependency and works directly with Arrow, but does not provide Deequ's Spark ecosystem. |

These tools can coexist: use ProofFrame at an Arrow boundary when repeatable checks, resource
limits, and independently verifiable evidence matter.

## CLI

```bash
proofframe check data.parquet --contract contract.json --max-memory 256MiB --max-temp 2GiB
proofframe fingerprint data.csv --fingerprint-version v2
proofframe diff old.parquet new.parquet --key order_id --output changes.jsonl
proofframe evidence data.parquet --contract contract.json --output evidence.json
proofframe verify receipt.json --expected-public-key "$PROOFFRAME_PUBLIC_KEY"
```

| Exit code | Meaning |
| ---: | --- |
| 0 | Operation succeeded; check or receipt is valid |
| 1 | Contract violation or invalid receipt |
| 2 | Invalid input, contract, or command configuration |
| 3 | Engine, I/O, schema, or corrupt-data failure |
| 4 | Resource limit exceeded |

JSON is emitted only after a successful operation. File outputs use same-directory temporary files,
`fsync`, and atomic replacement.

## Rust core

The default crate has no Python dependency. Rust users get the same compiled contracts, typed Arrow
kernels, fingerprints, evidence, receipt verification, resource accounting, and spill engine used
by the Python wheels. See the [crate guide](README-crates.md) and [API documentation](https://docs.rs/proofframe).

## Compatibility and performance evidence

Version 0.6.0 preserves V1 fingerprints and the established compatibility entry points. New work
should use `check`, explicit fingerprint versions, Evidence V2, and Receipt V2.

Performance claims are tied to raw samples, dataset hashes, compiler and package versions, and
machine metadata. The committed smoke harness is deterministic; the pinned 7,645,034-row Bitcoin
comparison remains a dedicated-runner gate rather than a published benchmark claim. See
[testing and benchmark methodology](docs/testing.md).

## Release integrity

The release workflow packages the exact wheel, sdist, and crate subjects before publication. It
emits deterministic SHA-256 checksums, SPDX JSON SBOMs, GitHub build-provenance attestations, and
SBOM attestations. PyPI uses trusted publishing; crates.io publication is gated by the same tagged
commit and CI evidence. These controls support provenance verification, but they are not a claim of
formal SLSA certification.

## Development

```bash
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
maturin develop --release --locked
python -m pytest -q
```

CI covers Rust 1.85, Python 3.10–3.13 on Linux, macOS, and Windows, portable wheels, release-mode
allocation contracts, Miri-compatible state machines, fuzz targets, source-package hygiene,
coverage, DeepSource, and SonarCloud.

## License and security

ProofFrame is licensed under [Apache-2.0](LICENSE). Report vulnerabilities through the process in
[SECURITY.md](SECURITY.md). Sponsorship is available through [GitHub Sponsors](https://github.com/sponsors/emirhuseynrmx).
