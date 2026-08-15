import base64
import json
import subprocess
import sys

import polars as pl
import proofframe
import pyarrow as pa
import pyarrow.csv as arrow_csv
import pytest


def users(ids=(1, 2, 3), emails=("a@example.com", "b@example.com", "c@example.com")):
    return pa.table({"id": ids, "email": emails, "score": [0.9, 0.4, 0.8]})


def test_profile_is_deterministic():
    with pytest.warns(RuntimeWarning, match="exact distinct"):
        first = proofframe.profile(users(), distinct="exact")
    with pytest.warns(RuntimeWarning, match="exact distinct"):
        second = proofframe.profile(users(), distinct="exact")
    assert first["rows"] == 3
    assert first["fingerprint"].startswith("pf-fp-v1:")
    assert first["fingerprint"] == second["fingerprint"]
    assert first["columns"][0]["distinct_count"] == 3
    assert (
        first["fingerprint"]
        != proofframe.profile(
            pa.table(
                {
                    "other_id": [1, 2, 3],
                    "email": ["a@example.com", "b@example.com", "c@example.com"],
                    "score": [0.9, 0.4, 0.8],
                }
            )
        )["fingerprint"]
    )


def test_fingerprint_is_invariant_to_batch_segmentation():
    source = pa.table({"id": [1, 2, 3, 4], "flag": [True, False, True, False]})
    batches = [
        pa.record_batch([pa.array([1, 2]), pa.array([True, False])], names=["id", "flag"]),
        pa.record_batch([pa.array([3, 4]), pa.array([True, False])], names=["id", "flag"]),
    ]
    segmented = pa.Table.from_batches(batches, schema=source.schema)

    assert proofframe.profile(source)["fingerprint"] == proofframe.profile(segmented)[
        "fingerprint"
    ]
    assert proofframe.fingerprint(source) == proofframe.profile(source)["fingerprint"]
    assert proofframe.fingerprint(source) == proofframe.fingerprint(segmented)


def test_profile_can_skip_exact_distinct_counts():
    report = proofframe.profile(users())

    assert report["fingerprint"] == proofframe.fingerprint(users())
    assert [column["distinct_count"] for column in report["columns"]] == [None, None, None]

    with pytest.raises(ValueError, match="distinct"):
        proofframe.profile(users(), distinct="approximate")


def test_exact_profile_accepts_hard_resource_limits():
    with pytest.warns(RuntimeWarning, match="exact distinct"):
        report = proofframe.profile(
            pa.table({"id": list(range(20_000))}),
            distinct="exact",
            max_memory=64 * 1024,
            max_temp=32 * 1024 * 1024,
        )

    assert report["columns"][0]["distinct_count"] == 20_000


def test_real_polars_dataframe_uses_arrow_path():
    frame = pl.DataFrame({"id": [1, 2, 3], "score": [0.1, 0.2, 0.3]})

    profile = proofframe.profile(frame, distinct="none")
    report = proofframe.validate(
        frame,
        {"columns": {"id": {"required": True, "unique": True}, "score": {"min": 0, "max": 1}}},
        include_profile=False,
    )

    assert profile["rows"] == 3
    assert profile["fingerprint"] == proofframe.fingerprint(frame)
    assert report["valid"] is True


def test_contract_reports_row_level_evidence():
    report = proofframe.validate(
        pa.table({"id": [1, 1], "email": ["ok@example.com", None], "score": [1.2, 0.5]}),
        {
            "columns": {
                "id": {"required": True, "unique": True},
                "email": {"not_null": True, "pattern": r"^[^@]+@[^@]+$"},
                "score": {"min": 0, "max": 1},
            }
        },
    )
    assert report["valid"] is False
    assert {finding["rule"] for finding in report["findings"]} == {"unique", "not_null", "max"}

    fast = proofframe.validate(
        pa.table({"id": [1, 1], "email": ["ok@example.com", None], "score": [1.2, 0.5]}),
        {
            "columns": {
                "id": {"required": True, "unique": True},
                "email": {"not_null": True, "pattern": r"^[^@]+@[^@]+$"},
                "score": {"min": 0, "max": 1},
            }
        },
        include_profile=False,
    )
    assert fast["mode"] == "rules_only"
    assert fast["rows"] == 2
    assert {finding["rule"] for finding in fast["findings"]} == {"unique", "not_null", "max"}


def test_max_findings_zero_still_reports_invalid_and_truncated():
    report = proofframe.validate(
        pa.table({"id": pa.array([None], type=pa.int64())}),
        {"columns": {"id": {"not_null": True}}, "max_findings": 0},
    )

    assert report["valid"] is False
    assert report["violation_count"] == 1
    assert report["truncated"] is True
    assert report["findings"] == []


def test_diff_reports_changed_columns():
    before = users()
    after = pa.table(
        {
            "id": [1, 2, 4],
            "email": ["a@example.com", "new@example.com", "d@example.com"],
            "score": [0.9, 0.4, 0.7],
        }
    )
    report = proofframe.diff(before, after, keys="id")
    assert report["added_keys"] == ["4"]
    assert report["removed_keys"] == ["3"]
    assert report["changed"] == [{"key": "2", "columns": ["email"]}]


def test_diff_composite_keys_use_canonical_tuples_not_joined_text():
    before = pa.table({"k1": ["a", "a\u001fb"], "k2": ["b\u001fc", "c"], "value": [1, 2]})
    after = pa.table({"k1": ["a", "a\u001fb"], "k2": ["b\u001fc", "c"], "value": [1, 3]})

    report = proofframe.diff(before, after, keys=["k1", "k2"])

    assert report["changed_count"] == 1
    assert report["changed"][0]["columns"] == ["value"]


def test_diff_distinguishes_null_key_from_literal_null_text():
    before = pa.table({"id": pa.array([None, "<null>"], type=pa.string()), "value": [1, 2]})
    after = pa.table({"id": pa.array([None, "<null>"], type=pa.string()), "value": [3, 2]})

    report = proofframe.diff(before, after, keys="id")

    assert report["changed_count"] == 1
    assert report["changed"][0]["columns"] == ["value"]


def test_diff_rejects_same_column_names_with_different_types():
    before = pa.table({"id": pa.array([1], type=pa.int64()), "value": [1]})
    after = pa.table({"id": pa.array(["1"], type=pa.string()), "value": [1]})

    with pytest.raises(ValueError, match="Schemas differ"):
        proofframe.diff(before, after, keys="id")


def test_diff_handles_more_rows_than_one_partition():
    before = pa.table({"id": list(range(160)), "score": [float(value) for value in range(160)]})
    after = pa.table(
        {
            "id": [*range(80), *range(81, 160), 200],
            "score": [
                *(float(value) for value in range(80)),
                *(999.0 if value == 81 else float(value) for value in range(81, 160)),
                200.0,
            ],
        }
    )

    report = proofframe.diff(before, after, keys="id")
    assert report["added_keys"] == ["200"]
    assert report["removed_keys"] == ["80"]
    assert report["changed"] == [{"key": "81", "columns": ["score"]}]


def test_pii_findings_are_redacted():
    raw_email = "private.person@example.com"
    report = proofframe.scan_pii(pa.table({"contact": [raw_email, "not pii"]}))
    assert report["detected"] is True
    assert report["counts_by_kind"] == {"email": 1}
    assert report["findings"][0]["value_fingerprint"].startswith("pf-pii-v2:")
    assert len(report["findings"][0]["value_fingerprint"]) == len("pf-pii-v2:") + 64
    assert raw_email not in json.dumps(report)
    assert report["fingerprint_mode"] == "unlinkable"


def test_pii_fingerprints_support_unlinkable_and_caller_keyed_modes():
    source = pa.table({"contact": ["person@example.com"]})
    first = proofframe.scan_pii(source)
    second = proofframe.scan_pii(source)
    assert first["findings"][0]["value_fingerprint"] != second["findings"][0]["value_fingerprint"]

    key = base64.urlsafe_b64encode(bytes(range(32))).decode().rstrip("=")
    stable_a = proofframe.scan_pii(
        source,
        fingerprint_mode="stable",
        fingerprint_key=key,
        key_id="customer-pii-key-2026",
    )
    stable_b = proofframe.scan_pii(
        source,
        fingerprint_mode="stable",
        fingerprint_key=key,
        key_id="customer-pii-key-2026",
    )
    assert stable_a["findings"][0]["value_fingerprint"] == stable_b["findings"][0][
        "value_fingerprint"
    ]
    assert stable_a["key_id"] == "customer-pii-key-2026"
    assert key not in json.dumps(stable_a)

    with pytest.raises(ValueError, match="32 bytes"):
        proofframe.scan_pii(
            source,
            fingerprint_mode="stable",
            fingerprint_key="too-short",
            key_id="broken",
        )


def test_numeric_pii_matches_are_low_confidence_without_context():
    # Luhn-valid order IDs should not be elevated to a high-confidence card leak by type alone.
    report = proofframe.scan_pii(pa.table({"order_id": [4111111111111111]}))
    assert report["detected"] is True
    assert report["findings"][0]["kind"] == "payment_card"
    assert report["findings"][0]["confidence"] == "low"


def test_leakage_reports_only_hashed_samples():
    train = pa.table({"id": [1, 2, 3], "feature": ["a", "b", "c"]})
    test = pa.table({"id": [3, 4], "feature": ["x", "d"]})
    report = proofframe.detect_leakage(train, test, keys="id")
    assert report["overlap_count"] == 1
    assert report["mode"] == "key"
    assert report["sample_fingerprints"][0] != "3"


def test_full_and_fast_validation_agree_on_edge_numeric_values():
    source = pa.table({"id": [2**53 + 1, 2**53 + 2], "score": [-0.0, 0.0]})
    contract = {
        "columns": {
            "id": {"unique": True, "min": 2**53},
            "score": {"unique": True, "min": -0.0, "max": 0.0},
        }
    }
    full = proofframe.validate(source, contract, include_profile=True)
    fast = proofframe.validate(source, contract, include_profile=False)

    assert full["valid"] == fast["valid"]
    assert [(item["rule"], item["column"], item["row"]) for item in full["findings"]] == [
        (item["rule"], item["column"], item["row"]) for item in fast["findings"]
    ]


def test_fast_unique_handles_signed_and_large_integer_domains():
    values = [-(2**63), -1, 0, 2**63 - 1, -(2**63)]
    report = proofframe.validate(
        pa.table({"id": values}),
        {"columns": {"id": {"unique": True}}},
        include_profile=False,
    )
    assert report["valid"] is False
    assert report["findings"] == [
        {
            "rule": "unique",
            "column": "id",
            "row": 4,
            "message": "Duplicate value detected",
        }
    ]


def test_v2_evidence_receipt_detects_tampering():
    keys = proofframe.generate_keypair()
    contract = {
        "version": "proofframe.contract.v1",
        "columns": {"id": {"required": True}},
    }
    evidence = proofframe.check_with_evidence(users(), contract)["evidence"]
    assert evidence["schema"] == "proofframe.evidence.v2"
    assert evidence["contract_source_digest"].startswith("pf-contract-v1:")
    assert evidence["compiled_plan_digest"].startswith("pf-plan-v1:")
    assert evidence["schema_digest"].startswith("pf-schema-v1:")

    receipt = proofframe.sign_evidence(evidence, private_key=keys["private_key"])
    assert receipt["unsigned"]["public_key"] == keys["public_key"]
    assert (
        proofframe.verify_receipt(receipt, expected_public_key=keys["public_key"])["valid"]
        is True
    )
    receipt["unsigned"]["evidence"]["result"]["violation_count"] = 4
    verification = proofframe.verify_receipt(receipt, expected_public_key=keys["public_key"])
    assert verification["valid"] is False
    assert verification["report_hash_matches"] is False


def test_check_with_evidence_binds_result_and_fingerprint_in_one_reader_pass():
    contract = {
        "version": "proofframe.contract.v1",
        "columns": {"id": {"unique": True}},
    }
    source = pa.RecordBatchReader.from_batches(
        pa.schema([("id", pa.int64())]),
        [pa.record_batch({"id": [1, 1]})],
    )

    checked = proofframe.check_with_evidence(source, contract)

    assert checked["report"]["valid"] is False
    assert checked["report"]["violation_count"] == 1
    assert checked["evidence"]["dataset"]["rows"] == 2
    assert checked["evidence"]["result"]["valid"] is False
    assert checked["evidence"]["result"]["violation_count"] == 1
    assert checked["evidence"]["result"]["output_records"] == 1
    assert checked["evidence"]["result"]["truncated"] is False
    assert checked["evidence"]["result"]["result_digest"].startswith("pf-result-v1:")
    assert checked["evidence"]["result"]["report_digest"].startswith("pf-report-v1:")
    assert checked["evidence"]["result"]["findings_digest"].startswith("pf-findings-v1:")
    assert checked["evidence"]["result"]["metrics_digest"].startswith("pf-metrics-v1:")


def test_v1_receipt_requires_explicit_opt_in():
    keys = proofframe.generate_keypair()
    receipt = proofframe.sign_receipt(
        {"valid": True, "rows": 3},
        private_key=keys["private_key"],
        receipt_version="v1",
    )
    assert receipt["schema"] == "proofframe.receipt.v1"
    assert proofframe.verify_receipt(receipt)["legacy"] is True


def test_evidence_builder_rejects_a_report_from_another_contract():
    source = users()
    checked_contract = {
        "version": "proofframe.contract.v1",
        "columns": {"id": {"required": True}},
    }
    different_contract = {
        "version": "proofframe.contract.v1",
        "columns": {"score": {"min": 0.5}},
    }
    report = proofframe.check(source, checked_contract)

    with pytest.raises(proofframe.ReceiptError, match="digest does not match"):
        proofframe.assemble_evidence_unchecked(source, different_contract, report)


def test_cli_profiles_csv(tmp_path):
    source = tmp_path / "users.csv"
    arrow_csv.write_csv(users(), source)
    result = subprocess.run(
        [sys.executable, "-m", "proofframe.cli", "profile", str(source)],
        check=True,
        capture_output=True,
        text=True,
    )
    assert json.loads(result.stdout)["rows"] == 3
