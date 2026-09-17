"""What each acceptance status does to a task, and what reaches XCom."""

from __future__ import annotations

import inspect
import json

import pytest

pytest.importorskip("airflow")

from airflow.exceptions import AirflowFailException, AirflowSkipException
from airflow.models import BaseOperator
from proofframe_airflow import ProofFrameAcceptOperator, ProofFrameVerifyOperator
from proofframe_airflow.operators import _summary


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


def patched(monkeypatch, result: dict) -> None:
    monkeypatch.setattr("proofframe_airflow.operators.pf.accept_file", lambda *a, **k: result)


def operator(**kwargs) -> ProofFrameAcceptOperator:
    return ProofFrameAcceptOperator(
        task_id="check", path="orders.parquet", contract={"columns": {}}, **kwargs
    )


def test_no_constructor_argument_shadows_a_base_operator_property():
    """An operator that stores an argument over a BaseOperator property cannot be built.

    `output` was one: BaseOperator exposes it as a read-only XComArg for the task's
    return value, so `self.output = output` raised on construction and every use of
    the operator failed at line one. It is the kind of collision that is invisible
    until Airflow is actually installed, which is why this asserts the rule rather
    than the one name that broke.
    """
    reserved = {
        name
        for name in dir(BaseOperator)
        if isinstance(getattr(BaseOperator, name, None), property)
    }
    for operator in (ProofFrameAcceptOperator, ProofFrameVerifyOperator):
        stored = set(inspect.signature(operator.__init__).parameters) - {"self", "kwargs"}
        assert not (stored & reserved), (operator.__name__, sorted(stored & reserved))


def test_accepted_returns_the_summary(monkeypatch):
    patched(monkeypatch, bundle("accepted"))
    summary = operator().execute({})
    assert summary["status"] == "accepted"
    assert summary["rows"] == 10


def test_rejected_fails_without_retrying(monkeypatch):
    # The same bytes under the same contract cannot decide differently, so this
    # raises the failure Airflow does not retry.
    patched(monkeypatch, bundle("rejected", ["violation_limit_exceeded"]))
    with pytest.raises(AirflowFailException, match="violation_limit_exceeded"):
        operator().execute({})


def test_unknown_fails_by_default(monkeypatch):
    patched(monkeypatch, bundle("unknown", ["OSError"], report=False))
    with pytest.raises(AirflowFailException, match="OSError"):
        operator().execute({})


def test_unknown_can_skip(monkeypatch):
    patched(monkeypatch, bundle("unknown", ["OSError"], report=False))
    with pytest.raises(AirflowSkipException):
        operator(on_unknown="skip").execute({})


def test_unknown_can_pass(monkeypatch):
    patched(monkeypatch, bundle("unknown", ["OSError"], report=False))
    assert operator(on_unknown="pass").execute({})["status"] == "unknown"


def test_unknown_action_is_validated():
    with pytest.raises(ValueError):
        operator(on_unknown="retry")


def test_summary_leaves_the_bundle_on_disk():
    # XCom lives in the metadata database; report and evidence do not go there.
    summary = _summary(bundle("accepted"), None)
    assert "report" not in summary and "evidence" not in summary
    assert summary["sha256"] == "d" * 64


def test_contract_can_be_a_path(monkeypatch, tmp_path):
    path = tmp_path / "contract.json"
    path.write_text(json.dumps({"columns": {"amount": {"min": 0}}}), encoding="utf-8")
    seen = {}

    def capture(_path, contract, **kwargs):
        seen["contract"] = contract
        return bundle("accepted")

    monkeypatch.setattr("proofframe_airflow.operators.pf.accept_file", capture)
    ProofFrameAcceptOperator(task_id="check", path="orders.parquet", contract=path).execute({})
    assert seen["contract"] == {"columns": {"amount": {"min": 0}}}


def test_verify_rejects_an_authentic_unknown(monkeypatch, tmp_path):
    # Verification proves the envelope, never the decision inside it.
    path = tmp_path / "bundle.json"
    path.write_text(json.dumps(bundle("unknown", ["OSError"], report=False)), encoding="utf-8")
    monkeypatch.setattr(
        "proofframe_airflow.operators.pf.verify_acceptance",
        lambda *a, **k: {"valid": True, "authenticated": False},
    )
    task = ProofFrameVerifyOperator(task_id="verify", bundle_path=path)
    with pytest.raises(AirflowFailException, match="unknown"):
        task.execute({})

    allowed = ProofFrameVerifyOperator(task_id="verify", bundle_path=path, require_accepted=False)
    assert allowed.execute({})["status"] == "unknown"
