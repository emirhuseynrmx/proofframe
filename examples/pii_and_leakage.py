"""Scan for PII without exposing values and detect train/test overlap."""

import json

import proofframe as pf
import pyarrow as pa

customers = pa.table({"customer_id": [1, 2], "contact": ["private.person@example.com", "safe"]})
pii = pf.scan_pii(customers)

print(f"detected={pii['detected']} kinds={pii['counts_by_kind']}")
# Findings carry a keyed fingerprint, never the matched value itself.
print(f"report contains the address: {'private.person@example.com' in json.dumps(pii)}")

train = pa.table({"customer_id": [1, 2], "feature": ["a", "b"]})
test = pa.table({"customer_id": [2, 3], "feature": ["other", "c"]})
leakage = pf.detect_leakage(train, test, keys="customer_id")

print(f"mode={leakage['mode']} overlap={leakage['overlap_count']}")
print(f"overlapping keys are reported as fingerprints: {leakage['sample_fingerprints']}")
