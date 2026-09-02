# Python API reference

All tabular inputs may be a `pyarrow.Table`, `RecordBatch`, `RecordBatchReader`,
pandas DataFrame, Polars DataFrame, or Arrow C-stream provider. APIs with exact
global state accept `max_memory`, `max_temp`, and `spill="auto"|"never"`; use
`spill="never"` to prohibit temporary storage.

## Profiles and suggestions

- `profile(data, *, distinct="none", max_memory=512<<20, max_temp=4<<30, spill="auto") -> dict`
  returns rows, per-column metrics, and a deterministic V1 dataset fingerprint.
  `distinct="exact"` is opt-in, warns because it may spill, and returns exact counts.
- `fingerprint(data, *, version="v1") -> str` returns only the canonical dataset
  fingerprint. Supported versions are `v1` and `v2`.
- `suggest_contract(data, *, infer_uniqueness=False, infer_categories=False,
  max_categories=20, infer_required=False, infer_ranges=True,
  range_tolerance=0.0, infer_row_count=True, max_memory=512<<20, max_temp=4<<30,
  spill="auto") -> dict` performs its own Arrow scan and returns a V2
  `status: "draft"` contract. Exact uniqueness, required columns, and allowlists are
  only inferred when explicitly requested. Timestamp and monotonically increasing
  numeric ranges are omitted and recorded for review. This API is new in 0.6.0.

## Validation

- `check(data, contract, *, max_memory=..., max_temp=..., max_output_records=100000,
  max_samples=100, spill="auto", threads=None) -> dict` compiles and validates one
  input. The result includes `valid`, `rows`, `violation_count`, bounded `findings`,
  resource metrics, `compiled_plan_digest`, and `contract_source_digest`.
- `check_with_evidence(data, contract, **options) -> dict` validates and fingerprints
  the input in one native execution. It returns `{"report": ..., "evidence": ...}`.
- `check_partitions(partitions, contract, **options) -> dict` validates ordered
  partitions with global dataset rules.
- `check_partitions_with_evidence(partitions, contract, **options) -> dict` returns
  a report plus Evidence V2 and an ordered partition manifest.
- `validate(data, contract, *, include_profile=True, **options) -> dict` is a
  deprecated compatibility alias for `check`; it emits `DeprecationWarning`.

All validation entry points raise `ContractError` for invalid contracts and for
`status: "draft"` (`code == "PF_DRAFT_CONTRACT"`) before record batches are consumed.

## Comparison and privacy scans

- `diff(before, after, *, keys, max_memory=..., max_temp=...,
  max_output_records=100000, max_samples=100, output=None, output_format="jsonl",
  spill="auto") -> dict` computes an exact keyed diff. The report has added and
  removed keys plus changed-column samples; set `output` to atomically write all
  records as JSON Lines or Arrow IPC.
- `scan_pii(data, *, max_findings=100, fingerprint_mode="unlinkable",
  fingerprint_key=None, key_id=None) -> dict` detects known PII patterns without
  returning raw values. Use `fingerprint_mode="stable"` plus a 32-byte encoded key
  only when controlled correlation is required.
- `detect_leakage(train, test, *, keys=None, max_samples=20) -> dict` finds exact
  overlap by business key or full row and returns bounded hashed samples.

## Evidence and receipts

- `generate_keypair() -> dict[str, str]` returns URL-safe base64 Ed25519 private and
  public keys. Store the private key in a secret manager.
- `sign_evidence(evidence, *, private_key) -> dict` creates a V2 Ed25519 receipt.
- `sign_receipt(evidence, *, private_key, receipt_version="v2") -> dict` signs V2
  evidence by default; `receipt_version="v1"` is an explicit migration path.
- `verify_receipt(receipt, *, expected_public_key=None) -> dict[str, bool]` verifies
  receipt integrity and, when supplied, expected signer identity.
- `assemble_evidence_unchecked(data, contract, report) -> dict` builds an envelope
  around a caller-supplied report. Prefer `check_with_evidence`: this escape hatch
  cannot prove the report and input came from one validation pass.

## Exceptions

`ContractError`, `SchemaError`, `ResourceLimitError`, `ReceiptError`,
`ProofFrameArrowError`, `ProofFrameIoError`, and `ProofFrameCorruptDataError` derive
from `ProofFrameError`. Contract and resource errors expose stable error codes; use
those codes rather than parsing human-readable messages.
