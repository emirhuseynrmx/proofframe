# Migrating from ProofFrame 0.4 alpha to 0.5

ProofFrame 0.5 separates contract compilation, validation, profiling, fingerprinting, evidence, and
trust. The compatibility shims are intended to make the transition observable, not permanent.

## Python

| 0.4 alpha | 0.5 | Compatibility |
| --- | --- | --- |
| `validate(data, contract)` | `check(data, contract)` | `validate` delegates to `check` and emits `DeprecationWarning` |
| `include_profile=True/False` | Separate `check` and `profile`/`fingerprint` calls | The argument is accepted but no implicit profile is built |
| Permissive contract mapping | `proofframe.contract.v1` | `check` inserts the version when absent; unknown fields are rejected |
| Generic native exception | Typed `ContractError`, `SchemaError`, `ResourceLimitError`, `ProofFrameIoError`, `ProofFrameArrowError`, `ProofFrameCorruptDataError`, `ReceiptError` | `CorruptDataError` remains an alias; all carry stable `PF_*` codes |
| `fingerprint(data)` | `fingerprint(data, version="v1"|"v2")` | Python defaults to V1 for this release; select V2 explicitly for new stored proofs |
| Unbounded result assumptions | `max_memory`, `max_temp`, `max_output_records`, `max_samples` | Limits fail closed; exact counts are independent from samples |
| `profile` as part of validation | `profile(..., distinct="none")` compatibility operation | Exact distinct is opt-in, spill-backed, and resource-limited |
| V1 report signing | `check_with_evidence`, `sign_evidence`, `verify_receipt(expected_public_key=...)` | Check and fingerprint now share one execution; V1 requires `receipt_version="v1"` |

Do not compare V1 and V2 digests. Persist the fingerprint version next to every stored digest.

## Contract documents

Add the version field:

```json
{
  "version": "proofframe.contract.v1",
  "columns": {
    "id": {"required": true, "not_null": true, "unique": true}
  },
  "max_findings": 100
}
```

Unknown root and rule fields now return `PF_CONTRACT_UNKNOWN_FIELD`. Bounds are parsed from their
exact JSON spelling and compiled for the Arrow type. A string rule on an integer column, an invalid
timestamp literal, or an out-of-range integer fails before scanning.

For floating-point bounds, NaN is rejected by default. Use `"nan": "allow"` only when the contract
explicitly permits it.

## Rust

| 0.4 alpha | 0.5 replacement |
| --- | --- |
| Deserialize `Contract` directly | `ContractAst::from_json` |
| `validate_fast_reader(reader, &contract)` | `CompiledContract::compile`, then `execute_reader` |
| Implicit fingerprint protocol | `FingerprintOptions::new(FingerprintVersion::V1|V2)` |
| `diff_readers(before, after, keys)` | `diff_readers_with_options` for explicit limits and output sink |
| V1 `sign_json` / `verify_json` | `EvidenceV2`, `sign_v2`, `verify_v2`, and `TrustPolicy` |

The legacy Rust entry points remain in 0.5. They do not gain the full compiled-contract resource
model and should not be used in new integrations.

## Reports and evidence

0.5 validation reports add native execution metrics: peak accounted memory, peak temporary storage,
spill bytes, exact-run count, and capacity-growth events. Consumers must ignore unknown report fields
rather than comparing a serialized report byte-for-byte across versions.

Evidence V2 binds these identities in one strict envelope:

- dataset fingerprint version, digest, and row count;
- canonical source contract, compiled plan, and Arrow schema digests;
- engine name and version;
- operation and resource limits;
- validity, exact violation count, and output count.

A valid signature proves integrity, not identity. Use `ExpectedKey` or `TrustStore` when signer trust
is part of the decision. `SignatureOnly` is suitable for integrity checks, not authorization.

## Operational rollout

1. Run 0.4 and 0.5 on the same frozen input and retain both reports.
2. Make the fingerprint version explicit before storing any new digest.
3. Set memory, temporary-storage, output, and sample limits from production constraints.
4. Alert separately on contract violations, resource exhaustion, corrupt persisted state, and
   untrusted signers.
5. Remove compatibility calls after the 0.5 rollout; they are not a long-term API guarantee.
