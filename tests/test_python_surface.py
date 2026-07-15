import json
import sys

import pandas as pd
import pyarrow as pa
import pyarrow.csv as arrow_csv
import pyarrow.parquet as parquet
import pytest

import proofframe
from proofframe import api, cli


def table() -> pa.Table:
    return pa.table({"id": [1, 2], "score": [0.2, 0.8]})


class ArrowConvertible:
    def to_arrow(self):
        return table()


class ArrowStreamProvider:
    def __arrow_c_stream__(self, requested_schema=None):
        return table().__arrow_c_stream__(requested_schema)


def test_all_supported_python_inputs_reach_native_engine():
    source = table()
    batch = source.to_batches()[0]
    reader = source.to_reader()
    dataframe = pd.DataFrame({"id": [1, 2], "score": [0.2, 0.8]})

    assert proofframe.profile(reader)["rows"] == 2
    assert proofframe.profile(batch)["rows"] == 2
    assert proofframe.profile(ArrowConvertible())["rows"] == 2
    assert proofframe.profile(dataframe)["rows"] == 2
    assert proofframe.profile(ArrowStreamProvider())["rows"] == 2

    with pytest.raises(TypeError, match="Expected a PyArrow"):
        proofframe.profile(object())


def test_argument_validation_and_sequence_branches():
    source = table()
    with pytest.raises(ValueError, match="max_findings"):
        proofframe.scan_pii(source, max_findings=-1)
    with pytest.raises(ValueError, match="max_samples"):
        proofframe.detect_leakage(source, source, max_samples=-1)

    full_row = proofframe.detect_leakage(source, source, keys=None, max_samples=1)
    assert full_row["mode"] == "full_row"
    assert full_row["truncated"] is True
    keyed = proofframe.detect_leakage(source, source, keys=["id"])
    assert keyed["keys"] == ["id"]
    assert proofframe.diff(source, source, keys=["id"])["changed_count"] == 0


def run_cli(monkeypatch, capsys, *arguments: str) -> dict:
    monkeypatch.setattr(sys, "argv", ["proofframe", *arguments])
    cli.main()
    return json.loads(capsys.readouterr().out)


def test_cli_profile_validate_and_diff_paths(tmp_path, monkeypatch, capsys):
    before = tmp_path / "before.csv"
    after = tmp_path / "after.parquet"
    contract_path = tmp_path / "contract.json"
    arrow_csv.write_csv(table(), before)
    parquet.write_table(pa.table({"id": [1, 3], "score": [0.2, 0.7]}), after)
    contract_path.write_text(
        json.dumps({"columns": {"id": {"required": True, "unique": True}}}),
        encoding="utf-8",
    )

    assert run_cli(monkeypatch, capsys, "profile", str(before))["rows"] == 2
    validated = run_cli(
        monkeypatch,
        capsys,
        "validate",
        str(after),
        "--contract",
        str(contract_path),
    )
    assert validated["valid"] is True
    changed = run_cli(
        monkeypatch,
        capsys,
        "diff",
        str(before),
        str(after),
        "--key",
        "id",
    )
    assert changed["added_count"] == 1
    assert changed["removed_count"] == 1


def test_cli_validate_uses_nonzero_exit_for_contract_violations(tmp_path, monkeypatch, capsys):
    source = tmp_path / "invalid.parquet"
    contract_path = tmp_path / "contract.json"
    parquet.write_table(pa.table({"id": pa.array([None], type=pa.int64())}), source)
    contract_path.write_text(
        json.dumps({"columns": {"id": {"not_null": True}}, "max_findings": 0}),
        encoding="utf-8",
    )

    monkeypatch.setattr(
        sys,
        "argv",
        ["proofframe", "validate", str(source), "--contract", str(contract_path)],
    )
    with pytest.raises(SystemExit) as exit_info:
        cli.main()

    report = json.loads(capsys.readouterr().out)
    assert exit_info.value.code == 1
    assert report["valid"] is False
    assert report["violation_count"] == 1
    assert report["truncated"] is True


def test_cli_rejects_unknown_file_type():
    with pytest.raises(ValueError, match="Unsupported input"):
        cli._read("dataset.json")


def test_private_reader_helper_returns_reader_unchanged():
    reader = table().to_reader()
    assert api._as_reader(reader) is reader
