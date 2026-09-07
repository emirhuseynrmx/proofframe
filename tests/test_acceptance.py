import copy
import hashlib
import json

import proofframe as pf
import pytest


def test_acceptance_api_exists():
    assert callable(getattr(pf, "accept_file", None))


def test_explicit_csv_acceptance_and_signature(tmp_path):
    path = tmp_path / "input.csv"
    path.write_bytes("id;amount\n1;12,50\n2;NA\n".encode("cp1254"))
    keys = pf.generate_keypair()
    options = {
        "delimiter": ";",
        "encoding": "cp1254",
        "decimal_point": ",",
        "column_types": {"id": "int64", "amount": "float64"},
        "null_values": ["NA"],
    }
    policy = {"version": "proofframe.acceptance-policy.v1", "minimum_evaluated": {"amount": 1}}
    bundle = pf.accept_file(
        path,
        {"columns": {"amount": {"min": 0}}},
        policy=policy,
        csv_options=options,
        private_key=keys["private_key"],
    )
    assert bundle["payload"]["decision"]["status"] == "accepted"
    assert bundle["payload"]["read_settings"]["csv"]["decimal_point"] == ","
    assert pf.verify_acceptance(bundle, expected_public_key=keys["public_key"])["valid"]
    changed = copy.deepcopy(bundle)
    changed["payload"]["policy"]["minimum_evaluated"]["amount"] = 0
    assert not pf.verify_acceptance(changed, expected_public_key=keys["public_key"])["valid"]


def test_zero_evaluation_rejected_and_parse_failure_unknown(tmp_path):
    path = tmp_path / "null.csv"
    path.write_text("amount\nNA\n", encoding="utf8")
    options = {"column_types": {"amount": "float64"}, "null_values": ["NA"]}
    bundle = pf.accept_file(
        path,
        {"columns": {"amount": {"min": 0}}},
        policy={"minimum_evaluated": {"amount": 1}},
        csv_options=options,
    )
    assert bundle["payload"]["decision"]["status"] == "rejected"
    path.write_text("amount\nhello\n", encoding="utf8")
    failed = pf.accept_file(path, {"columns": {"amount": {"min": 0}}}, csv_options=options)
    assert failed["payload"]["decision"]["status"] == "unknown"
    assert failed["payload"]["evidence"] is None


def test_unknown_policy_rejected_before_io(tmp_path):
    with pytest.raises(ValueError, match="policy"):
        pf.accept_file(tmp_path / "missing.csv", {}, policy={"typo": 1})


def test_bundle_output_never_overwrites(tmp_path):
    source = tmp_path / "input.csv"
    source.write_text("x\n1\n", encoding="utf8")
    output = tmp_path / "result.json"
    pf.accept_file(source, {"columns": {"x": {"min": 0}}}, output=output)
    assert json.loads(output.read_text())["payload"]["decision"]["status"] == "accepted"
    with pytest.raises(FileExistsError):
        pf.accept_file(source, {}, output=output)


def test_signature_survives_no_payload_tamper_even_rehashed(tmp_path):
    from proofframe.acceptance import _digest

    path = tmp_path / "input.csv"
    path.write_text("x\n-1\n", encoding="utf8")
    keys = pf.generate_keypair()
    bundle = pf.accept_file(path, {"columns": {"x": {"min": 0}}}, private_key=keys["private_key"])
    assert bundle["payload"]["decision"]["status"] == "rejected"
    bundle["payload"]["decision"]["status"] = "accepted"
    bundle["sha256"] = _digest(bundle["payload"])
    assert not pf.verify_acceptance(bundle)["valid"]


def test_missing_file_is_unknown_and_unsigned_is_not_authenticated(tmp_path):
    bundle = pf.accept_file(tmp_path / "missing.csv", {"columns": {"x": {"min": 0}}})
    assert bundle["payload"]["decision"]["status"] == "unknown"
    assert pf.verify_acceptance(bundle) == {"valid": True, "authenticated": False}
    assert not pf.verify_acceptance(
        bundle, expected_public_key=pf.generate_keypair()["public_key"]
    )["valid"]


def test_acceptance_cli(tmp_path, capsys):
    from proofframe.cli import main

    path = tmp_path / "input.csv"
    path.write_text("x\n1\n", encoding="utf8")
    contract = tmp_path / "contract.json"
    contract.write_text(json.dumps({"columns": {"x": {"min": 0}}}))
    output = tmp_path / "acceptance.json"
    main(["accept", str(path), "--contract", str(contract), "--output", str(output)])
    assert json.loads(capsys.readouterr().out)["payload"]["decision"]["status"] == "accepted"
    main(["verify-acceptance", str(output)])
    assert json.loads(capsys.readouterr().out)["valid"]


def test_resource_exhaustion_is_unknown(tmp_path):
    path = tmp_path / "input.csv"
    path.write_text("x\n" + "\n".join(str(i) for i in range(1000)), encoding="utf8")
    bundle = pf.accept_file(
        path, {"columns": {"x": {"unique": True}}}, max_memory=1, max_temp=0, spill="never"
    )
    assert bundle["payload"]["decision"]["status"] == "unknown"
    assert bundle["payload"]["evidence"] is None


def test_missing_evaluated_column_cannot_silently_pass(tmp_path):
    path = tmp_path / "input.csv"
    path.write_text("x\n1\n", encoding="utf8")
    bundle = pf.accept_file(
        path, {"columns": {"x": {"min": 0}}}, policy={"minimum_evaluated": {"typo": 0}}
    )
    assert bundle["payload"]["decision"]["status"] == "rejected"


def test_parquet_and_empty_data(tmp_path):
    import pyarrow as pa
    import pyarrow.parquet as pq

    path = tmp_path / "input.parquet"
    pq.write_table(pa.table({"x": pa.array([], type=pa.int64())}), path)
    bundle = pf.accept_file(
        path, {"columns": {"x": {"min": 0}}}, policy={"minimum_evaluated": {"x": 1}}
    )
    assert bundle["payload"]["decision"]["status"] == "rejected"
    assert bundle["payload"]["read_settings"]["format"] == "parquet"
    with pytest.raises(ValueError, match="CSV"):
        pf.accept_file(path, {}, csv_options={"delimiter": ";"})


def test_output_budget_leaves_no_partial_artifact(tmp_path):
    source = tmp_path / "input.csv"
    source.write_text("x\n1\n", encoding="utf8")
    output = tmp_path / "bundle.json"
    with pytest.raises(pf.ResourceLimitError, match="output"):
        pf.accept_file(source, {"columns": {"x": {"min": 0}}}, output=output, max_output_bytes=1)
    assert not output.exists()


def _rehash(bundle):
    payload = json.dumps(bundle["payload"], sort_keys=True, separators=(",", ":")).encode()
    bundle["sha256"] = hashlib.sha256(payload).hexdigest()
    return bundle


def _accepted_bundle(tmp_path):
    path = tmp_path / "amounts.csv"
    path.write_text("amount\n1.0\n2.0\n", encoding="utf8")
    bundle = pf.accept_file(path, {"columns": {"amount": {"min": 0}}}, policy={"max_violations": 0})
    assert bundle["payload"]["decision"]["status"] == "accepted"
    assert pf.verify_acceptance(bundle)["valid"]
    return bundle


def test_a_payload_missing_required_fields_does_not_verify(tmp_path):
    """A digest proves what is present was not edited, not that anything is present.

    Removing the decision and the evidence and recomputing the hash leaves a bundle
    that is internally consistent and says nothing at all.
    """
    bundle = _accepted_bundle(tmp_path)
    for field in ("decision", "report", "evidence"):
        bundle["payload"].pop(field)

    assert not pf.verify_acceptance(_rehash(bundle))["valid"]


def test_an_accepted_decision_without_its_scan_does_not_verify(tmp_path):
    bundle = _accepted_bundle(tmp_path)
    bundle["payload"]["evidence"] = None

    assert not pf.verify_acceptance(_rehash(bundle))["valid"]


def test_an_unrecognized_status_or_extra_field_does_not_verify(tmp_path):
    invented = _accepted_bundle(tmp_path)
    invented["payload"]["decision"]["status"] = "provisionally-fine"
    assert not pf.verify_acceptance(_rehash(invented))["valid"]

    extended = _accepted_bundle(tmp_path)
    extended["payload"]["override"] = True
    assert not pf.verify_acceptance(_rehash(extended))["valid"]


def test_an_unknown_decision_may_legitimately_carry_no_scan(tmp_path):
    bundle = _accepted_bundle(tmp_path)
    bundle["payload"]["report"] = None
    bundle["payload"]["evidence"] = None
    bundle["payload"]["decision"] = {"status": "unknown", "reasons": ["OSError"], "evaluated": {}}

    assert pf.verify_acceptance(_rehash(bundle))["valid"]


def test_a_real_unknown_from_a_missing_file_verifies(tmp_path):
    bundle = pf.accept_file(tmp_path / "absent.csv", {"columns": {"amount": {"min": 0}}})

    assert bundle["payload"]["decision"]["status"] == "unknown"
    assert pf.verify_acceptance(bundle)["valid"]


def _violating_bundle(tmp_path):
    path = tmp_path / "amounts.csv"
    path.write_text("amount\n1.0\n-5.0\n", encoding="utf8")
    bundle = pf.accept_file(
        path, {"columns": {"amount": {"min": 0}}}, policy={"max_violations": 99}
    )
    assert bundle["payload"]["decision"]["status"] == "accepted"
    assert bundle["payload"]["report"]["violation_count"] == 1
    assert pf.verify_acceptance(bundle)["valid"]
    return bundle


@pytest.mark.parametrize(
    ("name", "mutate"),
    [
        ("missing report field", lambda p: p["report"].pop("violation_count")),
        ("negative count", lambda p: p["report"].__setitem__("violation_count", -1)),
        # bool is an int in Python, so a count of True would pass a naive type check.
        ("boolean count", lambda p: p["report"].__setitem__("violation_count", True)),
        ("row count as text", lambda p: p["report"].__setitem__("rows", "2")),
        ("validity as int", lambda p: p["report"].__setitem__("valid", 1)),
        ("findings as mapping", lambda p: p["report"].__setitem__("findings", {})),
        ("metrics as list", lambda p: p["report"].__setitem__("metrics", [])),
        ("coverage lists disagree", lambda p: p["report"].__setitem__("evaluated_indices", [])),
        ("negative coverage", lambda p: p["report"].__setitem__("evaluated_columns", [-1])),
        ("older evidence schema", lambda p: p["evidence"].__setitem__("schema", "x.v1")),
        ("evidence missing dataset", lambda p: p["evidence"].pop("dataset")),
        ("evidence result as text", lambda p: p["evidence"].__setitem__("result", "x")),
        ("accepted with no report", lambda p: p.__setitem__("report", None)),
        ("unknown that kept a scan", lambda p: p["decision"].__setitem__("status", "unknown")),
    ],
)
def test_a_report_or_evidence_of_the_wrong_shape_does_not_verify(tmp_path, name, mutate):
    """Present-but-wrong is a stronger claim than absent, and must fail at least as hard.

    A reader who sees `valid` goes on to use the report. A field that exists with the
    wrong type or an impossible count would be used, which is worse than a field the
    reader can see is missing.
    """
    bundle = _violating_bundle(tmp_path)
    mutate(bundle["payload"])

    assert not pf.verify_acceptance(_rehash(bundle))["valid"], name


def test_an_untouched_bundle_still_verifies_signed_and_unsigned(tmp_path):
    path = tmp_path / "amounts.csv"
    path.write_text("amount\n1.0\n2.0\n", encoding="utf8")
    keys = pf.generate_keypair()
    signed = pf.accept_file(
        path, {"columns": {"amount": {"min": 0}}}, private_key=keys["private_key"]
    )

    assert pf.verify_acceptance(signed, expected_public_key=keys["public_key"]) == {
        "valid": True,
        "authenticated": True,
    }
