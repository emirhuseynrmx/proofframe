import proofframe as pf
import pyarrow as pa
import pytest
from proofframe import _proofframe


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


def test_python_partition_validation_uses_global_rows_and_exact_dataset_state():
    partitions = [
        pa.table({"order_id": [1, 1], "line_id": [1, 2]}),
        pa.table({"order_id": [1], "line_id": [1]}),
    ]
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "dataset_rules": {
            "composite_unique": [
                {"name": "line_key", "columns": ["order_id", "line_id"]}
            ]
        },
    }

    report = pf.check_partitions(partitions, contract, threads=2)

    assert report["violation_count"] == 1
    assert report["findings"][0]["row"] == 2
    assert report["compiled_plan_digest"].startswith("pf-plan-v2:")


def test_python_rejects_non_positive_worker_counts_before_native_execution():
    with pytest.raises(ValueError, match="threads must be at least 1"):
        pf.check(pa.table({"id": [1]}), {"columns": {}}, threads=0)

    with pytest.raises(ValueError, match="threads must be at least 1"):
        pf.check_partitions(
            [pa.table({"id": [1]})], {"columns": {}}, threads=-1
        )


def test_python_partition_evidence_binds_ordered_native_fingerprints():
    checked = pf.check_partitions_with_evidence(
        [pa.table({"id": [1, 2]}), pa.table({"id": [3]})],
        {
            "version": "proofframe.contract.v2",
            "columns": {"id": {"not_null": True}},
        },
        threads=2,
    )

    assert checked["report"]["rows"] == 3
    assert len(checked["manifest"]["partitions"]) == 2
    assert checked["manifest"]["partitions"][0]["rows"] == 2
    assert checked["manifest"]["partitions"][1]["rows"] == 1
    assert checked["manifest"]["root_digest"].startswith("pf-partition-root-v1:")


def test_release_benchmark_can_time_only_the_compiled_native_scan():
    table = pa.table({"low": [1, 2, 3], "high": [2, 3, 4]})
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "row_rules": [
            {
                "name": "ordered",
                "compare": {
                    "left": {"column": "low"},
                    "op": "lt",
                    "right": {"column": "high"},
                },
            }
        ],
    }

    result = _proofframe.benchmark_check_arrow(
        table.to_reader(), __import__("json").dumps(contract)
    )

    assert result["report"]["valid"] is True
    assert result["native_elapsed_ns"] > 0
