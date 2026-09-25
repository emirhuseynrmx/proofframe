"""Airflow operators that run a ProofFrame acceptance scan in the worker process.

The scan runs through the Python bindings rather than a subprocess, so the task
keeps the exception types the library raises and the worker's resource limits
apply to the scan itself.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path
from typing import Any

import proofframe as pf
from airflow.exceptions import AirflowFailException, AirflowSkipException
from airflow.models import BaseOperator

#: What a task does with each acceptance status. A rejected contract is a
#: deterministic result, so it raises the non-retryable failure: repeating the
#: same scan over the same bytes cannot reach a different decision.
UNKNOWN_ACTIONS = frozenset({"fail", "skip", "pass"})


def _contract(contract: Mapping[str, Any] | str | Path) -> dict:
    """Accepts an inline contract or a path to one."""
    if isinstance(contract, (str, Path)):
        return json.loads(Path(contract).read_text(encoding="utf-8"))
    return dict(contract)


def _summary(bundle: dict, evidence_path: Path | None) -> dict:
    """The part of a bundle small enough to travel through XCom.

    Airflow stores XCom in its metadata database, and a bundle carries the full
    report and Evidence V2 envelope. Only the decision and the digests that
    identify what was decided go through; the bundle itself stays on disk.
    """
    payload = bundle["payload"]
    decision = payload["decision"]
    report = payload.get("report") or {}
    return {
        "status": decision["status"],
        "reasons": decision["reasons"],
        "sha256": bundle["sha256"],
        "signed": bundle["signature"] is not None,
        "contract_id": payload["contract_id"],
        "policy_id": payload["policy_id"],
        "schema_digest": report.get("schema_digest"),
        "violation_count": report.get("violation_count"),
        "rows": report.get("rows"),
        "bundle_path": str(evidence_path) if evidence_path is not None else None,
    }


class ProofFrameAcceptOperator(BaseOperator):
    """Scans a file against a contract and ends the task on the decision.

    :param path: the CSV or Parquet file to scan, on a filesystem the worker can read
    :param contract: the contract itself, or a path to a JSON file holding one
    :param policy: acceptance policy; the library's default is used when omitted
    :param output_path: where to write the acceptance bundle; nothing is written when omitted
    :param private_key: signs the bundle when given, leaving it hash-bound when not
    :param on_unknown: ``fail``, ``skip`` or ``pass`` for a scan that could not complete
    """

    template_fields = ("path", "output_path")
    ui_color = "#d8e6f3"

    def __init__(
        self,
        *,
        path: str | Path,
        contract: Mapping[str, Any] | str | Path,
        policy: Mapping[str, Any] | None = None,
        csv_options: Mapping[str, Any] | None = None,
        output_path: str | Path | None = None,
        private_key: str | None = None,
        on_unknown: str = "fail",
        **kwargs: Any,
    ) -> None:
        super().__init__(**kwargs)
        if on_unknown not in UNKNOWN_ACTIONS:
            raise ValueError(f"on_unknown must be one of {sorted(UNKNOWN_ACTIONS)}")
        self.path = path
        self.contract = contract
        self.policy = policy
        self.csv_options = csv_options
        self.output_path = output_path
        self.private_key = private_key
        self.on_unknown = on_unknown

    def execute(self, context: Any) -> dict:
        bundle = pf.accept_file(
            self.path,
            _contract(self.contract),
            policy=dict(self.policy) if self.policy is not None else None,
            csv_options=dict(self.csv_options) if self.csv_options is not None else None,
            output=self.output_path,
            private_key=self.private_key,
        )
        summary = _summary(bundle, Path(self.output_path) if self.output_path is not None else None)
        status = summary["status"]
        self.log.info("ProofFrame decision for %s: %s", self.path, status)

        if status == "rejected":
            raise AirflowFailException(
                f"{self.path} was rejected: {', '.join(summary['reasons']) or 'no reason recorded'}"
            )
        if status == "unknown":
            # The scan did not complete, so nothing was decided either way. An
            # unknown is the one status a retry can legitimately change.
            reason = ", ".join(summary["reasons"]) or "no reason recorded"
            if self.on_unknown == "fail":
                raise AirflowFailException(f"{self.path} could not be scanned: {reason}")
            if self.on_unknown == "skip":
                raise AirflowSkipException(f"{self.path} could not be scanned: {reason}")
            self.log.warning("continuing past an unknown decision: %s", reason)
        return summary


class ProofFrameVerifyOperator(BaseOperator):
    """Verifies an acceptance bundle another task produced.

    Verification rescans nothing. It checks that the bundle's bytes still bind to
    its digests and that the pinned key signed it. Without ``expected_public_key``
    the task fails, because an unsigned bundle can be edited and re-hashed by anyone;
    ``integrity_only=True`` accepts an intact bundle whose signer is not checked.
    """

    template_fields = ("bundle_path",)
    ui_color = "#d8e6f3"

    def __init__(
        self,
        *,
        bundle_path: str | Path,
        expected_public_key: str | None = None,
        require_accepted: bool = True,
        integrity_only: bool = False,
        **kwargs: Any,
    ) -> None:
        super().__init__(**kwargs)
        self.bundle_path = bundle_path
        self.expected_public_key = expected_public_key
        self.require_accepted = require_accepted
        self.integrity_only = integrity_only

    def execute(self, context: Any) -> dict:
        bundle = json.loads(Path(self.bundle_path).read_text(encoding="utf-8"))
        result = pf.verify_acceptance(bundle, expected_public_key=self.expected_public_key)
        verified = result["intact"] if self.integrity_only else result["valid"]
        if not verified:
            hint = (
                ""
                if self.expected_public_key or self.integrity_only
                else ("; no expected_public_key was given")
            )
            raise AirflowFailException(f"{self.bundle_path} did not verify: {result}{hint}")

        status = bundle["payload"]["decision"]["status"]
        if self.require_accepted and status != "accepted":
            # An authentic bundle is not an accepted one; an unknown that verifies
            # is still unknown.
            raise AirflowFailException(f"{self.bundle_path} verified but is {status}")
        return {
            "valid": result["valid"],
            "intact": result["intact"],
            "status": status,
            "authenticated": result["authenticated"],
        }
