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
import subprocess
import sys
import time
from collections.abc import Callable
from importlib.metadata import version
from pathlib import Path
from typing import Any

import proofframe
import psutil
import pyarrow as pa


def build_table(rows: int) -> pa.Table:
    return pa.table(
        {
            "id": pa.array(range(rows), type=pa.int64()),
            "score": pa.array((index / rows for index in range(rows)), type=pa.float64()),
            "event_ts": pa.array(range(rows), type=pa.timestamp("us")),
            "bucket": pa.array((f"b{index % 1000}" for index in range(rows)), type=pa.string()),
        }
    )


def timed(run: Callable[[], Any], *, warmups: int, repeats: int) -> tuple[list[float], int, int]:
    for _ in range(warmups):
        run()
    samples: list[float] = []
    baseline_rss = psutil.Process().memory_info().rss
    peak_rss = baseline_rss
    for _ in range(repeats):
        started = time.perf_counter()
        run()
        samples.append(time.perf_counter() - started)
        peak_rss = max(peak_rss, psutil.Process().memory_info().rss)
    return samples, baseline_rss, peak_rss


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


def summarize(
    case: str, rows: int, samples: list[float], baseline_rss: int, peak_rss: int
) -> dict[str, Any]:
    median = statistics.median(samples)
    return {
        "rules": rules_for(case),
        "samples_seconds": samples,
        "median_seconds": median,
        "rows_per_second": rows / median,
        "baseline_rss_bytes": baseline_rss,
        "peak_rss_bytes": peak_rss,
        "peak_rss_delta_bytes": max(0, peak_rss - baseline_rss),
    }


def run_isolated_case(case: str, rows: int, warmups: int, repeats: int) -> dict[str, Any]:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--case",
        case,
        "--rows",
        str(rows),
        "--warmups",
        str(warmups),
        "--repeats",
        str(repeats),
    ]
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    sampled_peak = 0
    observed = psutil.Process(process.pid)
    while process.poll() is None:
        try:
            sampled_peak = max(sampled_peak, observed.memory_info().rss)
        except psutil.Error:
            pass
        time.sleep(0.005)
    stdout, stderr = process.communicate()
    if process.returncode != 0:
        raise RuntimeError(f"benchmark worker failed ({process.returncode}): {stderr}")
    result = json.loads(stdout)
    result["peak_rss_bytes"] = max(sampled_peak, result["peak_rss_bytes"])
    result["peak_rss_delta_bytes"] = max(
        0, result["peak_rss_bytes"] - result["baseline_rss_bytes"]
    )
    result["rss_method"] = "parent process sampling at 5 ms"
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=1_000_000)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--case", choices=[
        "required_not_null",
        "min_max",
        "unique",
        "full_contract",
        "fingerprint_only",
        "exact_distinct_profile",
    ])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    cases = [
        "required_not_null",
        "min_max",
        "unique",
        "full_contract",
        "fingerprint_only",
        "exact_distinct_profile",
    ]
    if args.case:
        table = build_table(args.rows)
        samples, baseline_rss, peak_rss = timed(
            run_case(args.case, table), warmups=args.warmups, repeats=args.repeats
        )
        print(json.dumps(summarize(args.case, args.rows, samples, baseline_rss, peak_rss)))
        return

    arrow_schema = str(build_table(args.rows).schema)
    results = {}
    for case in cases:
        results[case] = run_isolated_case(case, args.rows, args.warmups, args.repeats)

    payload = {
        "methodology": {
            "rows": args.rows,
            "warmups": args.warmups,
            "repeats": args.repeats,
            "timing_scope": "each case runs in an isolated subprocess; setup/import/table construction excluded from timed section",
            "memory_scope": "baseline RSS is measured after table construction; peak_rss_delta_bytes is peak minus baseline within that subprocess",
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
        "arrow_schema": arrow_schema,
        "results": results,
    }
    rendered = json.dumps(payload, indent=2)
    print(rendered)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
