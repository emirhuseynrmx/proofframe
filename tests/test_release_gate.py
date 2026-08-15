import copy
import json

import pytest

from benchmarks.release_gate import (
    CASES,
    SCHEMA_VERSION,
    ArtifactError,
    _write_atomic,
    validate_artifact,
    validate_comparison,
)


def _case() -> dict:
    return {
        "samples_ms": [10.0, 11.0, 9.0, 10.5, 9.5],
        "median_ms": 10.0,
        "iqr_ms": 1.0,
        "rows_per_second": 10_000.0,
        "allocations_per_row": 0.0,
        "capacity_growth_events": 0,
        "engine_peak_bytes": 1024,
        "engine_peak_temp_bytes": 2048,
        "process_peak_rss_bytes": 64 * 1024 * 1024,
        "spill_bytes": 2048,
        "correctness": True,
        "fingerprint_version": "v1",
    }


@pytest.fixture
def valid_artifact() -> dict:
    return {
        "schema_version": SCHEMA_VERSION,
        "run_count": 5,
        "warmup_count": 1,
        "fingerprint_version": "v1",
        "dataset": {"sha256": "a" * 64, "rows": 100_000},
        "resources": {"max_memory_bytes": 1 << 30, "max_temp_bytes": 1 << 32},
        "hardware": {
            "system": "Linux",
            "machine": "x86_64",
            "cpu": "Pinned CPU",
            "logical_cpus": 8,
        },
        "compiler": {"rustc": "rustc 1.85.0", "error": None},
        "allocation_contract": {"passed": True},
        "correctness_guards": {"all_cases_passed": True},
        "cases": {name: _case() for name in CASES},
    }


@pytest.mark.parametrize(
    "mutation",
    ["wrong_sha256", "four_runs", "missing_correctness", "mixed_version", "different_cpu"],
)
def test_release_artifact_rejects_incomparable_inputs(valid_artifact, mutation):
    candidate = copy.deepcopy(valid_artifact)
    if mutation == "wrong_sha256":
        candidate["dataset"]["sha256"] = "b" * 64
    elif mutation == "four_runs":
        candidate["run_count"] = 4
        for case in candidate["cases"].values():
            case["samples_ms"] = case["samples_ms"][:4]
    elif mutation == "missing_correctness":
        candidate["correctness_guards"] = {}
    elif mutation == "mixed_version":
        candidate["cases"]["fingerprint"]["fingerprint_version"] = "v2"
    elif mutation == "different_cpu":
        candidate["hardware"]["cpu"] = "Different CPU"

    with pytest.raises(ArtifactError):
        validate_comparison(valid_artifact, candidate)


def test_artifact_requires_every_diagnostic_field(valid_artifact):
    validate_artifact(valid_artifact)
    del valid_artifact["cases"]["numeric_min"]["process_peak_rss_bytes"]
    with pytest.raises(ArtifactError, match="process_peak_rss_bytes"):
        validate_artifact(valid_artifact)


def test_benchmark_artifact_publish_is_atomic(tmp_path, valid_artifact):
    target = tmp_path / "artifact.json"
    _write_atomic(target, valid_artifact)
    assert json.loads(target.read_text(encoding="utf-8"))["schema_version"] == SCHEMA_VERSION
    assert not list(tmp_path.glob(".artifact.json.*.tmp"))
