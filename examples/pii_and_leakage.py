"""Scan for PII without exposing values and detect train/test overlap."""

import json

import proofframe as pf
import pyarrow as pa

customers = pa.table({"customer_id": [1, 2], "contact": ["private.person@example.com", "safe"]})
pii = pf.scan_pii(customers)

assert pii["detected"] is True
assert pii["counts_by_kind"] == {"email": 1}
assert "private.person@example.com" not in json.dumps(pii)

train = pa.table({"customer_id": [1, 2], "feature": ["a", "b"]})
test = pa.table({"customer_id": [2, 3], "feature": ["other", "c"]})
leakage = pf.detect_leakage(train, test, keys="customer_id")

assert leakage["mode"] == "key"
assert leakage["overlap_count"] == 1
assert leakage["sample_fingerprints"][0] != "2"

print(f"pii={pii['counts_by_kind']} overlap={leakage['overlap_count']}")
