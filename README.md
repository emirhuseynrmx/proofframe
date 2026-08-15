<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame" width="100%" />
</div>

# ProofFrame

[![PyPI](https://img.shields.io/pypi/v/proofframe.svg)](https://pypi.org/project/proofframe/)
[![Crates.io](https://img.shields.io/crates/v/proofframe.svg)](https://crates.io/crates/proofframe)
[![CI](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml/badge.svg)](https://github.com/emirhuseynrmx/proofframe/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

ProofFrame compiles strict data contracts into typed Arrow kernels. It scans record-batch streams,
keeps evidence bounded, and fails before execution when a rule does not match the physical schema.

Version 0.5.0 is a beta release. Its release gates cover synthetic scaling and allocation contracts;
the pinned 7,645,034-row Bitcoin comparison remains a dedicated-runner gate, not a published claim.

## What 0.5 changes

- Contracts are a versioned, deny-unknown-fields syntax tree compiled against the Arrow schema.
- Numeric, timestamp, decimal, string, binary, and uniqueness rules execute through specialized
  kernels rather than per-row dynamic dispatch.
- Exact uniqueness, keyed diff, and leakage checks have explicit memory, temporary-storage, output,
  and sample limits. Exact state spills as checksummed runs when its memory account is exhausted.
- `pf-fp-v1` stays frozen for existing proofs. `pf-fp-v2` adds a segmented encoder designed for
  prepared buffers and stable batch-independent hashing.
- Python receives native dictionaries and typed exceptions; the Rust scan runs with the GIL
  released and consumes the Arrow C Stream when available.
- Evidence V2 binds dataset, contract, engine, limits, and result. Receipt verification reports
  cryptographic integrity separately from signer trust.

The Rust crate forbids unsafe code. PyO3 and Arrow's FFI remain dependency boundaries and are tested
through wheel-level Python integration tests.

## Install

```bash
pip install proofframe==0.5.0
```

Rust users can install the core without Python:

```bash
cargo add proofframe@0.5.0
```

ProofFrame supports Python 3.10–3.13 and Rust 1.85 or newer.

## Check a table

```python
import pyarrow as pa
import proofframe as pf

table = pa.table({
    "order_id": [101, 102, 102],
    "amount": [12.50, 8.00, -1.00],
})

contract = {
    "version": "proofframe.contract.v1",
    "columns": {
        "order_id": {"required": True, "not_null": True, "unique": True},
        "amount": {"required": True, "min": 0},
    },
    "max_findings": 20,
}

report = pf.check(
    table,
    contract,
    max_memory=64 * 1024 * 1024,
    max_temp=512 * 1024 * 1024,
    max_samples=20,
)

assert report["valid"] is False
assert report["violation_count"] == 2
```

`violation_count` is exact even when the `findings` sample is truncated. Missing required columns
and incompatible rule/type combinations are rejected during compilation, before rows are scanned.

Pandas, Polars, PyArrow tables, record batches, readers, and Arrow C Stream providers are accepted.
Inputs that expose a stream stay streaming; the CLI does not construct a full table for CSV or
Parquet input.

## Fingerprints

```python
legacy = pf.fingerprint(table, version="v1")
current = pf.fingerprint(table, version="v2")
```

Fingerprint versions are separate protocols. Never compare a V1 digest with a V2 digest. V1 is the
Python compatibility default for 0.5; the CLI defaults new work to V2:

```bash
proofframe fingerprint data.parquet --fingerprint-version v2
```

## Exact diff with bounded output

```python
result = pf.diff(
    before,
    after,
    keys="order_id",
    max_memory=256 * 1024 * 1024,
    max_temp=2 * 1024 * 1024 * 1024,
    max_samples=100,
    max_output_records=1_000_000,
    output="changes.jsonl",
)
```

Counts remain exact. In-memory examples are bounded by `max_samples`; complete change records can be
written atomically as JSON Lines or Arrow IPC, up to `max_output_records`. Temporary partitions carry
format version, schema digest, declared lengths, and a BLAKE3 checksum. Length limits are checked
before allocation.

## CLI

```bash
proofframe check data.parquet --contract contract.json --max-memory 256MiB --max-temp 2GiB
proofframe diff old.parquet new.parquet --key order_id --output changes.jsonl
proofframe fingerprint data.csv --fingerprint-version v2
```

Exit codes are stable:

| Code | Meaning |
| ---: | --- |
| 0 | Operation succeeded; a check or receipt is valid |
| 1 | Contract violation or invalid receipt |
| 2 | Invalid input, contract, or command configuration |
| 3 | Engine, I/O, schema, or corrupt-data failure |
| 4 | Resource limit exceeded |

JSON output is emitted only after a successful operation. File outputs use a same-directory temporary
file, flush and `fsync`, then an atomic replace.

## Compatibility

`validate()` and `profile()` remain migration shims for the 0.5 line. `validate()` delegates to
`check()` and no longer creates an implicit profile. New code should call `check()` and
`fingerprint()` separately.

See [the 0.4 to 0.5 migration guide](docs/migration-0.4-to-0.5.md) for API and serialized-contract
changes. Rust API details are in [README-crates.md](README-crates.md).

## Performance evidence

The release harness records raw samples, median, IQR, throughput, subprocess peak RSS, engine memory
and spill counters, algorithm version, schema, compiler, CPU, and dataset SHA-256. It refuses to
compare artifacts captured on different hardware, compilers, datasets, or fingerprint versions.

```bash
python benchmarks/release_gate.py \
  --rows 100000 --runs 5 --warmups 1 \
  --output target/release-gate-smoke.json
```

This command is a deterministic synthetic smoke test. The real Bitcoin fixture is identified by
`benchmarks/fixtures/bitcoin-7_6m-manifest.json`; the data itself is not redistributed. Methodology
and remaining stable-release gates are documented in [docs/testing.md](docs/testing.md).

## Development

```bash
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
maturin develop --release --locked
python -m pytest -q
```

CI additionally runs Miri-compatible state-machine tests, three 60-second fuzz targets, release-mode
allocation contracts, Python 3.10–3.13 on Linux/macOS/Windows, wheel smoke tests, and package checks.

Apache-2.0 licensed. Security reports follow [SECURITY.md](SECURITY.md).
