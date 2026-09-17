# ProofFrame in Apache Airflow

Two operators that run an acceptance scan inside the worker process, through the
Python bindings rather than a subprocess.

```sh
pip install proofframe-airflow
```

## Deciding

```python
from proofframe_airflow import ProofFrameAcceptOperator

validate = ProofFrameAcceptOperator(
    task_id="validate_orders",
    path="/opt/airflow/data/orders.parquet",
    contract="/opt/airflow/contracts/orders.json",
    output="/opt/airflow/evidence/{{ ds }}/orders.json",
)
```

`path` and `output` are templated, so `{{ ds }}` and the rest of the Airflow
context resolve as usual.

## What each status does to the task

| Status | Task | Why |
| --- | --- | --- |
| `accepted` | succeeds, returns a summary | |
| `rejected` | `AirflowFailException` | the same bytes under the same contract cannot decide differently, so a retry is wasted |
| `unknown` | `on_unknown`, default fail | the scan did not complete; a retry can legitimately change this |

These are acceptance-layer statuses, not a general verdict on the data. A
rejection is this contract under this policy, and an `unknown` is a scan that
could not finish — a missing file, an unreadable block — not a finding about the
rows.

Set `on_unknown="skip"` to let the rest of the DAG proceed without the branch, or
`on_unknown="pass"` to log and continue.

## What travels through XCom

An acceptance bundle carries the full report and the Evidence V2 envelope. Airflow
stores XCom in its metadata database, so the operator pushes only the decision and
the digests that identify it:

```json
{
  "status": "accepted",
  "reasons": [],
  "sha256": "...",
  "signed": false,
  "contract_id": "...",
  "policy_id": "...",
  "schema_digest": "...",
  "violation_count": 0,
  "rows": 148230,
  "bundle_path": "/opt/airflow/evidence/2026-09-16/orders.json"
}
```

The bundle itself is written to `output`, and `bundle_path` names it. Omit
`output` and nothing is written; `bundle_path` is then null.

`accept_file` refuses to overwrite an existing `output`. Give the path something
per-run, as the `{{ ds }}` above does, or a cleared task will fail on the file its
first attempt wrote.

## Signing

Pass `private_key` to sign the bundle. Without it the bundle is hash-bound: an
integrity checksum, editable by anyone who can write the file. Signing is
optional and the summary's `signed` field says which one you got.

## Verifying downstream

```python
from proofframe_airflow import ProofFrameVerifyOperator

verify = ProofFrameVerifyOperator(
    task_id="verify_orders",
    bundle_path="/opt/airflow/evidence/{{ ds }}/orders.json",
    expected_public_key="{{ var.value.proofframe_public_key }}",
)
```

Verification rescans nothing. It checks that the bundle's bytes still bind to its
digests and, when a key is pinned, that the signature is that signer's. An
authentic bundle is not an accepted one, so the task also fails on a bundle that
verifies but is `rejected` or `unknown`; pass `require_accepted=False` when you
only want the integrity check.

## Resource limits

The scan runs in the worker, so the worker's memory is what it uses. The engine's
own limits still apply — 512 MiB memory and 4 GiB temporary storage by default —
and a scan that exceeds them raises rather than growing. Pass acceptance
`policy` and read options as you would to `proofframe.accept_file`.

## Example DAG

`example_dags/proofframe_validation_dag.py` runs the CLI through a `BashOperator`
instead, for deployments where the bindings are not installed on the worker.
