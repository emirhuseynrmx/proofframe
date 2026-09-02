"""Compare two versions of a dataset by business key."""

from pathlib import Path
from tempfile import TemporaryDirectory

import proofframe as pf
import pyarrow as pa

before = pa.table({"order_id": [101, 102, 103], "status": ["paid", "paid", "open"]})
after = pa.table({"order_id": [101, 102, 104], "status": ["paid", "refunded", "open"]})

with TemporaryDirectory() as directory:
    output = Path(directory) / "changes.jsonl"
    report = pf.diff(before, after, keys="order_id", output=str(output), max_samples=10)

    assert output.is_file()
    assert report["added_keys"] == ["104"]
    assert report["removed_keys"] == ["103"]
    assert report["changed"] == [{"key": "102", "columns": ["status"]}]

print("added=1 removed=1 changed=1")
