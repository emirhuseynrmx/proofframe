"""Versioned file acceptance. Signatures attest to a publisher, not data truth."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any

import pyarrow as pa
from pyarrow import csv, parquet

from .api import check_with_evidence
from .errors import ContractError, ProofFrameError, ResourceLimitError

POLICY_VERSION = "proofframe.acceptance-policy.v1"
BUNDLE_VERSION = "proofframe.acceptance.v1"


def _canonical(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False
    ).encode("utf8")


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical(value)).hexdigest()


def _policy(value: dict | None) -> dict:
    result = {"version": POLICY_VERSION, "max_violations": 0, "minimum_evaluated": {}}
    result.update(value or {})
    if set(result) != {"version", "max_violations", "minimum_evaluated"}:
        raise ValueError("Unknown acceptance policy field")
    if result["version"] != POLICY_VERSION:
        raise ValueError("Unsupported policy version")
    if type(result["max_violations"]) is not int or result["max_violations"] < 0:
        raise ValueError("policy max_violations must be a non-negative integer")
    if not isinstance(result["minimum_evaluated"], dict) or any(
        not isinstance(k, str) or type(v) is not int or v < 0
        for k, v in result["minimum_evaluated"].items()
    ):
        raise ValueError("policy minimum_evaluated requires column names and non-negative integers")
    return json.loads(_canonical(result))


def _csv_options(value: dict | None) -> tuple[dict, dict]:
    settings = {
        "delimiter": ",",
        "encoding": "utf8",
        "decimal_point": ".",
        "column_types": {},
        "null_values": list(csv.ConvertOptions().null_values),
    }
    settings.update(value or {})
    if set(settings) != {"delimiter", "encoding", "decimal_point", "column_types", "null_values"}:
        raise ValueError("Unknown CSV setting")
    settings = json.loads(_canonical(settings))
    options = {
        "read_options": csv.ReadOptions(encoding=settings["encoding"], block_size=1 << 20),
        "parse_options": csv.ParseOptions(delimiter=settings["delimiter"]),
        "convert_options": csv.ConvertOptions(
            decimal_point=settings["decimal_point"],
            column_types=settings["column_types"],
            null_values=settings["null_values"],
            strings_can_be_null=True,
        ),
    }
    settings.update({"block_size": 1 << 20, "strings_can_be_null": True})
    return settings, options


def _decode(value: str) -> bytes:
    return base64.b64decode(value + "=" * (-len(value) % 4), altchars=b"-_", validate=True)


def _encode(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).decode().rstrip("=")


def _write_new(path: Path, bundle: dict) -> None:
    # Cooperative exclusive lock + same-filesystem publication; no power-loss guarantee.
    path.parent.mkdir(parents=True, exist_ok=True)
    lock = path.with_name("." + path.name + ".acceptance.lock")
    fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    temporary = None
    try:
        if os.path.lexists(path):
            raise FileExistsError(path)
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(_canonical(bundle))
            stream.flush()
            os.fsync(stream.fileno())
        if os.path.lexists(path):
            raise FileExistsError(path)
        temporary.rename(path)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
        lock.unlink()


def accept_file(
    path: str | Path,
    contract: dict,
    *,
    policy: dict | None = None,
    csv_options: dict | None = None,
    output: str | Path | None = None,
    private_key: str | None = None,
    max_output_bytes: int = 16 << 20,
    **scan_options: Any,
) -> dict:
    """Scan CSV/Parquet once and bind evidence, policy and parsing settings.

    Missing files, parsing failures and resource exhaustion produce ``unknown``;
    invalid policy/contract/options raise. Counts describe column evaluation, not
    execution of every rule. No automatic repair or upload is performed.
    """
    if type(max_output_bytes) is not int or max_output_bytes <= 0:
        raise ValueError("max_output_bytes must be a positive integer")
    policy = _policy(policy)
    contract = json.loads(_canonical(contract))
    contract.setdefault("version", "proofframe.contract.v1")
    source = Path(path)
    if source.suffix.lower() not in {".csv", ".parquet"}:
        raise ValueError("Use .csv or .parquet")
    if source.suffix.lower() == ".parquet" and csv_options is not None:
        raise ValueError("CSV options cannot apply to Parquet")
    settings, options = _csv_options(csv_options)
    read_settings = {
        "format": source.suffix.lower()[1:],
        "csv": settings if source.suffix.lower() == ".csv" else None,
    }
    if output is not None and os.path.lexists(output):
        raise FileExistsError(output)
    scan_options.setdefault("max_samples", 0)
    checked = None
    decision = {"status": "unknown", "reasons": [], "evaluated": {}}
    reader = None
    pf = None
    try:
        if source.suffix.lower() == ".csv":
            reader = csv.open_csv(source, **options)
        else:
            pf = parquet.ParquetFile(source)
            reader = pa.RecordBatchReader.from_batches(pf.schema_arrow, pf.iter_batches())
        names = reader.schema.names
        if len(names) != len(set(names)):
            raise ValueError("Duplicate column names are ambiguous")
        checked = check_with_evidence(reader, contract, **scan_options)
        report = checked["report"]
        evaluated = {
            names[i]: n
            for i, n in zip(
                report.get("evaluated_indices", []), report.get("evaluated_columns", [])
            )
        }
        reasons = []
        if report["violation_count"] > policy["max_violations"]:
            reasons.append("violation_limit_exceeded")
        for name, minimum in policy["minimum_evaluated"].items():
            if name not in evaluated or evaluated[name] < minimum:
                reasons.append("minimum_evaluated:" + name)
        decision = {
            "status": "rejected" if reasons else "accepted",
            "reasons": reasons,
            "evaluated": evaluated,
        }
    except ContractError:
        raise
    except (OSError, pa.ArrowException, ProofFrameError) as error:
        decision["reasons"] = [type(error).__name__]
    finally:
        if reader is not None:
            reader.close()
        if pf is not None:
            pf.close()
    payload = {
        "version": BUNDLE_VERSION,
        "contract": contract,
        "contract_id": _digest(contract),
        "policy": policy,
        "policy_id": _digest(policy),
        "read_settings": read_settings,
        "read_settings_id": _digest(read_settings),
        "decision": decision,
        "report": checked["report"] if checked else None,
        "evidence": checked["evidence"] if checked else None,
    }
    bundle = {"payload": payload, "sha256": _digest(payload), "signature": None}
    if private_key is not None:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        key = Ed25519PrivateKey.from_private_bytes(_decode(private_key))
        bundle["signature"] = {
            "algorithm": "Ed25519",
            "public_key": _encode(key.public_key().public_bytes_raw()),
            "value": _encode(key.sign(_canonical(payload))),
        }
    if len(_canonical(bundle)) > max_output_bytes:
        raise ResourceLimitError("Acceptance output exceeds max_output_bytes")
    if output is not None:
        _write_new(Path(output), bundle)
    return bundle


#: Every field an acceptance payload must carry. `report` and `evidence` may be null
#: for an `unknown` decision, but the keys themselves are never optional.
PAYLOAD_FIELDS = frozenset(
    {
        "version",
        "contract",
        "contract_id",
        "policy",
        "policy_id",
        "read_settings",
        "read_settings_id",
        "decision",
        "report",
        "evidence",
    }
)

DECISION_STATUSES = frozenset({"accepted", "rejected", "unknown"})

#: Fields the native report carries, and the type each must have. A completed scan
#: without these is not a scan, whatever its digest says.
REPORT_FIELDS: dict[str, type | tuple[type, ...]] = {
    "valid": bool,
    "mode": str,
    "rows": int,
    "violation_count": int,
    "truncated": bool,
    "findings": list,
    "metrics": dict,
    "resources": dict,
    "evaluated_columns": list,
    "evaluated_indices": list,
    "schema_digest": str,
    "compiled_plan_digest": str,
    "contract_source_digest": str,
}

#: The Evidence V2 envelope this release binds into an acceptance bundle.
EVIDENCE_SCHEMA = "proofframe.evidence.v2"
EVIDENCE_FIELDS: dict[str, type | tuple[type, ...]] = {
    "schema": str,
    "dataset": dict,
    "engine": dict,
    "execution": dict,
    "result": dict,
    "schema_digest": str,
    "compiled_plan_digest": str,
    "contract_source_digest": str,
}


def _typed(value: object, fields: dict[str, type | tuple[type, ...]]) -> bool:
    """Whether every named field is present with the type the producer writes.

    ``bool`` is checked before ``int`` because Python's ``bool`` is an ``int``: a count
    of ``True`` would otherwise pass as a row count.
    """
    if not isinstance(value, dict) or not fields.keys() <= value.keys():
        return False
    for name, expected in fields.items():
        found = value[name]
        if expected is int and isinstance(found, bool):
            return False
        if not isinstance(found, expected):
            return False
    return True


def _counts_are_sane(report: dict) -> bool:
    """Rejects counters that are the wrong sign or disagree with what they count."""
    if report["rows"] < 0 or report["violation_count"] < 0:
        return False
    if len(report["evaluated_columns"]) != len(report["evaluated_indices"]):
        return False
    return all(
        isinstance(count, int) and not isinstance(count, bool) and count >= 0
        for count in report["evaluated_columns"]
    ) and all(
        isinstance(index, int) and not isinstance(index, bool) and index >= 0
        for index in report["evaluated_indices"]
    )


def _well_formed(payload: object) -> bool:
    """Whether this is an acceptance payload at all, before any digest is trusted.

    A digest proves that what is present has not been edited. It says nothing about
    what is absent, so a payload with its decision or its evidence removed and its
    hash recomputed would otherwise verify. Shape is checked first, and exactly: an
    unrecognized field is as disqualifying as a missing one, because a reader cannot
    know what an unrecognized field was meant to change.
    """
    if not isinstance(payload, dict) or set(payload) != PAYLOAD_FIELDS:
        return False
    decision = payload["decision"]
    if not isinstance(decision, dict) or set(decision) != {"status", "reasons", "evaluated"}:
        return False
    if decision["status"] not in DECISION_STATUSES:
        return False
    if not isinstance(decision["reasons"], list) or not all(
        isinstance(reason, str) for reason in decision["reasons"]
    ):
        return False
    if not isinstance(decision["evaluated"], dict):
        return False
    # Only an incomplete scan may lack a report, and only by being absent outright.
    # Anything else in that slot has to be the scan the producer writes: a present
    # field of the wrong shape is a stronger claim than a missing one, not a weaker
    # one, because a reader will go on to use it.
    if decision["status"] == "unknown":
        if not (payload["report"] is None and payload["evidence"] is None):
            return False
    else:
        if not _typed(payload["report"], REPORT_FIELDS):
            return False
        if not _counts_are_sane(payload["report"]):
            return False
        if not _typed(payload["evidence"], EVIDENCE_FIELDS):
            return False
        if payload["evidence"]["schema"] != EVIDENCE_SCHEMA:
            return False
    return all(
        isinstance(payload[field], dict) for field in ("contract", "policy", "read_settings")
    )


def verify_acceptance(bundle: dict, *, expected_public_key: str | None = None) -> dict:
    """Offline integrity/signature check; does not rescan input or prove publisher honesty.

    Pin expected_public_key to require authentication. An unsigned hash can be
    rewritten by anyone and provides no authenticity.
    """
    try:
        payload = bundle["payload"]
        valid = (
            _well_formed(payload)
            and payload["version"] == BUNDLE_VERSION
            and bundle["sha256"] == _digest(payload)
            and payload["contract_id"] == _digest(payload["contract"])
            and payload["policy_id"] == _digest(payload["policy"])
            and payload["read_settings_id"] == _digest(payload["read_settings"])
        )
        signature = bundle["signature"]
        authenticated = False
        if signature is not None:
            from cryptography.exceptions import InvalidSignature
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

            if signature["algorithm"] != "Ed25519":
                return {"valid": False, "authenticated": False}
            try:
                Ed25519PublicKey.from_public_bytes(_decode(signature["public_key"])).verify(
                    _decode(signature["value"]), _canonical(payload)
                )
            except InvalidSignature:
                return {"valid": False, "authenticated": False}
            authenticated = expected_public_key is not None and _decode(
                expected_public_key
            ) == _decode(signature["public_key"])
        if expected_public_key is not None and not authenticated:
            valid = False
        return {"valid": valid, "authenticated": bool(valid and authenticated)}
    except (KeyError, ValueError, TypeError):
        return {"valid": False, "authenticated": False}
