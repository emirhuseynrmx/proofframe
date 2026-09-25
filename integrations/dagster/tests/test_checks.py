"""The three-way decision has to survive the two-field check result."""

from __future__ import annotations

import pytest

pytest.importorskip("dagster")

from dagster import AssetCheckSeverity
from proofframe_dagster import acceptance_result


def meta(result, key):
    """Dagster wraps every metadata entry in a MetadataValue; unwrap it."""
    return result.metadata[key].value


def bundle(status: str, reasons: list[str] | None = None, *, report: bool = True) -> dict:
    """An acceptance bundle shaped like the one `accept_file` returns."""
    payload = {
        "version": "proofframe.acceptance.v1",
        "contract": {},
        "contract_id": "c" * 64,
        "policy": {},
        "policy_id": "p" * 64,
        "read_settings": {},
        "read_settings_id": "r" * 64,
        "decision": {"status": status, "reasons": reasons or [], "evaluated": {}},
        "report": {
            "rows": 10,
            "violation_count": 0 if status == "accepted" else 3,
            "schema_digest": "s" * 64,
        }
        if report
        else None,
        "evidence": None,
    }
    return {"payload": payload, "sha256": "d" * 64, "signature": None}


def test_accepted_passes():
    result = acceptance_result(bundle("accepted"))
    assert result.passed
    assert meta(result, "proofframe/status") == "accepted"
    assert meta(result, "proofframe/violations") == 0


def test_rejected_is_an_error():
    result = acceptance_result(bundle("rejected", ["violation_limit_exceeded"]))
    assert not result.passed
    assert result.severity == AssetCheckSeverity.ERROR
    assert "violation_limit_exceeded" in result.description


def test_unknown_stops_downstream_by_default():
    # A scan that did not complete decided nothing, and data nobody accepted must
    # not flow on: Dagster blocks downstream assets only on ERROR. The description
    # still says `unknown`, so it is not read as a rejection.
    result = acceptance_result(bundle("unknown", ["OSError"], report=False))
    assert not result.passed
    assert result.severity == AssetCheckSeverity.ERROR
    assert result.description.startswith("unknown:")
    assert "proofframe/rows" not in result.metadata


def test_unknown_can_be_relaxed_explicitly():
    result = acceptance_result(
        bundle("unknown", ["OSError"], report=False),
        unknown_severity=AssetCheckSeverity.WARN,
    )
    assert result.severity == AssetCheckSeverity.WARN


def test_signature_presence_is_reported_not_assumed():
    # Signing is optional, so the metadata says which of the two a bundle is.
    unsigned = acceptance_result(bundle("accepted"))
    assert meta(unsigned, "proofframe/signed") is False

    signed = bundle("accepted")
    signed["signature"] = {"algorithm": "Ed25519", "public_key": "k", "value": "v"}
    assert meta(acceptance_result(signed), "proofframe/signed") is True
