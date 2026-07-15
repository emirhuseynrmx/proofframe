"""Rule-by-rule ProofFrame benchmark harness.

This is intentionally not part of normal CI. It records enough metadata to make
local performance results auditable instead of relying on one headline number:
raw repeat durations, rows/second, peak RSS, rule set, Arrow schema, and package
versions.
"""

from __future__ import annotations

import argparse
import json
import platform
import statistics
import threading
import time
from collections.abc import Callable
from importlib.metadata import version
from pathlib import Path
from typing import Any

import psutil
import pyarrow as pa

import proofframe


def build_table(rows: int) -> pa.Table:
    return pa.table(
        {
            "id": pa.array(range(rows), type=pa.int64()),
            "score": pa.array((index / rows for index in range(rows)), type=pa.float64()),
            "event_ts": pa.array(range(rows), type=pa.timestamp("us")),
            "bucket": pa.array((f"b{index % 1000}" for index in range(rows)), type=pa.string()),
        }
    )


def monitor_peak_rss(stop: threading.Event, peak: list[int], interval: float = 0.01) -> None:
    process = psutil.Process()
    while not stop.is_set():
        peak[0] = max(peak[0], process.memory_info().rss)
        time.sleep(interval)
    peak[0] = max(peak[0], process.memory_info().rss)


def timed(run: Callable[[], Any], *, warmups: int, repeats: int) -> tuple[list[float], int]:
    for _ in range(warmups):
        run()
    samples: list[float] = []
    peak_rss = 0
    for _ in range(repeats):
        stop = threading.Event()
        peak = [psutil.Process().memory_info().rss]
        thread = threading.Thread(target=monitor_peak_rss, args=(stop, peak), daemon=True)
        thread.start()
        started = time.perf_counter()
        run()
        samples.append(time.perf_counter() - started)
        stop.set()
        thread.join()
        peak_rss = max(peak_rss, peak[0])
    return samples, peak_rss


def contract_for(case: str) -> dict[str, Any]:
    if case == "required_not_null":
        return {"columns": {"id": {"required": True, "not_null": True}}}
    if case == "min_max":
        return {"columns": {"score": {"min": 0, "max": 1}}}
    if case == "unique":
        return {"columns": {"id": {"unique": True}}}
    if case == "full_contract":
        return {
            "columns": {
                "id": {"required": True, "not_null": True, "unique": True},
                "score": {"min": 0, "max": 1},
                "bucket": {"not_null": True},
            }
        }
    raise ValueError(f"Unknown validation case: {case}")


def rules_for(case: str) -> list[str]:
    return {
        "required_not_null": ["id required", "id not_null"],
        "min_max": ["0 <= score <= 1"],
        "unique": ["id unique"],
        "full_contract": ["id required", "id not_null", "id unique", "0 <= score <= 1", "bucket not_null"],
        "fingerprint_only": ["schema", "null/value boundaries", "ordered canonical data", "BLAKE3"],
        "exact_distinct_profile": ["profile", "exact distinct", "min/max", "fingerprint"],
    }[case]


def run_case(case: str, table: pa.Table) -> Callable[[], Any]:
    if case in {"required_not_null", "min_max", "unique", "full_contract"}:
        contract = contract_for(case)

        def validate() -> Any:
            report = proofframe.validate(table, contract, include_profile=False)
            if not report["valid"]:
                raise RuntimeError(f"ProofFrame rejected valid benchmark data for {case}")
            return report

        return validate

    if case == "fingerprint_only":

        def fingerprint() -> Any:
            return proofframe.fingerprint(table)

        return fingerprint

    if case == "exact_distinct_profile":

        def profile() -> Any:
            return proofframe.profile(table, distinct="exact")

        return profile

    raise ValueError(f"Unknown benchmark case: {case}")


def summarize(case: str, rows: int, samples: list[float], peak_rss: int) -> dict[str, Any]:
    median = statistics.median(samples)
    return {
        "rules": rules_for(case),
        "samples_seconds": samples,
        "median_seconds": median,
        "rows_per_second": rows / median,
        "peak_rss_bytes": peak_rss,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=1_000_000)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    table = build_table(args.rows)
    cases = [
        "required_not_null",
        "min_max",
        "unique",
        "full_contract",
        "fingerprint_only",
        "exact_distinct_profile",
    ]
    results = {}
    for case in cases:
        samples, peak_rss = timed(run_case(case, table), warmups=args.warmups, repeats=args.repeats)
        results[case] = summarize(case, args.rows, samples, peak_rss)

    payload = {
        "methodology": {
            "rows": args.rows,
            "warmups": args.warmups,
            "repeats": args.repeats,
            "timing_scope": "in-memory Arrow table; setup/import/table construction excluded",
            "cases": cases,
        },
        "environment": {
            "python": platform.python_version(),
            "platform": platform.platform(),
            "versions": {
                "proofframe": version("proofframe"),
                "pyarrow": version("pyarrow"),
                "psutil": version("psutil"),
            },
        },
        "arrow_schema": str(table.schema),
        "results": results,
    }
    rendered = json.dumps(payload, indent=2)
    print(rendered)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
