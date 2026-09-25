<div align="center">
  <img src="https://raw.githubusercontent.com/emirhuseynrmx/proofframe/main/assets/banner.png" alt="ProofFrame 0.7.2 — verifiable data contracts" width="100%" />
  <br/><br/>
  <a href="https://github.com/sponsors/emirhuseynrmx"><img src="https://img.shields.io/badge/Sponsor_ProofFrame-%E2%9D%A4-db61a2?style=for-the-badge&logo=githubsponsors&logoColor=white" alt="Sponsor ProofFrame on GitHub Sponsors" height="36" /></a>
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
[![Sponsor](https://img.shields.io/badge/Sponsor-%E2%9D%A4-db61a2?logo=githubsponsors&logoColor=white)](https://github.com/sponsors/emirhuseynrmx)

**A validator that tells you when it did not run, and hands you the evidence when it did.**

ProofFrame is a Rust data-quality engine for PyArrow, Pandas, Polars, CSV, Parquet and Arrow
streams. It compiles a contract against the physical schema before the first row is read, scans
record batches without turning cells into Python objects, and returns a decision you can verify
months later without the data in front of you.

## Sponsor ProofFrame

ProofFrame is built and maintained by one person, in the open, under Apache-2.0. Sponsorship
pays for the parts that do not ship as features: security fixes like the ones in 0.7.2,
keeping the Airflow and Dagster packages current, and the logical fingerprint mode planned for
0.8. If ProofFrame guards data you depend on, sponsoring keeps it maintained.

<p align="center"><a href="https://github.com/sponsors/emirhuseynrmx"><img src="https://img.shields.io/badge/Sponsor_ProofFrame-%E2%9D%A4-db61a2?style=for-the-badge&logo=githubsponsors&logoColor=white" alt="Sponsor ProofFrame on GitHub Sponsors" height="36" /></a></p>

## What is different

**The answer has three values, not two.** `accepted`, `rejected`, `unknown`. A file that could not
be opened, a limit that was exhausted, a schema that did not match — none of those become an
acceptance. Most validators have nowhere to put *the check did not run*, so it lands on one of the
two real outcomes, and which one you inherit is an accident of how the error was caught.

**The verdict travels with what produced it.** Evidence V2 binds the dataset fingerprint, the
resolved Arrow schema, the contract source, the compiled plan and the resources the scan actually
used. Verification is offline and rescans nothing — it proves the envelope, which is what lets it
tell you that the contract you are reading now is not the contract that ran.

**Counts are exact while output stays bounded.** You get the true number of violations and a capped
set of example rows, rather than a sampled estimate or an unbounded dump. A truncated finding list
says it was truncated; the count beside it is still the count.

**The limits are stated before the scan, not discovered during it.** 512 MiB of memory and 4 GiB of
temporary storage by default, charged before each allocation rather than observed afterwards. A scan
that would exceed them fails instead of growing, and `spill="never"` refuses rather than touching
the disk at all. Corrupt temporary data, ambiguous contracts and incompatible schemas fail closed.

**One compiled engine behind every surface.** The CLI, the Python package, the browser build and the
scheduler operators call the same compiled functions. The offline review you save from a browser tab
is byte for byte the document the CLI writes, because no second renderer exists to disagree with the
first.

**A new rule cannot invalidate an old receipt.** A rule joins the compiled plan digest only when a
contract uses it, and the V1 contract format and both fingerprint protocols stay frozen. Receipts
written before a release keep verifying after it, and there is a test for each rule that says so.

> **0.7.2 — Trust fixes, and the contract moves into the scheduler**
>
> **Security.** A receipt or acceptance bundle is `valid` only under a key you trust.
> Before 0.7.2 a receipt signed with any freshly generated key verified as `valid`
> when no key was pinned, and an unsigned acceptance bundle whose decision was edited
> and re-hashed verified as `valid`. Now `valid` means intact *and* signed by the key
> you pinned; `intact` reports integrity alone, and the CLI asks for
> `--expected-public-key` or an explicit `--integrity-only`. See the
> [changelog](CHANGELOG.md#072) for every change and how to adapt.
>
> Airflow and Dagster get real packages instead of a sample DAG. `proofframe-airflow`
> ships `ProofFrameAcceptOperator` and `ProofFrameVerifyOperator`; `proofframe-dagster`
> ships `build_acceptance_check`. Both call the bindings in process rather than shelling
> out to the CLI, so the library's exception types survive and the engine's own memory
> and temporary-storage limits are the ones that apply.
>
> Acceptance answers with three statuses and each scheduler offers two, so neither
> mapping is left to the caller. Dagster reports `accepted` as a pass and fails
> `rejected` and `unknown` at `ERROR`, so a blocking check stops downstream assets on data
> nobody accepted; the description still says `unknown`. Airflow raises the
> non-retryable failure on `rejected`, because the same bytes under the same contract
> cannot reach a different decision, and leaves `unknown` to `on_unknown`, which is the
> one status a retry can legitimately change.
>
> The Airflow operator pushes a summary rather than a bundle. XCom lives in the
> scheduler's metadata database and a bundle carries the full report and the Evidence V2
> envelope; the decision, the digests that identify it, and the path the bundle was
> written to go through instead.
>
> **0.7.1 — Nine more rules, and a review every surface can write**
>
> Ordering, step, exclusivity, totals, mean, standard deviation, conditional uniqueness,
> ledger balance, category dominance, row-count drift, text length, and dates written as
> dates. Each joins the plan digest only when a contract uses it, so existing receipts
> keep verifying. Uniqueness no longer needs a writable directory, so it runs on a
> read-only filesystem and in a browser, failing closed on the memory budget instead of
> on a missing folder. A profile drops the one count it cannot finish rather than the
> whole scan, and says which. `review_html` and `review_markdown` are part of the crate,
> so every surface renders the same report from the same code.
>
> Earlier releases are in the [changelog](CHANGELOG.md).

## Try it without installing anything

[**Drop a CSV into the browser linter →**](https://emirhuseyin.tech/proofframe/#linter)

The engine is compiled to WebAssembly and runs in the tab: it reads the file with Arrow, infers a
draft contract, refuses to execute that draft until you have reviewed it, and renders the same HTML
review `proofframe review` writes. Nothing is uploaded, and the page reports the engine time it
measured on your machine.

Uniqueness and the dataset-level exact rules run there too, in memory, failing closed on the
budget rather than on a missing folder. Only `references` is refused, because it resolves against a
second dataset the page cannot bind; a contract that uses it is still checked for everything else,
and that result is reported as incomplete and carries no evidence.

## Review a dataset

```bash
proofframe review orders.parquet --contract contract.json --out review-run
# Open review-run/index.html. To include finding details, use a NEW folder:
proofframe review orders.parquet --contract contract.json --out review-details --max-samples 20
```

**CI:** `review` exits **1** on contract violations; existing `evidence` still exits **0**
when evidence generation succeeds. Samples default to zero. Contracts and column names
are not redacted. Nothing is uploaded. Read the [review guide](docs/review.md) for limits,
privacy, signing, output publication and the [runnable demo](examples/review_demo.py).

## Accept or reject a delivery

A review tells you what the data looks like. Acceptance turns that into a decision an
application can act on.

```bash
proofframe accept orders.csv --contract contract.json --policy policy.json \
  --csv-options reader.json --private-key-file signer.key --output acceptance.json
proofframe verify-acceptance acceptance.json --expected-public-key "$PROOFFRAME_PUBLIC_KEY"
```

```python
bundle = pf.accept_file("orders.csv", contract, policy=policy, csv_options=options)
bundle["payload"]["decision"]["status"]   # "accepted", "rejected" or "unknown"
```

Three answers, and the third is a real one: a missing file, a parse failure or an exhausted
resource limit produces **unknown**, never a quiet **accepted**.

Zero violations is not the same as a result. A policy can require that named columns actually
had values evaluated, so a file cannot be accepted because nothing was asked of it. The limit
is stated plainly: those counters are per column, not proof that every rule ran.

CSV delimiter, encoding, decimal separator, column types and null tokens are chosen by you and
recorded in the bundle under their own identity, so two systems reading the same file either
agree or disagree visibly. `1.234` is not silently guessed.

`verify_acceptance` checks the bundle offline without rescanning, and checks its shape before
it trusts any digest — a hash proves that what is present was not edited and says nothing about
what is absent. It returns three answers: `intact` (the payload binds to its digests and any
signature on it is correct), `authenticated` (the key you pinned signed it) and `valid` (both).
An unsigned hash can be recomputed by anyone, so a bundle is never `valid` without a pinned key.
Ed25519 signing (`proofframe[signing]`) covers the whole payload.

Read the [acceptance guide](docs/acceptance.md) for the decision contract and its limits, and
[the runnable example](examples/accept_and_verify.py) for an accepted delivery, a rejected one,
one that could not be read, and a tampered bundle that fails verification.

## In GitHub Actions

Check a delivered file on the way in, and fail the job when it does not match the
contract. Nothing is uploaded anywhere: the check runs on the runner.

```yaml
- uses: emirhuseynrmx/proofframe@v1
  with:
    path: data/orders.csv
    contract: contracts/orders.json
```

The default mode writes an offline HTML review, a Markdown summary, report JSON
and Evidence V2, uploads them as a build artifact, puts the summary in the job
summary, and exits non-zero on a violation.

`accept` mode answers a different question — whether to take the delivery at all:

```yaml
- id: gate
  uses: emirhuseynrmx/proofframe@v1
  with:
    path: data/orders.csv
    contract: contracts/orders.json
    policy: contracts/acceptance.json
    mode: accept
    out: acceptance.json

- if: steps.gate.outputs.status == 'accepted'
  run: ./load-into-warehouse.sh
```

`status` is `accepted`, `rejected` or `unknown`. A missing file, a parse failure
or an exhausted limit answers `unknown` and fails the step; it never becomes an
acceptance. Set `fail-on-unknown: false` to handle that case yourself — to retry
a delivery that has not landed yet, for example.

Findings are not sampled by default: a sample can carry values out of your data
and into a build artifact. Set `max-samples` when you want them.

## In Airflow and Dagster

The same acceptance scan, inside the worker rather than in a subprocess.

```python
from proofframe_airflow import ProofFrameAcceptOperator

validate = ProofFrameAcceptOperator(
    task_id="validate_orders",
    path="/opt/airflow/data/orders.parquet",
    contract="/opt/airflow/contracts/orders.json",
    output_path="/opt/airflow/evidence/{{ ds }}/orders.json",
)
```

`accepted` succeeds and returns a summary. `rejected` raises Airflow's non-retryable
failure, because the same bytes under the same contract cannot decide differently.
`unknown` is the one status a retry can change, so it is the one left configurable
through `on_unknown`. `ProofFrameVerifyOperator` checks a bundle a previous task wrote,
and rescans nothing.

In Dagster the result belongs on the asset that produced the file:

```python
from proofframe_dagster import build_acceptance_check

orders_accepted = build_acceptance_check(
    asset=orders,
    path="data/orders.parquet",
    contract="contracts/orders.json",
    output_path="evidence/orders.json",
)
```

`accepted` passes; `rejected` and `unknown` fail the check at `ERROR`. The check is
blocking by default and Dagster stops downstream assets only on `ERROR`, so data nobody
accepted does not flow on. The description still says `unknown` for a scan that never
completed; pass `unknown_severity="warn"` to let downstream assets run on it.

Both packages put the decision, the digests and the bundle path in the scheduler's own
metadata, and leave the report and the Evidence V2 envelope on disk.

`proofframe-airflow` requires `apache-airflow>=2.7,<3`. The operators import
`BaseOperator` from `airflow.models`, which is where Airflow 2 keeps it; Airflow 3 moves
the provider surface to `airflow.sdk`. The ceiling is there so a resolver is not told
that a version nothing has been tested against will work, and it lifts in the release
that adds a 3.x job to CI beside the 2.10.5 one.

## Install

Python 3.10–3.13:

```bash
pip install proofframe==0.7.2
```

Rust 1.85 or newer:

```bash
cargo add proofframe@0.7.2
```

The scheduler integrations are separate distributions, so the core package keeps its
dependency surface and you install only the one you run:

```bash
pip install proofframe-airflow==0.7.2
pip install proofframe-dagster==0.7.2
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

```json
{
  "dataset_rules": {
    "monotonicity": [{ "name": "clock", "column": "ts", "direction": "strictly_increasing" }],
    "gap_detection": [{ "name": "bars", "column": "ts", "expected_step": 60 }],
    "mutually_exclusive": [{ "name": "tax_id", "columns": ["tckn", "vkn"] }],
    "sum": [{ "name": "turnover", "column": "amount", "max": 50000000 }],
    "mean": [{ "name": "latency", "column": "latency_ms", "max": 45 }],
    "std_dev": [{ "name": "spread", "column": "latency_ms", "max": 15 }],
    "balance_equal": [{ "name": "books", "left_column": "debit", "right_column": "credit" }],
    "max_dominant_value_ratio": [{ "name": "skew", "column": "country", "max": 0.7 }],
    "conditional_unique": [
      {
        "name": "live_ids",
        "columns": ["id"],
        "when": { "left": { "column": "is_deleted" }, "op": "eq", "right": { "literal": false } }
      }
    ]
  }
}
```

A step is measured in the column's own units and findings name them. A total is
counted in 128-bit integers, or with compensated addition for floats, so it does not
change with the reader's batch size. A statistic over no values is reported as such
rather than as zero.


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

### Check a foreign key across datasets

```python
orders = pa.table({"customer_id": [1, 99, 2]})
customers = pa.table({"id": [1, 2]})

contract = {
    "version": "proofframe.contract.v2",
    "dataset_rules": {
        "references": [{
            "name": "orders_customer_fk",
            "columns": ["customer_id"],
            "reference": "customers",
            "reference_columns": ["id"],
        }],
    },
}

report = pf.check(orders, contract, references={"customers": customers})
assert report["valid"] is False
assert report["findings"][0]["row"] == 1
assert report["references"][0]["reference_fingerprint"].startswith("pf-fp-v2:")
```

The contract names the reference; the caller supplies it. A declared reference with no bound
dataset, and a bound dataset no rule uses, are both errors: a foreign key that is never evaluated
would otherwise report as one that held. The report records the fingerprint of the dataset the keys
resolved against, because "the key held" is not verifiable without saying against what.

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
| [Great Expectations](https://docs.greatexpectations.io/) | You need a large expectation library, data docs, and broad orchestration connectors. | ProofFrame has a narrower rule surface and a smaller connector set — GitHub Actions, Airflow, Dagster and dbt — but keeps validation and proof artifacts compact. |
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
proofframe verify receipt.json --integrity-only   # intact, whoever signed it
```

`verify` and `verify-acceptance` exit 0 only for a receipt or bundle signed by the key you
pinned. Without a key they exit 1 and say why; `--integrity-only` checks integrity alone and
says that the signer was not verified. `sign` reads the key from `--private-key-file`; the
older `--private-key` still works but warns, because a key on the command line lands in shell
history and process listings.

| Exit code | Meaning |
| ---: | --- |
| 0 | Operation succeeded; check or receipt is valid |
| 1 | Contract violation, invalid receipt, or no trusted key given |
| 2 | Invalid input, contract, or command configuration |
| 3 | Engine, I/O, schema, or corrupt-data failure |
| 4 | Resource limit exceeded |

JSON is emitted only after a successful operation. File outputs use same-directory temporary files,
`fsync`, and atomic replacement.

## Rust core

The default crate has no Python dependency. Rust users get the same compiled contracts, typed Arrow
kernels, fingerprints, evidence, receipt verification, resource accounting, and spill engine used
by the Python wheels.

Since 0.7.1 that includes the review itself. `review_html` and `review_markdown` render the report
and the CI summary from a report, its evidence and the contract, so the document a Rust caller
produces is the document `proofframe review` writes.

```rust
let html = proofframe::review_html(&report, &evidence, &contract, "orders.parquet", &columns)?;
```

See the [crate guide](README-crates.md) and [API documentation](https://docs.rs/proofframe).

## Compatibility and performance evidence

Version 0.7.2 preserves V1 fingerprints and the established compatibility entry points. New work
should use `check`, explicit fingerprint versions, Evidence V2, and Receipt V2.

Known limitation: a fingerprint binds the physical Arrow types, not the logical values. The same
rows arriving as `large_string` from Polars and as `string` from PyArrow produce different
fingerprints. Changing that would invalidate every existing receipt, so a separate logical
fingerprint mode is planned for 0.8 rather than slipped into a patch release. Cast to one schema
before fingerprinting when two producers must agree.

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
[SECURITY.md](SECURITY.md).

If ProofFrame is useful to you, [sponsor it on GitHub](https://github.com/sponsors/emirhuseynrmx).
