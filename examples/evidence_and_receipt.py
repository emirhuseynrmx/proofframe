"""Create evidence in the validation pass, then sign and verify its receipt."""

import proofframe as pf
import pyarrow as pa

orders = pa.table({"order_id": [101, 102], "amount": [12.5, 8.0]})
contract = {
    "version": "proofframe.contract.v1",
    "columns": {"order_id": {"required": True, "unique": True}, "amount": {"min": 0}},
}

checked = pf.check_with_evidence(orders, contract)
keys = pf.generate_keypair()
receipt = pf.sign_evidence(checked["evidence"], private_key=keys["private_key"])
verification = pf.verify_receipt(receipt, expected_public_key=keys["public_key"])

print(f"valid={checked['report']['valid']}")
print(f"evidence={checked['evidence']['schema']}")
print(f"receipt_valid={verification['valid']}")
