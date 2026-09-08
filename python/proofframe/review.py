"""Publish a bounded local review bundle from one native evidence scan."""

from __future__ import annotations

import json
import os
import shutil
import tempfile
from collections.abc import Iterable, Mapping
from pathlib import Path
from typing import Any

from .api import check_with_evidence
from .errors import ResourceLimitError
from .review_render import document, markdown

__all__ = ["review"]


def _write_chunks(path: Path, chunks: Iterable[str], remaining: list[int]) -> None:
    with path.open("xb") as stream:
        for chunk in chunks:
            encoded = chunk.encode("utf-8")
            if len(encoded) > remaining[0]:
                raise ResourceLimitError("Review output exceeds max_output_bytes")
            stream.write(encoded)
            remaining[0] -= len(encoded)
        stream.flush()
        os.fsync(stream.fileno())


def review(
    data: Any,
    contract: Mapping[str, Any],
    output: str | Path,
    *,
    label: str = "Dataset",
    references: Mapping[str, Any] | None = None,
    max_samples: int = 0,
    max_memory: int = 512 << 20,
    max_temp: int = 4 << 30,
    max_output_records: int = 100_000,
    spill: str = "auto",
    threads: int | None = None,
    max_output_bytes: int = 16 << 20,
) -> dict[str, Any]:
    """Write HTML, Markdown, report JSON and Evidence V2 without rescanning data.

    Existing output is never replaced. A cooperative exclusive sidecar lock
    protects concurrent publishers. After process termination, an abandoned lock
    may require manual removal after confirming no writer remains. Publication is
    a same-filesystem rename; no new power-loss durability guarantee is implied.

    Samples default to zero in all artifacts. Contract literals and schema labels
    are not secrets-redacted. No data is uploaded and no browser is opened.
    """
    if type(max_output_bytes) is not int or max_output_bytes <= 0:
        raise ValueError("max_output_bytes must be a positive integer")
    if type(label) is not str or len(label) > 4096:
        raise ValueError("label must be a string of at most 4096 characters")
    # Freeze the JSON contract before a reader can execute user code while scanning.
    contract = json.loads(json.dumps(dict(contract)))
    target = Path(os.path.abspath(output))
    if not target.name:
        raise ValueError("output must name a new directory")
    if os.path.lexists(target):
        raise FileExistsError(f"Review output already exists: {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    lock = target.with_name(f".{target.name}.proofframe-review.lock")
    fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    staging: Path | None = None
    try:
        if os.path.lexists(target):
            raise FileExistsError(f"Review output already exists: {target}")
        staging = Path(tempfile.mkdtemp(prefix=f".{target.name}.review-", dir=target.parent))
        checked = check_with_evidence(
            data,
            contract,
            references=references,
            max_samples=max_samples,
            max_memory=max_memory,
            max_temp=max_temp,
            max_output_records=max_output_records,
            spill=spill,
            threads=threads,
        )
        report, evidence = checked["report"], checked["evidence"]
        # Read without consuming: Table and RecordBatchReader both expose a schema,
        # and anything else falls back to indices rather than dropping the finding.
        schema = getattr(data, "schema", None)
        column_names = tuple(schema.names) if schema is not None else ()
        remaining = [max_output_bytes]
        encoder = json.JSONEncoder(indent=2, sort_keys=True, ensure_ascii=True, allow_nan=False)
        _write_chunks(staging / "report.json", encoder.iterencode(report), remaining)
        _write_chunks(staging / "evidence.json", encoder.iterencode(evidence), remaining)
        _write_chunks(
            staging / "summary.md", markdown(report, evidence, label, column_names), remaining
        )
        _write_chunks(
            staging / "index.html",
            document(report, evidence, contract, label, column_names),
            remaining,
        )
        if os.path.lexists(target):
            raise FileExistsError(f"Review output already exists: {target}")
        staging.rename(target)
        staging = None
        return {
            "valid": report["valid"],
            "violation_count": report["violation_count"],
            "rows": report["rows"],
            "sampled_findings": len(report["findings"]),
            "output": str(target),
            "output_bytes": max_output_bytes - remaining[0],
        }
    finally:
        if staging is not None:
            shutil.rmtree(staging)
        lock.unlink()
