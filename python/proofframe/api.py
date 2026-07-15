"""Public Python API with Arrow C Stream interoperability."""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from typing import Any

import pyarrow as pa

from ._proofframe import (
    detect_leakage_arrow,
    diff_arrow,
    generate_signing_keypair,
    profile_arrow,
    scan_pii_arrow,
    sign_proof_receipt,
    validate_arrow,
    verify_proof_receipt,
)


def _as_reader(data: Any) -> pa.RecordBatchReader:
    if isinstance(data, pa.RecordBatchReader):
        return data
    if isinstance(data, pa.Table):
        return data.to_reader()
    if isinstance(data, pa.RecordBatch):
        return pa.RecordBatchReader.from_batches(data.schema, [data])
    if hasattr(data, "to_arrow"):
        return _as_reader(data.to_arrow())
    if data.__class__.__module__.split(".")[0] == "pandas":
        return pa.Table.from_pandas(data, preserve_index=False).to_reader()
    if hasattr(data, "__arrow_c_stream__"):
        return pa.RecordBatchReader.from_stream(data)
    raise TypeError(
        "Expected a PyArrow Table/RecordBatchReader, pandas DataFrame, Polars DataFrame, "
        "or Arrow C Stream provider"
    )


def profile(data: Any) -> dict[str, Any]:
    """Return a deterministic profile and BLAKE3 fingerprint for tabular data."""
    return json.loads(profile_arrow(_as_reader(data)))


def validate(data: Any, contract: Mapping[str, Any]) -> dict[str, Any]:
    """Validate data against a declarative contract in one streaming pass."""
    return json.loads(validate_arrow(_as_reader(data), json.dumps(contract, sort_keys=True)))


def diff(before: Any, after: Any, *, keys: str | Sequence[str]) -> dict[str, Any]:
    """Return added, removed, and column-level changed rows by stable key."""
    key_list = [keys] if isinstance(keys, str) else list(keys)
    return json.loads(diff_arrow(_as_reader(before), _as_reader(after), key_list))


def scan_pii(data: Any, *, max_findings: int = 100) -> dict[str, Any]:
    """Detect common PII classes without returning raw values."""
    if max_findings < 0:
        raise ValueError("max_findings must be non-negative")
    return json.loads(scan_pii_arrow(_as_reader(data), max_findings))


def detect_leakage(
    train: Any,
    test: Any,
    *,
    keys: str | Sequence[str] | None = None,
    max_samples: int = 20,
) -> dict[str, Any]:
    """Find exact key or full-row overlap while exposing only hashed sample IDs."""
    if max_samples < 0:
        raise ValueError("max_samples must be non-negative")
    key_list = [] if keys is None else ([keys] if isinstance(keys, str) else list(keys))
    return json.loads(
        detect_leakage_arrow(_as_reader(train), _as_reader(test), key_list, max_samples)
    )


def generate_keypair() -> dict[str, str]:
    """Generate an Ed25519 signing keypair encoded as URL-safe base64."""
    return json.loads(generate_signing_keypair())


def sign_receipt(report: Mapping[str, Any], *, private_key: str) -> dict[str, Any]:
    """Create an RFC 8785-canonicalized, Ed25519-signed proof receipt."""
    payload = json.dumps(report, sort_keys=True, separators=(",", ":"))
    return json.loads(sign_proof_receipt(payload, private_key))


def verify_receipt(receipt: Mapping[str, Any]) -> dict[str, bool]:
    """Verify receipt schema, report hash, and Ed25519 signature."""
    payload = json.dumps(receipt, sort_keys=True, separators=(",", ":"))
    return json.loads(verify_proof_receipt(payload))
