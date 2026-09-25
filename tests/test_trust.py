"""0.7.2: a receipt is valid only under a key the verifier trusts."""

import json

import proofframe as pf
import pytest
from proofframe.cli import main

REPORT = {"valid": True, "rows": 3}


def test_a_receipt_signed_by_a_stranger_is_intact_but_not_valid():
    stranger = pf.generate_keypair()
    receipt = pf.sign_receipt(REPORT, private_key=stranger["private_key"], receipt_version="v1")

    unpinned = pf.verify_receipt(receipt)
    assert unpinned["intact"]
    assert not unpinned["signer_trusted"]
    assert not unpinned["valid"]

    trusted = pf.generate_keypair()
    wrong_key = pf.verify_receipt(receipt, expected_public_key=trusted["public_key"])
    assert wrong_key["intact"] and not wrong_key["valid"]

    right_key = pf.verify_receipt(receipt, expected_public_key=stranger["public_key"])
    assert right_key["intact"] and right_key["valid"] and right_key["signer_trusted"]


def test_verify_without_a_key_exits_nonzero_and_says_why(tmp_path, capsys):
    keys = pf.generate_keypair()
    path = tmp_path / "receipt.json"
    path.write_text(
        json.dumps(pf.sign_receipt(REPORT, private_key=keys["private_key"], receipt_version="v1"))
    )

    with pytest.raises(SystemExit) as refused:
        main(["verify", str(path)])
    assert refused.value.code == 1
    out = json.loads(capsys.readouterr().out)
    assert out["intact"] and not out["valid"] and "no trusted key" in out["reason"]

    main(["verify", str(path), "--integrity-only"])
    out = json.loads(capsys.readouterr().out)
    assert out["intact"] and "integrity only" in out["checked"]

    main(["verify", str(path), "--expected-public-key", keys["public_key"]])
    assert json.loads(capsys.readouterr().out)["valid"]


def test_sign_reads_the_key_from_a_file(tmp_path, capsys):
    keys = pf.generate_keypair()
    report = tmp_path / "report.json"
    report.write_text(json.dumps(REPORT))
    key_file = tmp_path / "signer.key"
    key_file.write_text(keys["private_key"] + "\n")

    main(["sign", str(report), "--private-key-file", str(key_file), "--receipt-version", "v1"])
    captured = capsys.readouterr()
    receipt = json.loads(captured.out)
    assert "warning" not in captured.err
    assert pf.verify_receipt(receipt, expected_public_key=keys["public_key"])["valid"]


def test_a_key_on_the_command_line_still_works_but_warns(tmp_path, capsys):
    keys = pf.generate_keypair()
    report = tmp_path / "report.json"
    report.write_text(json.dumps(REPORT))

    main(["sign", str(report), "--private-key", keys["private_key"], "--receipt-version", "v1"])
    captured = capsys.readouterr()
    assert "--private-key-file" in captured.err
    assert pf.verify_receipt(json.loads(captured.out), expected_public_key=keys["public_key"])[
        "valid"
    ]


def test_sign_needs_exactly_one_key_source(tmp_path):
    report = tmp_path / "report.json"
    report.write_text(json.dumps(REPORT))
    with pytest.raises(SystemExit) as missing:
        main(["sign", str(report)])
    assert missing.value.code == 2
