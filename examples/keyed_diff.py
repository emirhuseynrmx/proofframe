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

    print(f"added={report['added_keys']}")
    print(f"removed={report['removed_keys']}")
    print(f"changed={report['changed']}")
    print(f"full change records written to {output.name}")
