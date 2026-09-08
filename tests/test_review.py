import json
from concurrent.futures import ThreadPoolExecutor

import proofframe as pf
import pyarrow as pa
import pytest
from proofframe.review import review


def data():
    return pa.table({"id": [1, 1, 3, 4], "amount": [4, -1, -2, 5]})


def contract():
    return {
        "version": "proofframe.contract.v2",
        "columns": {"id": {"unique": True}, "amount": {"min": 0}},
    }


def test_default_privacy_has_exact_total_and_action_without_samples(tmp_path):
    result = review(data(), contract(), tmp_path / "report")
    assert result["valid"] is False
    assert result["violation_count"] == 3
    report = json.loads((tmp_path / "report/report.json").read_text())
    evidence = json.loads((tmp_path / "report/evidence.json").read_text())
    assert report["findings"] == []
    assert evidence["result"]["violation_count"] == 3
    html = (tmp_path / "report/index.html").read_text()
    assert "--max-samples 20" in html
    assert "Start here" in html
    assert "3 violations" in html
    assert "0 failing rows" not in html


def test_bounded_sample_does_not_turn_three_violations_into_one(tmp_path):
    review(data(), contract(), tmp_path / "sample", max_samples=1)
    report = json.loads((tmp_path / "sample/report.json").read_text())
    text = (tmp_path / "sample/summary.md").read_text()
    assert report["violation_count"] == 3
    assert len(report["findings"]) == 1
    assert "1 sampled finding" in text
    assert "3 violations" in text
    assert "not a complete list" in text


def test_reader_consumed_once_and_evidence_can_be_signed(tmp_path):
    source = data()
    count = []

    def batches():
        for batch in source.to_batches(max_chunksize=1):
            count.append(1)
            yield batch

    review(
        pa.RecordBatchReader.from_batches(source.schema, batches()), contract(), tmp_path / "one"
    )
    assert len(count) == 4
    evidence = json.loads((tmp_path / "one/evidence.json").read_text())
    keys = pf.generate_keypair()
    receipt = pf.sign_evidence(evidence, private_key=keys["private_key"])
    assert pf.verify_receipt(receipt, expected_public_key=keys["public_key"])["valid"]


def test_html_escapes_untrusted_content_with_samples_enabled(tmp_path):
    column = "<script>alert(1)</script>"
    table = pa.table({column: pa.array([None], type=pa.string())})
    rules = {"version": "proofframe.contract.v2", "columns": {column: {"not_null": True}}}
    review(
        table, rules, tmp_path / "xss", max_samples=20, label="</title><img src=x onerror=alert(1)>"
    )
    html = (tmp_path / "xss/index.html").read_text()
    assert "<script>alert(1)</script>" not in html
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in html
    assert "<img src=x" not in html
    assert "Content-Security-Policy" in html
    assert "<script" not in html


def test_draft_and_output_limit_never_publish_partial_results(tmp_path):
    rules = contract() | {"status": "draft"}
    table = data()
    with pytest.raises(pf.ContractError):
        review(table, rules, tmp_path / "draft")
    assert not (tmp_path / "draft").exists()
    table, valid = data(), contract()
    with pytest.raises(pf.ResourceLimitError):
        review(table, valid, tmp_path / "small", max_output_bytes=100)
    assert not (tmp_path / "small").exists()
    assert list(tmp_path.iterdir()) == []


def test_existing_output_is_preserved(tmp_path):
    target = tmp_path / "existing"
    target.mkdir()
    (target / "keep").write_text("user file")
    table, rules = data(), contract()
    with pytest.raises(FileExistsError):
        review(table, rules, target)
    assert (target / "keep").read_text() == "user file"


def test_two_publishers_have_only_one_winner(tmp_path):
    target = tmp_path / "shared"

    def generate(_):
        try:
            review(data(), contract(), target)
            return "ok"
        except FileExistsError:
            return "exists"

    with ThreadPoolExecutor(max_workers=2) as pool:
        assert sorted(pool.map(generate, range(2))) == ["exists", "ok"]
    assert json.loads((target / "report.json").read_text())["violation_count"] == 3


def test_io_failure_cleans_staging_and_never_publishes(tmp_path, monkeypatch):
    from importlib import import_module

    module = import_module("proofframe.review")

    def fail(*args, **kwargs):
        raise OSError("disk write failed")

    table, rules = data(), contract()
    monkeypatch.setattr(module, "_write_chunks", fail)
    with pytest.raises(OSError):
        review(table, rules, tmp_path / "broken")
    assert list(tmp_path.iterdir()) == []


def test_contract_display_uses_same_snapshot_as_native_scan(tmp_path):
    rules = contract()

    def batches():
        rules["columns"]["amount"]["min"] = 9999999
        yield data().to_batches()[0]

    reader = pa.RecordBatchReader.from_batches(data().schema, batches())
    review(reader, rules, tmp_path / "snapshot")
    assert "9999999" not in (tmp_path / "snapshot/index.html").read_text()


def test_markdown_does_not_create_links_from_sample_names(tmp_path):
    column = "[click](https://untrusted.invalid)"
    review(
        pa.table({column: pa.array([None], type=pa.string())}),
        {"columns": {column: {"not_null": True}}},
        tmp_path / "markdown",
        max_samples=1,
    )
    assert column not in (tmp_path / "markdown/summary.md").read_text()


@pytest.mark.parametrize("valid", [True, False])
def test_cli_review_exit_code_and_evidence_compatibility(tmp_path, valid, capsys):
    from proofframe import cli
    from pyarrow import parquet

    path = tmp_path / "orders.parquet"
    parquet.write_table(pa.table({"amount": [1 if valid else -1]}), path)
    cpath = tmp_path / "contract.json"
    cpath.write_text(json.dumps({"columns": {"amount": {"min": 0}}}))
    arguments = ["review", str(path), "--contract", str(cpath), "--out", str(tmp_path / "review")]
    if valid:
        cli.main(arguments)
    else:
        with pytest.raises(SystemExit) as error:
            cli.main(arguments)
        assert error.value.code == 1
    assert json.loads(capsys.readouterr().out)["valid"] is valid
    cli.main(["evidence", str(path), "--contract", str(cpath)])
    assert json.loads(capsys.readouterr().out)["result"]["valid"] is valid


@pytest.mark.parametrize("typ", ["string", "integer", {"name": "decimal128", "scale": 2}])
def test_python_type_error_identifies_column(tmp_path, typ):
    table = data()
    rules = {"version": "proofframe.contract.v2", "columns": {"amount": {"type": typ}}}
    with pytest.raises(pf.ContractError) as error:
        review(table, rules, tmp_path / "invalid")
    assert "amount" in str(error.value)
    assert "untagged enum" not in str(error.value)


def test_missing_version_points_to_v2_in_python():
    table = data()
    with pytest.raises(pf.ContractError) as error:
        pf.check(table, {"status": "active", "columns": {}})
    assert "proofframe.contract.v2" in str(error.value)


def test_native_rule_message_is_escaped_with_samples(tmp_path):
    name = "<img src=x onerror=alert(9)>"
    rules = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "row_rules": [
            {
                "name": name,
                "compare": {"left": {"column": "amount"}, "op": "gte", "right": {"literal": 0}},
            }
        ],
    }
    review(data(), rules, tmp_path / "message", max_samples=20)
    report = json.loads((tmp_path / "message/report.json").read_text())
    assert name in json.dumps(report["findings"])
    output = (tmp_path / "message/index.html").read_text()
    assert name not in output
    assert "&lt;img" in output


def test_the_tools_own_prose_stays_copyable(tmp_path):
    """A reader is told to run a command; escaping it would hand them a broken one."""
    review(data(), contract(), tmp_path / "prose")
    summary = (tmp_path / "prose/summary.md").read_text(encoding="utf-8")
    assert "--max-samples 20" in summary, summary
    assert r"\-\-max\-samples" not in summary
    assert "Rows are zero indexed." not in summary  # no samples: this line is not shown
    assert r"\." not in summary


def test_static_sentences_are_not_escaped_when_samples_are_on(tmp_path):
    review(data(), contract(), tmp_path / "prose-samples", max_samples=5)
    summary = (tmp_path / "prose-samples/summary.md").read_text(encoding="utf-8")
    assert "Inspect the min rule in your contract. Rows are zero indexed." in summary, summary


def test_a_dataset_name_keeps_its_dots(tmp_path):
    review(data(), contract(), tmp_path / "label", label="orders.2026-09.parquet")
    summary = (tmp_path / "label/summary.md").read_text(encoding="utf-8")
    assert "Dataset: orders.2026-09.parquet" in summary, summary


def test_hostile_names_are_still_escaped_in_the_action_line(tmp_path):
    """Relaxing the escape set must not let data reach the reader as markup."""
    column = "[x](javascript:alert(1))*bold*`code`"
    review(
        pa.table({column: pa.array([None], type=pa.string())}),
        {"columns": {column: {"not_null": True}}},
        tmp_path / "hostile",
        max_samples=1,
    )
    summary = (tmp_path / "hostile/summary.md").read_text(encoding="utf-8")
    assert column not in summary
    assert "[x](" not in summary
    assert "*bold*" not in summary
    assert "`code`" not in summary


def test_a_passing_contract_reports_rules_that_were_never_asked(tmp_path):
    """Green because nothing was wrong, or green because nothing was checked?"""
    table = pa.table(
        {
            "amount": pa.array([1, 2, 3], type=pa.int64()),
            "region": pa.array([None, None, None], type=pa.string()),
        }
    )
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {"amount": {"min": 0}, "region": {"allowed": ["eu", "us"]}},
    }
    review(table, contract, tmp_path / "coverage")

    report = json.loads((tmp_path / "coverage/report.json").read_text(encoding="utf-8"))
    assert report["valid"] is True
    assert report["evaluated_columns"] == [3, 0]

    summary = (tmp_path / "coverage/summary.md").read_text(encoding="utf-8")
    assert "region: 0 values evaluated" in summary
    assert "A rule that was never asked did not pass." in summary


def test_a_fully_exercised_contract_says_nothing_extra(tmp_path):
    review(data(), contract(), tmp_path / "exercised")
    report = json.loads((tmp_path / "exercised/report.json").read_text(encoding="utf-8"))
    assert all(count > 0 for count in report["evaluated_columns"])
    summary = (tmp_path / "exercised/summary.md").read_text(encoding="utf-8")
    assert "values evaluated" not in summary


def test_an_empty_dataset_exercises_nothing(tmp_path):
    empty = pa.table({"amount": pa.array([], type=pa.int64())})
    review(empty, {"columns": {"amount": {"min": 0}}}, tmp_path / "empty")
    report = json.loads((tmp_path / "empty/report.json").read_text(encoding="utf-8"))
    assert report["valid"] is True, "no rows means no violations"
    assert report["evaluated_columns"] == [0]
    assert "amount: 0 values evaluated" in (tmp_path / "empty/summary.md").read_text(
        encoding="utf-8"
    )


def test_the_html_also_names_the_rules_that_were_never_asked(tmp_path):
    table = pa.table(
        {
            "amount": pa.array([1, 2, 3], type=pa.int64()),
            "region": pa.array([None, None, None], type=pa.string()),
        }
    )
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {"amount": {"min": 0}, "region": {"allowed": ["eu", "us"]}},
    }
    review(table, contract, tmp_path / "html-coverage")
    page = (tmp_path / "html-coverage/index.html").read_text(encoding="utf-8")
    assert "Rules that were never asked" in page
    assert "<code>region</code>" in page
    assert "0 values evaluated" in page


def test_a_hostile_column_name_cannot_inject_through_the_coverage_block(tmp_path):
    column = "<script>alert(1)</script>"
    review(
        pa.table({column: pa.array([None, None], type=pa.string())}),
        {"columns": {column: {"allowed": ["eu"]}}},
        tmp_path / "html-hostile",
    )
    page = (tmp_path / "html-hostile/index.html").read_text(encoding="utf-8")
    assert "<script>alert(1)</script>" not in page
    assert "&lt;script&gt;" in page
    summary = (tmp_path / "html-hostile/summary.md").read_text(encoding="utf-8")
    assert "<script>" not in summary
