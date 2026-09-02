"""Validate a pandas DataFrame with a strict Arrow-native contract."""

import pandas as pd
import proofframe as pf

orders = pd.DataFrame(
    {
        "order_id": [101, 102, 102],
        "amount": [12.50, 8.00, -1.00],
        "email": ["ada@example.com", "linus@example.com", None],
    }
)

contract = {
    "version": "proofframe.contract.v1",
    "columns": {
        "order_id": {"required": True, "not_null": True, "unique": True},
        "amount": {"min": 0},
        "email": {"not_null": True},
    },
}

report = pf.check(orders, contract, max_samples=10)

assert report["valid"] is False
assert report["violation_count"] == 3
assert {finding["rule"] for finding in report["findings"]} == {"unique", "min", "not_null"}

print(f"valid={report['valid']} violations={report['violation_count']}")
