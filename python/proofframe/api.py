"""Public Python API with Arrow C Stream interoperability."""

from __future__ import annotations

import json
import warnings
from collections.abc import Mapping, Sequence
from typing import Any

import pyarrow as pa

from ._proofframe import (
    check_arrow,
    detect_leakage_arrow,
    diff_arrow,
    fingerprint_arrow,
    generate_signing_keypair,
    profile_arrow,
    scan_pii_arrow,
    sign_proof_receipt,
    verify_proof_receipt,
)


def _as_reader(data: Any) -> pa.RecordBatchReader:
    if isinstance(data, pa.RecordBatchReader):
        return data
    if isinstance(data, pa.Table):
        return data.to_reader()
    if isinstance(data, pa.RecordBatch):
        return pa.RecordBatchReader.from_batches(data.schema, [data])
    if hasattr(data, "__arrow_c_stream__"):
        return pa.RecordBatchReader.from_stream(data)
    if hasattr(data, "to_arrow"):
        return _as_reader(data.to_arrow())
    if data.__class__.__module__.split(".")[0] == "pandas":
        return pa.Table.from_pandas(data, preserve_index=False).to_reader()
    raise TypeError(
        "Expected a PyArrow Table/RecordBatchReader, pandas DataFrame, Polars DataFrame, "
        "or Arrow C Stream provider"
    )


def profile(data: Any, *, distinct: str = "exact") -> dict[str, Any]:
    """Return a deterministic profile and BLAKE3 fingerprint for tabular data."""
    if distinct not in {"none", "exact"}:
        raise ValueError("distinct must be 'none' or 'exact'")
    return profile_arrow(_as_reader(data), distinct)


def fingerprint(data: Any) -> str:
    """Return only the canonical dataset fingerprint without profile or distinct state."""
    return fingerprint_arrow(_as_reader(data))


def check(
    data: Any,
    contract: Mapping[str, Any],
    *,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    max_output_records: int = 100_000,
    max_samples: int = 100,
) -> dict[str, Any]:
    """Compile a strict contract and execute bounded, typed validation kernels."""
    normalized = dict(contract)
    normalized.setdefault("version", "proofframe.contract.v1")
    row_count_hint = data.num_rows if isinstance(data, (pa.Table, pa.RecordBatch)) else None
    return check_arrow(
        _as_reader(data),
        json.dumps(normalized, sort_keys=True, separators=(",", ":")),
        row_count_hint,
        max_memory,
        max_temp,
        max_output_records,
        max_samples,
    )


def validate(
    data: Any,
    contract: Mapping[str, Any],
    *,
    include_profile: bool = True,
    **options: Any,
) -> dict[str, Any]:
    """Compatibility alias for :func:`check` retained for the 0.5 release line."""
    warnings.warn(
        "validate() is retained for 0.5 compatibility; use check(). "
        "Validation no longer builds an implicit unbounded profile.",
        DeprecationWarning,
        stacklevel=2,
    )
    del include_profile
    return check(data, contract, **options)


def diff(before: Any, after: Any, *, keys: str | Sequence[str]) -> dict[str, Any]:
    """Return added, removed, and column-level changed rows by stable key."""
    key_list = [keys] if isinstance(keys, str) else list(keys)
    return diff_arrow(_as_reader(before), _as_reader(after), key_list)


def scan_pii(data: Any, *, max_findings: int = 100) -> dict[str, Any]:
    """Detect common PII classes without returning raw values."""
    if max_findings < 0:
        raise ValueError("max_findings must be non-negative")
    return scan_pii_arrow(_as_reader(data), max_findings)


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
    return detect_leakage_arrow(_as_reader(train), _as_reader(test), key_list, max_samples)


def generate_keypair() -> dict[str, str]:
    """Generate an Ed25519 signing keypair encoded as URL-safe base64."""
    return generate_signing_keypair()


def sign_receipt(report: Mapping[str, Any], *, private_key: str) -> dict[str, Any]:
    """Create an RFC 8785-canonicalized, Ed25519-signed proof receipt."""
    payload = json.dumps(report, sort_keys=True, separators=(",", ":"))
    return sign_proof_receipt(payload, private_key)


def verify_receipt(receipt: Mapping[str, Any]) -> dict[str, bool]:
    """Verify receipt schema, report hash, and Ed25519 signature."""
    payload = json.dumps(receipt, sort_keys=True, separators=(",", ":"))
    return verify_proof_receipt(payload)
