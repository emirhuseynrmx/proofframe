import pyarrow as pa

import proofframe as pf


def test_python_check_uses_the_native_v2_relational_engine():
    table = pa.table({"start": [1, 3], "end": [2, 2]})
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "row_rules": [
            {
                "name": "valid_window",
                "compare": {
                    "left": {"column": "start"},
                    "op": "lte",
                    "right": {"column": "end"},
                },
            }
        ],
    }

    report = pf.check(table, contract)

    assert report["valid"] is False
    assert report["violation_count"] == 1
    assert report["findings"][0]["row"] == 1
    assert report["compiled_plan_digest"].startswith("pf-plan-v2:")
    assert report["contract_source_digest"].startswith("pf-contract-v2:")


def test_python_v2_evidence_binds_the_same_native_plan():
    table = pa.table({"customer_id": [1, 1, 2]})
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "dataset_rules": {"distinct_ratio": {"customer_id": {"min": 1.0}}},
    }

    checked = pf.check_with_evidence(table, contract)

    assert checked["report"]["valid"] is False
    assert checked["evidence"]["compiled_plan_digest"].startswith("pf-plan-v2:")
    assert checked["evidence"]["contract_source_digest"].startswith("pf-contract-v2:")
