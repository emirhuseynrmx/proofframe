from __future__ import annotations

import argparse
import time

import pyarrow as pa

import proofframe


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=1_000_000)
    args = parser.parse_args()

    table = pa.table(
        {
            "id": pa.array(range(args.rows), type=pa.int64()),
            "score": pa.array((index / args.rows for index in range(args.rows)), type=pa.float64()),
            "bucket": pa.array((f"b{index % 100}" for index in range(args.rows))),
        }
    )
    started = time.perf_counter()
    report = proofframe.profile(table)
    elapsed = time.perf_counter() - started
    print(f"rows={report['rows']:,}")
    print(f"seconds={elapsed:.3f}")
    print(f"rows_per_second={args.rows / elapsed:,.0f}")
    print(f"fingerprint={report['fingerprint']}")


if __name__ == "__main__":
    main()
