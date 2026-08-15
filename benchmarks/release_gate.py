"""Reproducible ProofFrame 0.5 release benchmark and artifact validator.

The timed worker owns data preparation, warmup, correctness checks, and repeated
measurements. The parent samples worker RSS out-of-process so the GIL cannot hide
the peak. Artifacts are comparable only when dataset, CPU, compiler, algorithm,
and run-count contracts match.
"""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import UTC, datetime
from importlib.metadata import version
from pathlib import Path
from typing import Any

import numpy as np
import proofframe
import psutil
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.csv as arrow_csv
from pyarrow import parquet

SCHEMA_VERSION = "proofframe.release-benchmark.v1"
CASES = ("numeric_min", "timestamp_unique", "full_contract", "fingerprint")
PERF_EVENTS = (
    "cycles",
    "instructions",
    "cache-misses",
    "branch-misses",
    "stalled-cycles-frontend",
    "stalled-cycles-backend",
)
SYNTHETIC_SCHEMA = {
    "generator": "proofframe-bitcoin-shaped-v1",
    "columns": ["timestamp", "open", "high", "low", "close", "volume"],
}


class ArtifactError(ValueError):
    """A benchmark artifact cannot support the claimed comparison."""


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _synthetic_sha256(rows: int) -> str:
    payload = json.dumps(
        {**SYNTHETIC_SCHEMA, "rows": rows}, sort_keys=True, separators=(",", ":")
    )
    return hashlib.sha256(payload.encode()).hexdigest()


def _build_synthetic(rows: int) -> pa.Table:
    ordinal = np.arange(rows, dtype=np.int64)
    price = ordinal.astype(np.float64)
    price /= max(rows, 1)
    timestamp = pa.array(ordinal).cast(pa.timestamp("s"))
    base = pa.array(price)
    return pa.table(
        {
            "timestamp": timestamp,
            "open": base,
            "high": pc.add(base, 0.01),
            "low": base,
            "close": base,
            "volume": pc.add(base, 1.0),
        }
    )


def _load_dataset(path: Path, rows: int | None) -> pa.Table:
    if path.suffix.lower() == ".csv":
        table = arrow_csv.read_csv(path)
    elif path.suffix.lower() in {".parquet", ".pq"}:
        table = parquet.read_table(path)
    else:
        raise ValueError(f"Unsupported benchmark dataset: {path}")
    normalized_names = [name.strip().lower() for name in table.column_names]
    table = table.rename_columns(normalized_names)
    required = ["timestamp", "open", "high", "low", "close", "volume"]
    missing = set(required) - set(table.column_names)
    if missing:
        raise ValueError(f"Benchmark dataset is missing columns: {sorted(missing)}")
    table = table.select(required)
    timestamp = table["timestamp"]
    if not pa.types.is_timestamp(timestamp.type):
        timestamp = pc.cast(pc.cast(timestamp, pa.int64(), safe=False), pa.timestamp("s"))
    arrays: dict[str, pa.Array | pa.ChunkedArray] = {"timestamp": timestamp}
    for name in required[1:]:
        arrays[name] = pc.cast(table[name], pa.float64(), safe=False)
    normalized = pa.table(arrays)
    return normalized if rows is None else normalized.slice(0, min(rows, normalized.num_rows))


def _contract(case: str) -> dict[str, Any]:
    numeric = {name: {"min": 0} for name in ("open", "high", "low", "close", "volume")}
    if case == "numeric_min":
        return {"columns": numeric}
    if case == "timestamp_unique":
        return {"columns": {"timestamp": {"unique": True}}}
    if case == "full_contract":
        return {
            "columns": {
                "timestamp": {"required": True, "not_null": True, "unique": True},
                **{
                    name: {"required": True, "not_null": True, "min": 0}
                    for name in ("open", "high", "low", "close", "volume")
                },
            },
            "max_findings": 100,
        }
    raise ValueError(f"Unknown contract case: {case}")


def _execute_case(
    case: str,
    table: pa.Table,
    *,
    fingerprint_version: str,
    max_memory: int,
    max_temp: int,
) -> tuple[dict[str, Any] | None, str | None]:
    if case == "fingerprint":
        digest = proofframe.fingerprint(table, version=fingerprint_version)
        expected_prefix = f"pf-fp-{fingerprint_version}:"
        if not digest.startswith(expected_prefix):
            raise RuntimeError(f"fingerprint correctness guard failed: {digest}")
        return None, digest
    report = proofframe.check(
        table,
        _contract(case),
        max_memory=max_memory,
        max_temp=max_temp,
        max_samples=100,
    )
    if not report["valid"] or report["rows"] != table.num_rows:
        raise RuntimeError(f"{case} correctness guard failed: {report}")
    return report, None


def _os_peak_rss() -> int:
    memory = psutil.Process().memory_info()
    return int(getattr(memory, "peak_wset", memory.rss))


def _worker(args: argparse.Namespace) -> None:
    table = _load_dataset(args.dataset, args.rows) if args.dataset else _build_synthetic(args.rows)
    for _ in range(args.warmups):
        _execute_case(
            args.worker_case,
            table,
            fingerprint_version=args.fingerprint_version,
            max_memory=args.max_memory,
            max_temp=args.max_temp,
        )

    samples_ms: list[float] = []
    last_report: dict[str, Any] | None = None
    last_digest: str | None = None
    for _ in range(args.runs):
        gc.collect()
        started = time.perf_counter_ns()
        last_report, last_digest = _execute_case(
            args.worker_case,
            table,
            fingerprint_version=args.fingerprint_version,
            max_memory=args.max_memory,
            max_temp=args.max_temp,
        )
        elapsed = time.perf_counter_ns() - started
        samples_ms.append(elapsed / 1_000_000)

    metrics = (last_report or {}).get("metrics", {})
    print(
        json.dumps(
            {
                "case": args.worker_case,
                "rows": table.num_rows,
                "samples_ms": samples_ms,
                "correctness": True,
                "fingerprint": last_digest,
                "engine_peak_bytes": int(metrics.get("peak_memory_bytes", 0)),
                "engine_peak_temp_bytes": int(metrics.get("peak_temp_bytes", 0)),
                "spill_bytes": int(metrics.get("spill_bytes", 0)),
                "capacity_growth_events": int(metrics.get("capacity_growth_events", 0)),
                "os_reported_peak_rss_bytes": _os_peak_rss(),
            },
            separators=(",", ":"),
        )
    )


def _worker_command(args: argparse.Namespace, case: str, *, runs: int | None = None) -> list[str]:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--worker-case",
        case,
        "--rows",
        str(args.rows),
        "--runs",
        str(args.runs if runs is None else runs),
        "--warmups",
        str(args.warmups),
        "--fingerprint-version",
        args.fingerprint_version,
        "--max-memory",
        str(args.max_memory),
        "--max-temp",
        str(args.max_temp),
    ]
    if args.dataset:
        command.extend(("--dataset", str(args.dataset)))
    return command


def _run_worker(args: argparse.Namespace, case: str) -> dict[str, Any]:
    process = subprocess.Popen(
        _worker_command(args, case),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={**os.environ, "PYTHONHASHSEED": "0"},
    )
    observed_peak = 0
    sampler = psutil.Process(process.pid)
    while process.poll() is None:
        try:
            observed_peak = max(observed_peak, sampler.memory_info().rss)
        except psutil.Error:
            pass
        time.sleep(args.rss_interval)
    stdout, stderr = process.communicate()
    if process.returncode != 0:
        raise RuntimeError(f"benchmark worker {case} failed ({process.returncode}):\n{stderr}")
    result = json.loads(stdout)
    result["process_peak_rss_bytes"] = max(
        observed_peak, int(result["os_reported_peak_rss_bytes"])
    )
    result["rss_sample_interval_seconds"] = args.rss_interval
    return result


def _perf_counters(args: argparse.Namespace, case: str) -> dict[str, Any]:
    perf = shutil.which("perf")
    empty = {event: None for event in PERF_EVENTS}
    if not args.perf:
        return {**empty, "unavailable_reason": "perf collection not requested"}
    if perf is None or platform.system() != "Linux":
        return {**empty, "unavailable_reason": "Linux perf executable is unavailable"}
    command = [
        perf,
        "stat",
        "-x",
        ";",
        "-e",
        ",".join(PERF_EVENTS),
        "--",
        *_worker_command(args, case, runs=1),
    ]
    completed = subprocess.run(command, capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        return {**empty, "unavailable_reason": completed.stderr.strip()[:1000]}
    counters: dict[str, Any] = {**empty, "unavailable_reason": None}
    for line in completed.stderr.splitlines():
        fields = line.split(";")
        if len(fields) >= 3 and fields[2] in counters:
            raw = fields[0].strip().replace(",", "")
            counters[fields[2]] = int(raw) if raw.isdigit() else None
    return counters


def _iqr(samples: list[float]) -> float:
    quartiles = statistics.quantiles(samples, n=4, method="inclusive")
    return quartiles[2] - quartiles[0]


def _allocation_contract() -> dict[str, Any]:
    cargo = shutil.which("cargo")
    if cargo is None:
        return {"passed": False, "command": None, "error": "cargo is unavailable"}
    command = [cargo, "test", "--release", "--locked", "--test", "allocation_contract"]
    completed = subprocess.run(command, capture_output=True, text=True, check=False)
    diagnostic = "\n".join(
        output for output in (completed.stdout.strip(), completed.stderr.strip()) if output
    )
    return {
        "passed": completed.returncode == 0,
        "command": " ".join(command),
        "error": (
            None
            if completed.returncode == 0
            else (diagnostic[-4000:] or f"cargo test exited with {completed.returncode}")
        ),
    }


def _hardware() -> dict[str, Any]:
    return {
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "cpu": platform.processor() or "unknown",
        "logical_cpus": os.cpu_count(),
        "physical_memory_bytes": psutil.virtual_memory().total,
    }


def _compiler() -> dict[str, Any]:
    rustc = shutil.which("rustc")
    if rustc is None:
        return {"rustc": None, "error": "rustc is unavailable"}
    completed = subprocess.run([rustc, "-Vv"], capture_output=True, text=True, check=False)
    return {
        "rustc": completed.stdout.strip() if completed.returncode == 0 else None,
        "error": completed.stderr.strip() or None,
    }


def _dataset_identity(args: argparse.Namespace) -> dict[str, Any]:
    if args.dataset:
        identity = {
            "kind": "file",
            "path": str(args.dataset),
            "sha256": _sha256_file(args.dataset),
            "rows": args.rows,
        }
        if args.dataset_manifest:
            manifest = json.loads(args.dataset_manifest.read_text(encoding="utf-8"))
            if identity["sha256"] != manifest["source_sha256"]:
                raise ArtifactError("dataset SHA-256 does not match the pinned manifest")
            identity["manifest"] = str(args.dataset_manifest)
        return identity
    return {
        "kind": "synthetic",
        "sha256": _synthetic_sha256(args.rows),
        "rows": args.rows,
        **SYNTHETIC_SCHEMA,
    }


def _summarize_case(
    result: dict[str, Any],
    *,
    allocation_passed: bool,
    fingerprint_version: str,
    perf: dict[str, Any],
) -> dict[str, Any]:
    samples = [float(sample) for sample in result["samples_ms"]]
    median_ms = statistics.median(samples)
    allocation_proven = result["case"] in {"numeric_min", "fingerprint"}
    return {
        "samples_ms": samples,
        "median_ms": median_ms,
        "iqr_ms": _iqr(samples),
        "rows_per_second": result["rows"] / (median_ms / 1000),
        "allocations_per_row": 0.0 if allocation_passed and allocation_proven else None,
        "allocation_evidence": (
            "tests/allocation_contract.rs" if allocation_proven else "not instrumented for this case"
        ),
        "capacity_growth_events": result["capacity_growth_events"],
        "engine_peak_bytes": result["engine_peak_bytes"],
        "engine_peak_temp_bytes": result["engine_peak_temp_bytes"],
        "process_peak_rss_bytes": result["process_peak_rss_bytes"],
        "spill_bytes": result["spill_bytes"],
        "rss_sample_interval_seconds": result["rss_sample_interval_seconds"],
        "correctness": result["correctness"],
        "fingerprint_version": fingerprint_version,
        "perf": perf,
    }


def validate_artifact(artifact: dict[str, Any]) -> None:
    if artifact.get("schema_version") != SCHEMA_VERSION:
        raise ArtifactError("unsupported benchmark artifact schema")
    if not isinstance(artifact.get("run_count"), int) or artifact["run_count"] < 5:
        raise ArtifactError("release artifacts require at least five measured runs")
    dataset_sha = artifact.get("dataset", {}).get("sha256", "")
    if len(dataset_sha) != 64 or any(character not in "0123456789abcdef" for character in dataset_sha):
        raise ArtifactError("dataset SHA-256 is absent or malformed")
    if not artifact.get("correctness_guards", {}).get("all_cases_passed"):
        raise ArtifactError("correctness guards are absent or failed")
    allocation_contract = artifact.get("allocation_contract", {})
    if not allocation_contract.get("passed"):
        reason = allocation_contract.get("error") or "no diagnostic was captured"
        raise ArtifactError(f"allocation contract did not pass: {reason}")
    cases = artifact.get("cases", {})
    if set(cases) != set(CASES):
        raise ArtifactError("artifact case matrix is incomplete")
    versions = {case.get("fingerprint_version") for case in cases.values()}
    if versions != {artifact.get("fingerprint_version")}:
        raise ArtifactError("artifact mixes fingerprint versions")
    for name, case in cases.items():
        if len(case.get("samples_ms", [])) != artifact["run_count"]:
            raise ArtifactError(f"{name} does not contain the declared sample count")
        if not case.get("correctness"):
            raise ArtifactError(f"{name} correctness guard failed")
        for field in (
            "median_ms",
            "iqr_ms",
            "rows_per_second",
            "allocations_per_row",
            "capacity_growth_events",
            "engine_peak_bytes",
            "process_peak_rss_bytes",
            "spill_bytes",
        ):
            if field not in case:
                raise ArtifactError(f"{name} is missing {field}")


def validate_comparison(baseline: dict[str, Any], candidate: dict[str, Any]) -> None:
    validate_artifact(baseline)
    validate_artifact(candidate)
    if baseline["dataset"]["sha256"] != candidate["dataset"]["sha256"]:
        raise ArtifactError("dataset SHA-256 differs")
    for field in ("system", "machine", "cpu", "logical_cpus"):
        if baseline["hardware"].get(field) != candidate["hardware"].get(field):
            raise ArtifactError(f"hardware field differs: {field}")
    if baseline["compiler"].get("rustc") != candidate["compiler"].get("rustc"):
        raise ArtifactError("Rust compiler differs")
    if baseline["fingerprint_version"] != candidate["fingerprint_version"]:
        raise ArtifactError("fingerprint version differs")


def enforce_release_gates(artifact: dict[str, Any], baseline: dict[str, Any] | None) -> None:
    validate_artifact(artifact)
    for name, case in artifact["cases"].items():
        if case["engine_peak_bytes"] > artifact["resources"]["max_memory_bytes"]:
            raise ArtifactError(f"{name} exceeded the engine memory budget")
        if case["engine_peak_temp_bytes"] > artifact["resources"]["max_temp_bytes"]:
            raise ArtifactError(f"{name} exceeded the engine temp budget")
    if artifact["cases"]["fingerprint"]["allocations_per_row"] != 0:
        raise ArtifactError("fingerprint allocation gate failed")
    if artifact["cases"]["numeric_min"]["allocations_per_row"] != 0:
        raise ArtifactError("numeric kernel allocation gate failed")
    if baseline is None:
        return
    validate_comparison(baseline, artifact)
    current = artifact["cases"]
    previous = baseline["cases"]
    if current["fingerprint"]["median_ms"] > previous["fingerprint"]["median_ms"] / 3:
        raise ArtifactError("fingerprint did not reach the 3x release gate")
    if current["timestamp_unique"]["median_ms"] > previous["timestamp_unique"]["median_ms"] / 2:
        raise ArtifactError("timestamp unique did not reach the 2x release gate")
    if current["numeric_min"]["median_ms"] > previous["numeric_min"]["median_ms"] * 1.10:
        raise ArtifactError("numeric min regressed by more than 10%")


def _write_atomic(path: Path, artifact: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    try:
        with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as output:
            json.dump(artifact, output, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=100_000)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--dataset", type=Path)
    parser.add_argument("--dataset-manifest", type=Path)
    parser.add_argument("--fingerprint-version", choices=("v1", "v2"), default="v1")
    parser.add_argument("--max-memory", type=int, default=512 * 1024 * 1024)
    parser.add_argument("--max-temp", type=int, default=4 * 1024 * 1024 * 1024)
    parser.add_argument("--rss-interval", type=float, default=0.005)
    parser.add_argument("--perf", action="store_true")
    parser.add_argument("--worker-case", choices=CASES, help=argparse.SUPPRESS)
    return parser


def main() -> None:
    args = _parser().parse_args()
    if args.worker_case:
        _worker(args)
        return
    if args.runs < 5:
        raise ArtifactError("release benchmark requires --runs >= 5")
    if args.rows <= 0 or args.warmups < 0 or args.rss_interval <= 0:
        raise ArtifactError("rows and RSS interval must be positive; warmups cannot be negative")

    allocation = _allocation_contract()
    raw_results = {case: _run_worker(args, case) for case in CASES}
    cases = {
        case: _summarize_case(
            raw_results[case],
            allocation_passed=allocation["passed"],
            fingerprint_version=args.fingerprint_version,
            perf=_perf_counters(args, case),
        )
        for case in CASES
    }
    artifact = {
        "schema_version": SCHEMA_VERSION,
        "created_at": datetime.now(UTC).isoformat(),
        "run_count": args.runs,
        "warmup_count": args.warmups,
        "fingerprint_version": args.fingerprint_version,
        "dataset": _dataset_identity(args),
        "resources": {
            "max_memory_bytes": args.max_memory,
            "max_temp_bytes": args.max_temp,
        },
        "hardware": _hardware(),
        "compiler": _compiler(),
        "runtime": {
            "python": platform.python_version(),
            "proofframe": version("proofframe"),
            "pyarrow": version("pyarrow"),
            "numpy": version("numpy"),
        },
        "allocation_contract": allocation,
        "correctness_guards": {"all_cases_passed": all(case["correctness"] for case in cases.values())},
        "cases": cases,
    }
    baseline = json.loads(args.baseline.read_text(encoding="utf-8")) if args.baseline else None
    enforce_release_gates(artifact, baseline)
    rendered = json.dumps(artifact, indent=2, sort_keys=True)
    print(rendered)
    if args.output:
        _write_atomic(args.output, artifact)


if __name__ == "__main__":
    main()
