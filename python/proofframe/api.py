"""Public Python API with Arrow C Stream interoperability."""

from __future__ import annotations

import json
import warnings
from collections.abc import Mapping, Sequence
from typing import Any

import pyarrow as pa

from ._proofframe import (
    assemble_evidence_unchecked_arrow,
    check_arrow,
    check_partitions_arrow,
    check_partitions_with_evidence_arrow,
    check_with_evidence_arrow,
    detect_leakage_arrow,
    diff_arrow,
    fingerprint_arrow,
    generate_signing_keypair,
    profile_arrow,
    scan_pii_arrow,
    sign_evidence_receipt,
    sign_proof_receipt,
    verify_proof_receipt_any,
    suggest_arrow,
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


def _row_count_hint(data: Any) -> int | None:
    """Return an exact, non-consuming row count for known tabular containers."""
    if isinstance(data, (pa.Table, pa.RecordBatch)):
        return data.num_rows
    root_module = data.__class__.__module__.split(".")[0]
    if root_module == "polars":
        height = getattr(data, "height", None)
        return height if isinstance(height, int) and height >= 0 else None
    if root_module == "pandas":
        return len(data.index)
    return None


def _byte_size_hint(data: Any) -> int | None:
    """Return a non-consuming logical byte estimate for trusted containers."""
    if isinstance(data, (pa.Table, pa.RecordBatch)):
        return data.nbytes
    root_module = data.__class__.__module__.split(".")[0]
    if root_module == "polars":
        estimated_size = getattr(data, "estimated_size", None)
        if callable(estimated_size):
            value = estimated_size()
            return value if isinstance(value, int) and value >= 0 else None
    if root_module == "pandas":
        value = int(data.memory_usage(index=False, deep=True).sum())
        return value if value >= 0 else None
    return None


def _temp_budget(max_temp: int, spill: str) -> int:
    if spill == "auto":
        return max_temp
    if spill == "never":
        return 0
    raise ValueError("spill must be 'auto' or 'never'")


def _threads(threads: int | None) -> int | None:
    if threads is not None and threads < 1:
        raise ValueError("threads must be at least 1")
    return threads


def profile(
    data: Any,
    *,
    distinct: str = "none",
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    spill: str = "auto",
) -> dict[str, Any]:
    """Return a deterministic profile and BLAKE3 fingerprint for tabular data."""
    if distinct not in {"none", "exact"}:
        raise ValueError("distinct must be 'none' or 'exact'")
    if distinct == "exact":
        warnings.warn(
            "exact distinct can spill to disk; configure max_memory and max_temp for the workload",
            RuntimeWarning,
            stacklevel=2,
        )
    temp_budget = _temp_budget(max_temp, spill)
    return profile_arrow(
        _as_reader(data), distinct, _row_count_hint(data), max_memory, temp_budget
    )


def suggest_contract(
    data: Any,
    *,
    infer_uniqueness: bool = False,
    infer_categories: bool = False,
    max_categories: int = 20,
    infer_required: bool = False,
    infer_ranges: bool = True,
    range_tolerance: float = 0.0,
    infer_row_count: bool = True,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    spill: str = "auto",
) -> dict[str, Any]:
    """Return a review-required V2 contract draft inferred from one Arrow scan."""
    if max_categories <= 0:
        raise ValueError("max_categories must be positive")
    if not isinstance(range_tolerance, (int, float)) or range_tolerance < 0:
        raise ValueError("range_tolerance must be a finite non-negative number")
    return suggest_arrow(
        _as_reader(data),
        infer_uniqueness,
        infer_categories,
        max_categories,
        infer_required,
        infer_ranges,
        float(range_tolerance),
        infer_row_count,
        _row_count_hint(data),
        max_memory,
        _temp_budget(max_temp, spill),
    )


def fingerprint(data: Any, *, version: str = "v1") -> str:
    """Return only the canonical dataset fingerprint without profile or distinct state."""
    if version not in {"v1", "v2"}:
        raise ValueError("version must be 'v1' or 'v2'")
    return fingerprint_arrow(_as_reader(data), version)


def check(
    data: Any,
    contract: Mapping[str, Any],
    *,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    max_output_records: int = 100_000,
    max_samples: int = 100,
    spill: str = "auto",
    threads: int | None = None,
) -> dict[str, Any]:
    """Compile a strict contract and execute bounded, typed validation kernels."""
    normalized = dict(contract)
    normalized.setdefault("version", "proofframe.contract.v1")
    row_count_hint = _row_count_hint(data)
    temp_budget = _temp_budget(max_temp, spill)
    return check_arrow(
        _as_reader(data),
        json.dumps(normalized, sort_keys=True, separators=(",", ":")),
        row_count_hint,
        max_memory,
        temp_budget,
        max_output_records,
        max_samples,
        _threads(threads),
    )


def check_partitions(
    partitions: Sequence[Any],
    contract: Mapping[str, Any],
    *,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    max_output_records: int = 100_000,
    max_samples: int = 100,
    spill: str = "auto",
    threads: int | None = None,
) -> dict[str, Any]:
    """Validate ordered Arrow partitions in the shared native Rust engine."""
    readers = [_as_reader(partition) for partition in partitions]
    if not readers:
        raise ValueError("partitions must contain at least one dataset")
    normalized = dict(contract)
    normalized.setdefault("version", "proofframe.contract.v1")
    return check_partitions_arrow(
        readers,
        json.dumps(normalized, sort_keys=True, separators=(",", ":")),
        max_memory,
        _temp_budget(max_temp, spill),
        max_output_records,
        max_samples,
        _threads(threads),
    )


def check_partitions_with_evidence(
    partitions: Sequence[Any],
    contract: Mapping[str, Any],
    *,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    max_output_records: int = 100_000,
    max_samples: int = 100,
    spill: str = "auto",
    threads: int | None = None,
) -> dict[str, Any]:
    """Validate partitions and bind their ordered V2 fingerprints into a manifest."""
    readers = [_as_reader(partition) for partition in partitions]
    if not readers:
        raise ValueError("partitions must contain at least one dataset")
    normalized = dict(contract)
    normalized.setdefault("version", "proofframe.contract.v1")
    return check_partitions_with_evidence_arrow(
        readers,
        json.dumps(normalized, sort_keys=True, separators=(",", ":")),
        max_memory,
        _temp_budget(max_temp, spill),
        max_output_records,
        max_samples,
        _threads(threads),
    )


def check_with_evidence(
    data: Any,
    contract: Mapping[str, Any],
    *,
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    max_output_records: int = 100_000,
    max_samples: int = 100,
    spill: str = "auto",
    threads: int | None = None,
) -> dict[str, Any]:
    """Validate and fingerprint one Arrow stream in a single native execution."""
    normalized = dict(contract)
    normalized.setdefault("version", "proofframe.contract.v1")
    row_count_hint = _row_count_hint(data)
    temp_budget = _temp_budget(max_temp, spill)
    return check_with_evidence_arrow(
        _as_reader(data),
        json.dumps(normalized, sort_keys=True, separators=(",", ":")),
        row_count_hint,
        max_memory,
        temp_budget,
        max_output_records,
        max_samples,
        _threads(threads),
    )


def validate(
    data: Any,
    contract: Mapping[str, Any],
    *,
    include_profile: bool = True,
    **options: Any,
) -> dict[str, Any]:
    """Compatibility alias for :func:`check` retained for the 0.5 release line."""
    profile_note = (
        " The requested compatibility profile is no longer materialized."
        if include_profile
        else " Profile materialization remains disabled."
    )
    warnings.warn(
        "validate() is retained for 0.5 compatibility; use check(). "
        "Validation no longer builds an implicit unbounded profile." + profile_note,
        DeprecationWarning,
        stacklevel=2,
    )
    return check(data, contract, **options)


def diff(
    before: Any,
    after: Any,
    *,
    keys: str | Sequence[str],
    max_memory: int = 512 * 1024 * 1024,
    max_temp: int = 4 * 1024 * 1024 * 1024,
    max_output_records: int = 100_000,
    max_samples: int = 100,
    output: str | None = None,
    output_format: str = "jsonl",
    spill: str = "auto",
) -> dict[str, Any]:
    """Return added, removed, and column-level changed rows by stable key."""
    _temp_budget(max_temp, spill)
    key_list = [keys] if isinstance(keys, str) else list(keys)
    before_rows = _row_count_hint(before)
    after_rows = _row_count_hint(after)
    before_bytes = _byte_size_hint(before)
    after_bytes = _byte_size_hint(after)
    input_bytes = (
        before_bytes + after_bytes
        if before_bytes is not None and after_bytes is not None
        else None
    )
    return diff_arrow(
        _as_reader(before),
        _as_reader(after),
        key_list,
        max_memory,
        max_temp,
        max_output_records,
        max_samples,
        output,
        output_format,
        before_rows,
        after_rows,
        input_bytes,
        spill,
    )


def scan_pii(
    data: Any,
    *,
    max_findings: int = 100,
    fingerprint_mode: str = "unlinkable",
    fingerprint_key: str | None = None,
    key_id: str | None = None,
) -> dict[str, Any]:
    """Detect PII and emit keyed, non-reversible value fingerprints."""
    if max_findings < 0:
        raise ValueError("max_findings must be non-negative")
    if fingerprint_mode not in {"stable", "unlinkable"}:
        raise ValueError("fingerprint_mode must be 'stable' or 'unlinkable'")
    return scan_pii_arrow(
        _as_reader(data),
        max_findings,
        fingerprint_mode,
        fingerprint_key,
        key_id,
    )


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


def assemble_evidence_unchecked(
    data: Any,
    contract: Mapping[str, Any],
    report: Mapping[str, Any],
) -> dict[str, Any]:
    """Assemble evidence from a caller-supplied report without proving execution identity.

    Prefer :func:`check_with_evidence`. This escape hatch verifies available digests but cannot
    prove that the mutable report and dataset came from the same validation execution.
    """
    normalized = dict(contract)
    normalized.setdefault("version", "proofframe.contract.v1")
    return assemble_evidence_unchecked_arrow(
        _as_reader(data),
        json.dumps(normalized, sort_keys=True, separators=(",", ":")),
        json.dumps(report, sort_keys=True, separators=(",", ":")),
    )


def sign_evidence(evidence: Mapping[str, Any], *, private_key: str) -> dict[str, Any]:
    """Create an Ed25519-signed V2 receipt over a strict evidence envelope."""
    payload = json.dumps(evidence, sort_keys=True, separators=(",", ":"))
    return sign_evidence_receipt(payload, private_key)


def sign_receipt(
    evidence: Mapping[str, Any],
    *,
    private_key: str,
    receipt_version: str = "v2",
) -> dict[str, Any]:
    """Sign V2 evidence by default; pass ``receipt_version='v1'`` only for migration."""
    if receipt_version == "v2":
        return sign_evidence(evidence, private_key=private_key)
    if receipt_version == "v1":
        payload = json.dumps(evidence, sort_keys=True, separators=(",", ":"))
        return sign_proof_receipt(payload, private_key)
    raise ValueError("receipt_version must be 'v1' or 'v2'")


def verify_receipt(
    receipt: Mapping[str, Any],
    *,
    expected_public_key: str | None = None,
) -> dict[str, bool]:
    """Verify receipt integrity and, when supplied, the expected signer identity."""
    payload = json.dumps(receipt, sort_keys=True, separators=(",", ":"))
    return verify_proof_receipt_any(payload, expected_public_key)
