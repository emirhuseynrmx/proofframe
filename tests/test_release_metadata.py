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
    assert "uses: ./.github/workflows/publish.yml" in ci
    assert "workflow_call:" in publish
    assert "release-evidence-${{ inputs.sha }}" in publish
    assert "ref: ${{ inputs.sha }}" in publish
    assert "ref: ${{ needs.gate.outputs.sha }}" in publish


def test_ci_installs_built_wheels_without_assuming_an_activated_virtualenv() -> None:
    workflows = (ROOT / ".github/workflows").glob("*.yml")
    for workflow in workflows:
        source = workflow.read_text(encoding="utf-8")
        assert "maturin develop" not in source, workflow.name
