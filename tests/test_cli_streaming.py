import json

import pyarrow as pa
import pyarrow.csv as arrow_csv
import pytest
from proofframe import cli, generate_keypair
from pyarrow import parquet


def _contract(path, rules: dict) -> None:
    path.write_text(json.dumps({"columns": rules}), encoding="utf-8")


def test_check_and_fingerprint_never_select_materializing_file_apis(
    tmp_path, monkeypatch, capsys
):
    csv_path = tmp_path / "input.csv"
    parquet_path = tmp_path / "input.parquet"
    contract_path = tmp_path / "contract.json"
    source = pa.table({"id": [1, 2, 3], "value": [0.1, 0.2, 0.3]})
    arrow_csv.write_csv(source, csv_path)
    parquet.write_table(source, parquet_path)
    _contract(contract_path, {"id": {"required": True, "unique": True}})

    def materialized(*_args, **_kwargs):
        raise AssertionError("materializing file API was selected")

    monkeypatch.setattr(arrow_csv, "read_csv", materialized)
    monkeypatch.setattr(parquet, "read_table", materialized)

    cli.main(["check", str(csv_path), "--contract", str(contract_path), "--batch-size", "1024"])
    assert json.loads(capsys.readouterr().out)["valid"] is True
    cli.main(
        [
            "fingerprint",
            str(parquet_path),
            "--fingerprint-version",
            "v2",
            "--batch-size",
            "2",
        ]
    )
    assert json.loads(capsys.readouterr().out)["fingerprint"].startswith("pf-fp-v2:")


def test_cli_exit_codes_distinguish_verdict_usage_engine_and_resources(tmp_path, capsys):
    valid_path = tmp_path / "valid.parquet"
    invalid_path = tmp_path / "invalid.parquet"
    string_path = tmp_path / "string.parquet"
    contract_path = tmp_path / "contract.json"
    unique_path = tmp_path / "unique.json"
    parquet.write_table(pa.table({"id": [1]}), valid_path)
    parquet.write_table(pa.table({"id": pa.array([None], type=pa.int64())}), invalid_path)
    parquet.write_table(pa.table({"id": ["1"]}), string_path)
    _contract(contract_path, {"id": {"not_null": True}})
    _contract(unique_path, {"id": {"unique": True}})

    cli.main(["check", str(valid_path), "--contract", str(contract_path)])
    capsys.readouterr()

    with pytest.raises(SystemExit, match="1"):
        cli.main(["check", str(invalid_path), "--contract", str(contract_path)])
    capsys.readouterr()

    with pytest.raises(SystemExit, match="2"):
        cli.main(["check"])
    capsys.readouterr()

    with pytest.raises(SystemExit, match="3"):
        cli.main(["diff", str(valid_path), str(string_path), "--key", "id"])
    error = json.loads(capsys.readouterr().err)
    assert error["code"] == "PF_SCHEMA_MISMATCH"

    with pytest.raises(SystemExit, match="4"):
        cli.main(
            [
                "check",
                str(valid_path),
                "--contract",
                str(unique_path),
                "--max-memory",
                "0",
                "--max-temp",
                "0",
            ]
        )
    error = json.loads(capsys.readouterr().err)
    assert error["code"] == "PF_RESOURCE_LIMIT"


def test_diff_streams_full_events_to_an_atomic_jsonl_sink(tmp_path, capsys):
    before_path = tmp_path / "before.parquet"
    after_path = tmp_path / "after.parquet"
    output_path = tmp_path / "changes.jsonl"
    parquet.write_table(pa.table({"id": [1, 2], "value": ["a", "b"]}), before_path)
    parquet.write_table(pa.table({"id": [2, 3], "value": ["changed", "c"]}), after_path)

    cli.main(
        [
            "diff",
            str(before_path),
            str(after_path),
            "--key",
            "id",
            "--output",
            str(output_path),
            "--max-samples",
            "1",
        ]
    )
    report = json.loads(capsys.readouterr().out)
    events = [json.loads(line) for line in output_path.read_text(encoding="utf-8").splitlines()]
    assert report["added_count"] == 1
    assert report["removed_count"] == 1
    assert report["changed_count"] == 1
    assert len(events) == 3
    assert not list(tmp_path.glob(".changes.jsonl.*.tmp"))


def test_atomic_json_writer_never_publishes_a_partial_target(tmp_path):
    output = tmp_path / "receipt.json"
    with pytest.raises(TypeError):
        cli._write_json_atomic(output, {"invalid": object()})
    assert not output.exists()
    assert not list(tmp_path.glob(".receipt.json.*.tmp"))


@pytest.mark.parametrize(
    ("option", "attribute"),
    [
        ("--private-key", "private_key"),
        ("--expected-public-key", "expected_public_key"),
    ],
)
def test_cli_accepts_dash_prefixed_key_option_values(option, attribute):
    command = "sign" if option == "--private-key" else "verify"
    argv = [command, "evidence.json", option, "-urlsafe-key-material"]

    args = cli._parser().parse_args(cli._normalize_key_options(argv))

    assert getattr(args, attribute) == "-urlsafe-key-material"


def test_cli_evidence_sign_and_verify_defaults_to_v2(tmp_path, capsys):
    data_path = tmp_path / "data.parquet"
    contract_path = tmp_path / "contract.json"
    evidence_path = tmp_path / "evidence.json"
    receipt_path = tmp_path / "receipt.json"
    parquet.write_table(pa.table({"id": [1, 2, 3]}), data_path)
    _contract(contract_path, {"id": {"required": True}})

    cli.main(
        [
            "evidence",
            str(data_path),
            "--contract",
            str(contract_path),
            "--output",
            str(evidence_path),
        ]
    )
    evidence = json.loads(capsys.readouterr().out)
    assert evidence["schema"] == "proofframe.evidence.v2"

    keys = generate_keypair()
    private_key = f"-{keys['private_key'][1:]}"
    cli.main(
        [
            "sign",
            str(evidence_path),
            "--private-key",
            private_key,
            "--output",
            str(receipt_path),
        ]
    )
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["schema"] == "proofframe.receipt.v2"
    public_key = receipt["unsigned"]["public_key"]
    cli.main(
        [
            "verify",
            str(receipt_path),
            "--expected-public-key",
            public_key,
        ]
    )
    assert json.loads(capsys.readouterr().out)["valid"] is True
