import proofframe as pf
import pyarrow as pa


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
