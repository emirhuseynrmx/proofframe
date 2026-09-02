"""Check that every foreign key resolves against another dataset."""

import proofframe as pf
import pyarrow as pa

orders = pa.table({"order_id": [1, 2, 3, 4], "customer_id": [10, 99, 20, 99]})
customers = pa.table({"id": [10, 20, 30], "name": ["a", "b", "c"]})

contract = {
    "version": "proofframe.contract.v2",
    "dataset_rules": {
        "references": [
            {
                "name": "orders_customer_fk",
                "columns": ["customer_id"],
                # A logical name, not a path: the caller decides what it resolves to.
                "reference": "customers",
                "reference_columns": ["id"],
            }
        ]
    },
}

report = pf.check(orders, contract, references={"customers": customers})

print(f"valid={report['valid']} violations={report['violation_count']}")
for finding in report["findings"]:
    print(f"  row {finding['row']}: {finding['message']}")

# 99 appears twice but counts once: the violation is the absent key, not each row.
outcome = report["references"][0]
print(f"checked={outcome['checked_distinct_keys']} missing={outcome['missing_distinct_keys']}")
print(f"resolved against {outcome['reference_rows']} rows of {outcome['reference_fingerprint']}")

# A declared reference with no bound dataset is an error, not a skipped rule.
try:
    pf.check(orders, contract)
except pf.ContractError as error:
    print(f"unbound reference: {error.code}")
