"""Generate three local Data Review examples from synthetic orders, without credentials."""

import argparse
from pathlib import Path

import proofframe as pf
import pyarrow as pa


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=Path("review-demo"))
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    contract = {
        "version": "proofframe.contract.v2",
        "columns": {"order_id": {"unique": True}, "amount_cents": {"min": 0}},
    }
    invalid = pa.table({"order_id": [101, 102, 102, 104], "amount_cents": [1200, -350, 9900, -45]})
    valid = pa.table({"order_id": [101, 102, 103, 104], "amount_cents": [1200, 350, 9900, 45]})
    for name, table, samples in [
        ("private", invalid, 0),
        ("details", invalid, 20),
        ("valid", valid, 20),
    ]:
        result = pf.review(
            table,
            contract,
            args.out / name,
            max_samples=samples,
            label="Synthetic orders · demo fixture",
        )
        print(result)


if __name__ == "__main__":
    main()
