"""The factory has to produce a check Dagster will actually load and run."""

from __future__ import annotations

import pytest

pytest.importorskip("dagster")

from dagster import (
    AssetCheckSeverity,
    Definitions,
    asset,
    materialize,
)
from proofframe_dagster import build_acceptance_check

CONTRACT = {"columns": {"amount": {"min": 0}}}


def orders_csv(path, rows: str):
    path.write_text("id,amount\n" + rows, encoding="utf-8")
    return path


def built(tmp_path, rows: str, **kwargs):
    data = orders_csv(tmp_path / "orders.csv", rows)

    @asset(name="orders")
    def orders() -> None:
        return None

    check = build_acceptance_check(asset=orders, path=data, contract=CONTRACT, **kwargs)
    return orders, check


def test_a_clean_file_passes(tmp_path):
    orders, check = built(tmp_path, "1,5\n2,7\n")
    result = materialize([orders, check])
    assert result.success
    evaluations = result.get_asset_check_evaluations()
    assert len(evaluations) == 1
    assert evaluations[0].passed
    assert evaluations[0].check_name == "proofframe_acceptance"


def test_a_violation_fails_the_check(tmp_path):
    orders, check = built(tmp_path, "1,5\n2,-3\n", blocking=False)
    result = materialize([orders, check], raise_on_error=False)
    evaluation = result.get_asset_check_evaluations()[0]
    assert not evaluation.passed
    assert evaluation.severity == AssetCheckSeverity.ERROR


def test_a_missing_file_is_unknown_not_rejected(tmp_path):
    # The file the asset was supposed to write is not there. Nothing about the
    # rows was decided, so this is unknown and warns rather than failing hard.
    @asset(name="orders")
    def orders() -> None:
        return None

    check = build_acceptance_check(
        asset=orders,
        path=tmp_path / "absent.csv",
        contract=CONTRACT,
        blocking=False,
    )
    result = materialize([orders, check], raise_on_error=False)
    evaluation = result.get_asset_check_evaluations()[0]
    assert not evaluation.passed
    assert evaluation.severity == AssetCheckSeverity.WARN
    assert evaluation.metadata["proofframe/status"].value == "unknown"


def test_the_check_loads_into_definitions(tmp_path):
    orders, check = built(tmp_path, "1,5\n")
    defs = Definitions(assets=[orders], asset_checks=[check])
    assert defs.get_asset_checks_def(next(iter(check.check_keys))) is check


def test_unknown_severity_is_validated(tmp_path):
    with pytest.raises(ValueError):
        built(tmp_path, "1,5\n", unknown_severity="ignore")


def test_the_bundle_is_written_when_asked(tmp_path):
    output = tmp_path / "evidence.json"
    orders, check = built(tmp_path, "1,5\n", output=output)
    result = materialize([orders, check])
    assert result.success
    assert output.exists()
    evaluation = result.get_asset_check_evaluations()[0]
    assert evaluation.metadata["proofframe/bundle"].path == str(output)
