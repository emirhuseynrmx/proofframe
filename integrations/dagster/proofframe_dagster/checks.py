"""ProofFrame acceptance as a Dagster asset check.

Acceptance answers with three statuses and Dagster's check result carries two
fields, so the mapping is deliberate rather than obvious:

    accepted   passed, no severity
    rejected   not passed, ERROR    the data was scanned and did not qualify
    unknown    not passed, WARN     the scan did not complete, so nothing was
                                    decided; an operator has to look

Collapsing `unknown` into a failure would report a decision that was never made,
and collapsing it into a pass would report one that was made the other way.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path
from typing import Any

import proofframe as pf
from dagster import (
    AssetCheckResult,
    AssetCheckSeverity,
    AssetKey,
    MetadataValue,
    asset_check,
)

UNKNOWN_SEVERITIES = {
    "warn": AssetCheckSeverity.WARN,
    "error": AssetCheckSeverity.ERROR,
}


def _contract(contract: Mapping[str, Any] | str | Path) -> dict:
    """Accepts an inline contract or a path to one."""
    if isinstance(contract, (str, Path)):
        return json.loads(Path(contract).read_text(encoding="utf-8"))
    return dict(contract)


def acceptance_result(
    bundle: dict,
    *,
    unknown_severity: AssetCheckSeverity = AssetCheckSeverity.WARN,
    bundle_path: str | Path | None = None,
) -> AssetCheckResult:
    """Turns an acceptance bundle into the check result that describes it."""
    payload = bundle["payload"]
    decision = payload["decision"]
    report = payload.get("report") or {}
    status = decision["status"]

    metadata: dict[str, Any] = {
        "proofframe/status": status,
        "proofframe/contract_id": payload["contract_id"],
        "proofframe/policy_id": payload["policy_id"],
        "proofframe/bundle_sha256": bundle["sha256"],
        "proofframe/signed": bundle["signature"] is not None,
    }
    if report:
        metadata["proofframe/rows"] = report["rows"]
        metadata["proofframe/violations"] = report["violation_count"]
        metadata["proofframe/schema_digest"] = report["schema_digest"]
    if decision["reasons"]:
        metadata["proofframe/reasons"] = MetadataValue.json(decision["reasons"])
    if bundle_path is not None:
        metadata["proofframe/bundle"] = MetadataValue.path(str(bundle_path))

    if status == "accepted":
        return AssetCheckResult(passed=True, metadata=metadata)
    severity = AssetCheckSeverity.ERROR if status == "rejected" else unknown_severity
    reasons = ", ".join(decision["reasons"]) or "no reason recorded"
    return AssetCheckResult(
        passed=False,
        severity=severity,
        metadata=metadata,
        description=f"{status}: {reasons}",
    )


def build_acceptance_check(
    *,
    asset: AssetKey | str,
    path: str | Path,
    contract: Mapping[str, Any] | str | Path,
    name: str = "proofframe_acceptance",
    policy: Mapping[str, Any] | None = None,
    csv_options: Mapping[str, Any] | None = None,
    output: str | Path | None = None,
    private_key: str | None = None,
    unknown_severity: str = "warn",
    blocking: bool = True,
):
    """Defines a check that scans one file against one contract.

    :param asset: the asset whose materialisation produced ``path``
    :param path: the CSV or Parquet file to scan
    :param contract: the contract itself, or a path to a JSON file holding one
    :param output: where to write the acceptance bundle; nothing is written when omitted
    :param private_key: signs the bundle when given, leaving it hash-bound when not
    :param unknown_severity: ``warn`` or ``error`` for a scan that could not complete
    :param blocking: whether a failed check stops downstream assets
    """
    if unknown_severity not in UNKNOWN_SEVERITIES:
        raise ValueError(f"unknown_severity must be one of {sorted(UNKNOWN_SEVERITIES)}")
    severity = UNKNOWN_SEVERITIES[unknown_severity]
    resolved = _contract(contract)

    @asset_check(asset=asset, name=name, blocking=blocking)
    def _check() -> AssetCheckResult:
        bundle = pf.accept_file(
            path,
            dict(resolved),
            policy=dict(policy) if policy is not None else None,
            csv_options=dict(csv_options) if csv_options is not None else None,
            output=output,
            private_key=private_key,
        )
        return acceptance_result(bundle, unknown_severity=severity, bundle_path=output)

    return _check
