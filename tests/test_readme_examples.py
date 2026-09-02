import subprocess
import sys
from pathlib import Path

import proofframe as pf
import pyarrow as pa
import pytest

ROOT = Path(__file__).resolve().parents[1]


@pytest.mark.parametrize(
    "example",
    [
        "pandas_validation.py",
        "polars_parquet_budget.py",
        "keyed_diff.py",
        "pii_and_leakage.py",
        "evidence_and_receipt.py",
        "suggest_review_check.py",
    ],
)
def test_example_workflows_remain_executable(example: str) -> None:
    """Catch examples that no longer exercise their documented workflow."""
    subprocess.run([sys.executable, ROOT / "examples" / example], check=True)


def test_readme_check_example_stays_executable() -> None:
    table = pa.table(
        {
            "order_id": [101, 102, 102],
            "amount": [12.50, 8.00, -1.00],
        }
    )
    contract = {
        "version": "proofframe.contract.v1",
        "columns": {
            "order_id": {"required": True, "not_null": True, "unique": True},
            "amount": {"required": True, "min": 0},
        },
        "max_findings": 20,
    }

    report = pf.check(
        table,
        contract,
        max_memory=64 * 1024 * 1024,
        max_temp=512 * 1024 * 1024,
        max_samples=20,
    )

    assert report["valid"] is False
    assert report["violation_count"] == 2


def test_readme_relational_and_conditional_example_stays_executable() -> None:
    shipments = pa.table(
        {
            "ordered_at": [1, 3],
            "delivered_at": [2, 2],
            "status": ["delivered", "pending"],
            "tracking_id": ["TR-1", None],
        }
    )
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "row_rules": [
            {
                "name": "delivery_window",
                "compare": {
                    "left": {"column": "ordered_at"},
                    "op": "lte",
                    "right": {"column": "delivered_at"},
                },
            },
            {
                "name": "delivered_has_tracking",
                "when": {
                    "left": {"column": "status"},
                    "op": "eq",
                    "right": {"literal": "delivered"},
                },
                "assert": {"column": "tracking_id", "not_null": True},
            },
        ],
    }

    report = pf.check(shipments, contract)

    assert report["valid"] is False
    assert any(
        finding["rule"] == "compare" and "delivery_window" in finding["message"]
        for finding in report["findings"]
    )


def test_readme_dataset_and_partition_example_stays_executable() -> None:
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {},
        "dataset_rules": {
            "row_count": {"min": 3},
            "distinct_ratio": {"order_id": {"min": 0.5}},
            "composite_unique": [
                {"name": "line_key", "columns": ["order_id", "line_id"]}
            ],
        },
    }
    partitions = [
        pa.table({"order_id": [101, 101], "line_id": [1, 2]}),
        pa.table({"order_id": [102], "line_id": [1]}),
    ]

    report = pf.check_partitions(partitions, contract, threads=2)

    assert report["valid"] is True
    assert report["rows"] == 3


def test_readme_documents_the_060_contract_surface_and_positioning() -> None:
    readme = (ROOT / "README.md").read_text(encoding="utf-8")

    assert "Cross-column and conditional rules" in readme
    assert "Dataset-level rules" in readme
    assert "pf.check_partitions" in readme
    assert "SLSA" in readme and "SBOM" in readme
    assert "proofframe suggest" in readme
    assert "Pandera" in readme
    assert "Great Expectations" in readme
    assert "Soda" in readme
    assert "Deequ" in readme
