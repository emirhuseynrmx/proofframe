# ProofFrame in Dagster

An acceptance scan as an asset check, so a contract result appears where Dagster
already shows data quality rather than as a separate op nobody reads.

```sh
pip install proofframe-dagster
```

## Checking an asset

```python
from dagster import Definitions, asset
from proofframe_dagster import build_acceptance_check


@asset
def orders() -> None:
    write_orders_parquet("data/orders.parquet")


orders_accepted = build_acceptance_check(
    asset=orders,
    path="data/orders.parquet",
    contract="contracts/orders.json",
    output_path="evidence/orders.json",
)

defs = Definitions(assets=[orders], asset_checks=[orders_accepted])
```

The check is blocking by default, so a failure stops downstream assets. Pass
`blocking=False` to record the result without holding the graph.

## How three statuses become two fields

Acceptance answers `accepted`, `rejected` or `unknown`. A check result carries
`passed` and a severity, so the mapping is deliberate:

| Status | `passed` | Severity | Meaning |
| --- | --- | --- | --- |
| `accepted` | true | — | scanned, qualified under this policy |
| `rejected` | false | `ERROR` | scanned, did not qualify |
| `unknown` | false | `WARN` | the scan did not complete, so nothing was decided |

`unknown` is not a failing dataset. It is a missing file, an unreadable block, an
I/O error — the scan never reached a verdict. Collapsing it into `ERROR` would
report a decision that was never made, and collapsing it into a pass would report
one made the other way. Pass `unknown_severity="error"` when a pipeline should
treat an incomplete scan as hard as a rejection.

These are acceptance-layer semantics. A rejection is this contract under this
policy, not a general judgement about the data.

## Metadata on the check result

```
proofframe/status          accepted | rejected | unknown
proofframe/contract_id     digest of the contract that was applied
proofframe/policy_id       digest of the acceptance policy
proofframe/bundle_sha256   digest of the bundle
proofframe/signed          whether the bundle carries a signature
proofframe/rows            rows scanned          (absent on unknown)
proofframe/violations      violation count       (absent on unknown)
proofframe/schema_digest   resolved Arrow schema (absent on unknown)
proofframe/reasons         why, when there is a why
proofframe/bundle          path, when output_path was given
```

An `unknown` has no report, so the row-level fields are absent rather than zero.

## Signing

Pass `private_key` to sign the bundle. Without it the bundle is hash-bound: an
integrity checksum, not authentication. Signing is optional, and
`proofframe/signed` says which one this run produced.

## Mapping a bundle you already have

`acceptance_result` is the mapping on its own, for an op or sensor that ran the
scan itself:

```python
import proofframe as pf
from proofframe_dagster import acceptance_result

bundle = pf.accept_file("data/orders.parquet", contract)
result = acceptance_result(bundle)
```

## Resource limits

The scan runs in the Dagster process. The engine's own limits apply — 512 MiB
memory and 4 GiB temporary storage by default — and a scan that exceeds them
raises rather than growing.
