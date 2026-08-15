import json
import subprocess
import sys

import pyarrow as pa
import pyarrow.csv as arrow_csv
import pytest
import polars as pl

import proofframe


def users(ids=(1, 2, 3), emails=("a@example.com", "b@example.com", "c@example.com")):
    return pa.table({"id": ids, "email": emails, "score": [0.9, 0.4, 0.8]})


def test_profile_is_deterministic():
    first = proofframe.profile(users())
    second = proofframe.profile(users())
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
    report = proofframe.profile(users(), distinct="none")

    assert report["fingerprint"] == proofframe.fingerprint(users())
    assert [column["distinct_count"] for column in report["columns"]] == [None, None, None]

    with pytest.raises(ValueError, match="distinct"):
        proofframe.profile(users(), distinct="approximate")


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
    assert report["findings"][0]["value_fingerprint"]
    assert raw_email not in json.dumps(report)


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


def test_signed_receipt_detects_tampering():
    keys = proofframe.generate_keypair()
    receipt = proofframe.sign_receipt({"valid": True, "rows": 3}, private_key=keys["private_key"])
    assert receipt["public_key"] == keys["public_key"]
    assert proofframe.verify_receipt(receipt)["valid"] is True
    receipt["report"]["rows"] = 4
    verification = proofframe.verify_receipt(receipt)
    assert verification["valid"] is False
    assert verification["report_hash_matches"] is False


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
