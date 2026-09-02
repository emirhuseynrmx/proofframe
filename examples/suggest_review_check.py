"""Generate a draft contract, review it, activate it, and then validate data.

Requires ProofFrame 0.6.0 or newer.
"""

import proofframe as pf
import pyarrow as pa

orders = pa.table({"order_id": [101, 102, 103], "amount": [12.5, 8.0, 10.0]})
draft = pf.suggest_contract(orders, infer_uniqueness=True)

assert draft["status"] == "draft"
assert draft["suggested_from"]["uniqueness_inferred"] is True

try:
    pf.check(orders, draft)
except pf.ContractError as error:
    assert error.code == "PF_DRAFT_CONTRACT"
else:
    raise AssertionError("draft contracts must not run")

# Review and edit inferred rules here before activating this source document.
draft["status"] = "active"
report = pf.check(orders, draft)

assert report["valid"] is True
print(f"valid={report['valid']} rows={report['rows']}")
