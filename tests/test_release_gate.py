import copy
import json

import pytest

from benchmarks.release_gate import (
    CASES,
    SCHEMA_VERSION,
    ArtifactError,
    _contract,
    _write_atomic,
    validate_artifact,
    validate_comparison,
)


def _case() -> dict:
    return {
        "samples_ms": [10.0, 11.0, 9.0, 10.5, 9.5, 10.25, 9.75],
        "native_samples_ms": [9.7, 10.6, 8.7, 10.1, 9.2, 9.9, 9.4],
        "median_ms": 10.0,
        "native_median_ms": 9.7,
        "iqr_ms": 1.0,
        "rows_per_second": 10_000.0,
        "native_rows_per_second": 10_310.0,
        "python_native_throughput_ratio": 0.97,
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
        "schema_version": "proofframe.release-benchmark.v3",
        "run_count": 7,
        "warmup_count": 1,
        "fingerprint_version": "v1",
        "dataset": {"sha256": "a" * 64, "rows": 1_000_000},
        "resources": {"max_memory_bytes": 1 << 30, "max_temp_bytes": 1 << 32},
        "hardware": {
            "system": "Linux",
            "machine": "x86_64",
            "cpu": "Pinned CPU",
            "logical_cpus": 8,
        },
        "compiler": {"rustc": "rustc 1.85.0", "error": None},
        "allocation_contract": {"passed": True},
        "native_scan_diagnostic": {
            "included": True,
            "comparison_scope": "installed_api_vs_compiled_scan",
        },
        "correctness_guards": {"all_cases_passed": True},
        "cases": {name: _case() for name in CASES},
    }


def test_v3_artifact_covers_relational_dataset_spill_and_v1_regression() -> None:
    assert SCHEMA_VERSION == "proofframe.release-benchmark.v3"
    assert set(CASES) >= {
        "v1_numeric_min",
        "relational_compare",
        "conditional_assertion",
        "dataset_ratios",
        "composite_memory",
        "composite_spill",
        "fingerprint",
    }

    assert _contract("v1_numeric_min")["version"] == "proofframe.contract.v1"


@pytest.mark.parametrize(
    "mutation",
    ["wrong_sha256", "six_runs", "missing_correctness", "mixed_version", "different_cpu"],
)
def test_release_artifact_rejects_incomparable_inputs(valid_artifact, mutation):
    candidate = copy.deepcopy(valid_artifact)
    if mutation == "wrong_sha256":
        candidate["dataset"]["sha256"] = "b" * 64
    elif mutation == "six_runs":
        candidate["run_count"] = 6
        for case in candidate["cases"].values():
            case["samples_ms"] = case["samples_ms"][:6]
            case["native_samples_ms"] = case["native_samples_ms"][:6]
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
    del valid_artifact["cases"]["v1_numeric_min"]["process_peak_rss_bytes"]
    with pytest.raises(ArtifactError, match="process_peak_rss_bytes"):
        validate_artifact(valid_artifact)


def test_artifact_requires_native_python_parity_measurements(valid_artifact):
    del valid_artifact["cases"]["relational_compare"]["python_native_throughput_ratio"]

    with pytest.raises(ArtifactError, match="python_native_throughput_ratio"):
        validate_artifact(valid_artifact)


def test_artifact_declares_the_native_scan_diagnostic_scope(valid_artifact):
    del valid_artifact["native_scan_diagnostic"]

    with pytest.raises(ArtifactError, match="native_scan_diagnostic"):
        validate_artifact(valid_artifact)


def test_release_gates_reject_v1_regression_and_allocation_growth(valid_artifact):
    from benchmarks.release_gate import enforce_release_gates

    baseline = copy.deepcopy(valid_artifact)

    regressed = copy.deepcopy(valid_artifact)
    regressed["cases"]["v1_numeric_min"]["median_ms"] = 10.31
    with pytest.raises(ArtifactError, match="V1.*3%"):
        enforce_release_gates(regressed, baseline)

    allocation_growth = copy.deepcopy(valid_artifact)
    allocation_growth["cases"]["relational_compare"]["capacity_growth_events"] = 1
    with pytest.raises(ArtifactError, match="capacity growth"):
        enforce_release_gates(allocation_growth, None)

    installed_overhead = copy.deepcopy(valid_artifact)
    installed_overhead["cases"]["conditional_assertion"]["python_native_throughput_ratio"] = 0.8
    enforce_release_gates(installed_overhead, None)


def test_allocation_contract_failure_preserves_the_root_cause(valid_artifact):
    valid_artifact["allocation_contract"] = {
        "passed": False,
        "command": None,
        "error": "cargo is unavailable",
    }
    with pytest.raises(ArtifactError, match="cargo is unavailable"):
        validate_artifact(valid_artifact)


def test_benchmark_artifact_publish_is_atomic(tmp_path, valid_artifact):
    target = tmp_path / "artifact.json"
    _write_atomic(target, valid_artifact)
    assert json.loads(target.read_text(encoding="utf-8"))["schema_version"] == SCHEMA_VERSION
    assert not list(tmp_path.glob(".artifact.json.*.tmp"))
