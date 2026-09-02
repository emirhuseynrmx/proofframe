# Contract reference

Contracts are JSON-compatible mappings. ProofFrame validates a contract against the
Arrow schema before scanning rows. Unknown fields and values that cannot be represented
by the target Arrow type are contract errors.

## Common document fields

| Field | V1 | V2 | Meaning |
| --- | --- | --- | --- |
| `version` | optional | required | `proofframe.contract.v1` or `proofframe.contract.v2`; the Python API defaults a missing version to V1. |
| `columns` | optional | optional | Mapping from Arrow column name to a column-rule object. |
| `max_findings` | optional | optional | Maximum retained row findings; exact counts still continue. |
| `status` | no | optional | `active` or `draft`; absent means `active` for backward compatibility. |
| `suggested_from` | no | optional | Draft provenance created by `suggest_contract()`; it is not a rule. |

`status: "draft"` is rejected by `check`, `check_with_evidence`, both partition
checks, and `validate` with `PF_DRAFT_CONTRACT`. Review the contract, change it to
`"active"`, then validate it.

## V1 column rules

```json
{
  "version": "proofframe.contract.v1",
  "columns": {
    "order_id": {"required": true, "not_null": true, "unique": true},
    "amount": {"min": 0, "max": 100000, "nan": "reject"},
    "country": {"allowed": ["TR", "GB", "US"]},
    "email": {"pattern": "^[^@]+@[^@]+$"}
  },
  "max_findings": 100
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `required` | boolean | The column must exist in the input schema. |
| `not_null` | boolean | Every observed value must be non-null. |
| `unique` | boolean | Every non-null canonical value must occur once; exact state can spill. |
| `min`, `max` | typed JSON scalar | Inclusive bounds for compatible numeric and temporal Arrow types. |
| `nan` | `"allow"` or `"reject"` | Float NaN policy. |
| `pattern` | string | Regular expression for UTF-8 values. |
| `allowed` | array of strings | Closed UTF-8 allowlist. |

Omit a field to omit that rule. `required` is deliberately independent from
`not_null`: a present column may allow nulls, and an optional column may have rules
when it is present.

## V2 column rules and physical types

V2 keeps the same value rules under `columns`, and adds `type` to require a physical
Arrow type. Primitive type names are `boolean`, `int8`, `int16`, `int32`, `int64`,
`uint8`, `uint16`, `uint32`, `uint64`, `float32`, `float64`, `date32`, `date64`,
`utf8`, `large_utf8`, `utf8_view`, `binary`, `large_binary`, and `binary_view`.
Parameterized types use `{"name": "timestamp", "unit": "us", "timezone": "UTC"}`
or `{"name": "decimal128", "precision": 12, "scale": 2}`. Timestamp units are
`s`, `ms`, `us`, and `ns`; `timezone` is optional.

```json
{
  "version": "proofframe.contract.v2",
  "columns": {
    "order_id": {"type": "int64", "not_null": true},
    "created_at": {"type": {"name": "timestamp", "unit": "us", "timezone": "UTC"}}
  }
}
```

## V2 row rules

`row_rules` is an array. Each item has a unique `name` and either a `compare` rule or
a conditional `when` plus `assert` rule. Comparison operands are exactly one
`{"column": "name"}` or `{"literal": value}`; at least one operand is a column.
`op` is one of `eq`, `ne`, `lt`, `lte`, `gt`, or `gte`. The optional `nulls` policy
is `"skip"` (the default), `"fail"`, or `"equal"`.

```json
{
  "name": "total_covers_subtotal",
  "compare": {
    "left": {"column": "total"}, "op": "gte", "right": {"column": "subtotal"}
  }
}
```

```json
{
  "name": "delivered_has_tracking",
  "when": {"left": {"column": "status"}, "op": "eq", "right": {"literal": "delivered"}},
  "assert": {"column": "tracking_id", "not_null": true}
}
```

The conditional assertion supports `not_null`, `min`, `max`, `nan`, `pattern`, and
`allowed`; it does not create a schema-level required or unique rule.

## V2 dataset rules

`dataset_rules` groups rules that need the complete dataset (or all partitions).

| Field | Shape | Meaning |
| --- | --- | --- |
| `row_count` | `{"exact": n, "min": n, "max": n}` | Total-row count constraint. |
| `null_ratio` | `{column: {"min": x, "max": x}}` | Inclusive null fraction, from 0 through 1. |
| `distinct_count` | `{column: {"exact": n, "min": n, "max": n}}` | Exact distinct non-null count. |
| `distinct_ratio` | `{column: {"min": x, "max": x}}` | Exact distinct count divided by total rows. |
| `composite_unique` | array | Exact unique combinations of named columns. |
| `references` | array | Every key must also appear in a named reference dataset. |

```json
{
  "dataset_rules": {
    "row_count": {"min": 1},
    "distinct_ratio": {"order_id": {"min": 1.0}},
    "composite_unique": [{
      "name": "line_key", "columns": ["order_id", "line_id"], "nulls": "equal"
    }]
  }
}
```

For `composite_unique`, `nulls` is `"equal"` (the default) or `"reject"`. Exact distinct and
composite state follow the resource limits supplied to the API or CLI.

## Referential integrity

A `references` rule states that every key in this dataset also exists in another one.

```json
{
  "dataset_rules": {
    "references": [{
      "name": "orders_customer_fk",
      "columns": ["customer_id"],
      "reference": "customers",
      "reference_columns": ["id"],
      "nulls": "skip"
    }]
  }
}
```

`reference` is a logical name, not a path. The contract says *what* must hold; the caller
supplies the dataset it holds against:

```python
report = pf.check(orders, contract, references={"customers": customers})
```

```bash
proofframe check orders.parquet --contract contract.json \
  --reference customers=customers.parquet
```

**The rule is checked or the run fails.** A contract that declares a reference the caller did
not bind is an error, not a skipped rule, because a foreign key that is never evaluated
reports as one that held. A bound dataset that no rule uses is an error too: it is almost
always a misspelled name, which would otherwise look like it had been checked.

`columns` and `reference_columns` pair **by position**, so the two datasets may name the same
identity differently, and both sides must have equal length. The Arrow types must match at
each position — the key encoding is type-tagged, so an `Int32` key would never match an
`Int64` one and every row would report as a violation. Rejecting the pair is the honest
answer. Timestamps compare by unit; a timezone is display metadata over the same ticks.

`nulls` is `"skip"` (the default) or `"reject"`. Skipping matches SQL: a key that is partly
unknown is not a claim about membership. Nulls on the *reference* side are never identities
anything can match, whichever policy the rule sets.

Findings report the first row that carried each absent key, and `violation_count` counts
**distinct absent keys** rather than the rows carrying them. The report also records what the
keys were resolved against:

```json
"references": [{
  "name": "orders_customer_fk",
  "reference": "customers",
  "reference_fingerprint": "pf-fp-v2:…",
  "reference_rows": 4096,
  "reference_distinct_keys": 4096,
  "checked_distinct_keys": 812,
  "missing_distinct_keys": 0
}]
```

The fingerprint is the point of that record: "the foreign key held" is not a verifiable claim
unless it also says which dataset it held against. Reference rules use the same bounded,
spilling exact-set machinery as uniqueness and stay within the supplied resource limits.

Under `check_partitions`, a reference rule puts the run on the single ordered traversal, so
every key resolves against the whole reference dataset exactly once however the subject was
partitioned.

## Suggestion provenance

Suggested V2 contracts include a machine-readable draft record:

```json
{
  "status": "draft",
  "suggested_from": {
    "proofframe_version": "0.6.0",
    "dataset_fingerprint": "pf-fp-v1:…",
    "rows_observed": 7645034,
    "uniqueness_inferred": false,
    "review": [{"column": "created_at", "reason": "timestamp_range_omitted"}]
  }
}
```

`review` calls out intentionally omitted suggestions. `status` and `suggested_from`
are operational source metadata: they are included in the contract-source digest but
not in the compiled-plan digest. Therefore activating a reviewed draft changes its
source identity without changing the identity of the rules it runs.
