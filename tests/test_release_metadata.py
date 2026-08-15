from pathlib import Path

import pytest

from scripts.verify_release_version import read_versions, verify_versions

ROOT = Path(__file__).resolve().parents[1]


def test_rust_and_python_versions_match_release() -> None:
    assert read_versions(ROOT) == ("0.5.0", "0.5.0")
    verify_versions(ROOT, "v0.5.0")


@pytest.mark.parametrize("tag", ["0.5.0", "v0.5.1", "v0.5.0-rc.1"])
def test_release_tag_must_match_exact_pep440_and_semver_version(tag: str) -> None:
    with pytest.raises(ValueError):
        verify_versions(ROOT, tag)


def test_publish_is_gated_by_exact_tag_commit_ci_evidence() -> None:
    ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    publish = (ROOT / ".github/workflows/publish.yml").read_text(encoding="utf-8")

    assert "cargo publish --dry-run --locked" in ci
    assert "release-evidence-${{ github.sha }}" in ci
    assert "uses: ./.github/workflows/publish.yml" not in ci
    assert "workflow_run:" in publish
    assert 'workflows: ["CI"]' in publish
    assert "github.event.workflow_run.conclusion == 'success'" in publish
    assert "release-evidence-${{ github.event.workflow_run.head_sha }}" in publish
    assert "run-id: ${{ github.event.workflow_run.id }}" in publish
    assert "github-token: ${{ secrets.GITHUB_TOKEN }}" in publish
    assert "ref: ${{ github.event.workflow_run.head_sha }}" in publish
    assert "ref: ${{ needs.gate.outputs.sha }}" in publish


def test_ci_installs_built_wheels_without_assuming_an_activated_virtualenv() -> None:
    workflows = (ROOT / ".github/workflows").glob("*.yml")
    for workflow in workflows:
        source = workflow.read_text(encoding="utf-8")
        assert "maturin develop" not in source, workflow.name


def test_ci_installs_dependencies_needed_during_full_test_collection() -> None:
    ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

    assert "pytest pyarrow pandas polars psutil" in ci
    assert "pytest-cov ruff twine pyarrow pandas polars psutil" in ci


def test_release_gate_remains_compatible_with_python_310() -> None:
    source = (ROOT / "benchmarks/release_gate.py").read_text(encoding="utf-8")

    assert "from datetime import UTC" not in source
    assert "datetime.now(UTC)" not in source
    assert "datetime.now(timezone.utc)" in source
