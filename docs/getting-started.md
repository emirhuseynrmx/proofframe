# Getting started

ProofFrame validates Arrow-shaped data with a versioned JSON contract. It accepts
PyArrow tables and readers, pandas DataFrames, Polars DataFrames, and Arrow C-stream
providers. The Python engine scans Arrow record batches; it does not turn each row
into a Python object.

## Install

```bash
pip install proofframe==0.6.0
```

Install optional dataframe libraries when needed:

```bash
pip install "proofframe[pandas]" "proofframe[polars]"
```

## Your first check

Create a contract and execute it against a dataframe. This takes the same path as
production validation: the contract is compiled before data is consumed.

```python
import pyarrow as pa
import proofframe as pf

orders = pa.table({"order_id": [101, 102, 102], "amount": [12.5, 8.0, -1.0]})
contract = {
    "version": "proofframe.contract.v1",
    "columns": {
        "order_id": {"required": True, "not_null": True, "unique": True},
        "amount": {"min": 0},
    },
}

report = pf.check(orders, contract, max_samples=20)
assert report["valid"] is False
print(report["violation_count"])
```

`valid` is the pass/fail verdict. `violation_count` remains exact even if
`findings` is capped by `max_samples`; use each finding's `rule`, `column`, `row`,
and `message` to locate the problem. Invalid contracts, incompatible Arrow types,
and resource limits raise a typed ProofFrame exception instead of returning an
invalid report.

## Start from observed data

In 0.6.0, `suggest_contract()` makes a **draft** V2 contract from a separate Arrow
scan. It can infer Arrow physical types, non-null columns, numeric ranges,
low-cardinality categories, and a row-count lower bound. A suggestion is not a
validation result and cannot be used until reviewed.

```python
draft = pf.suggest_contract(
    orders,
    infer_uniqueness=False,   # fast default; exact distinct is opt-in
    infer_categories=False,   # allowlists are opt-in
)
assert draft["status"] == "draft"

# Review inferred limits and any suggested_from.review entries first.
draft["status"] = "active"
report = pf.check(orders, draft)
```

`infer_uniqueness=True` performs exact distinct inference and may spill to disk;
set `max_memory`, `max_temp`, and `spill` for that workload. By default the scanner
does not create ranges for timestamps or monotonically increasing numeric columns;
the reason is recorded in `suggested_from.review`. This avoids a new batch failing
only because time or an identifier advanced.

The CLI uses the same model:

```bash
proofframe suggest data.parquet > contract.json
# review contract.json, then set "status" to "active"
proofframe check data.parquet --contract contract.json
```

## Choose explicit resource limits

Rules that need exact global state—such as uniqueness, distinct ratios, and keyed
diffs—can use temporary storage. Bound them at the call site:

```python
report = pf.check(
    orders,
    contract,
    max_memory=256 * 1024 * 1024,
    max_temp=2 * 1024 * 1024 * 1024,
    spill="auto",
)
```

Use `spill="never"` and `max_temp=0` when disk spill is prohibited. A requested
operation that cannot stay within these limits fails closed with `ResourceLimitError`.

Next, read [the contract reference](contracts.md), [the API reference](api.md), and
[the concepts guide](concepts.md).
