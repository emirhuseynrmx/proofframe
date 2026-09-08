from pathlib import Path

import pytest

from scripts.verify_release_version import read_versions, verify_versions

ROOT = Path(__file__).resolve().parents[1]


def test_rust_and_python_versions_match_release() -> None:
    assert read_versions(ROOT) == ("0.7.0", "0.7.0")
    verify_versions(ROOT, "v0.7.0")


def test_python_package_is_classified_as_stable() -> None:
    metadata = (ROOT / "pyproject.toml").read_text(encoding="utf-8")

    assert '"Development Status :: 5 - Production/Stable"' in metadata
    assert '"Development Status :: 4 - Beta"' not in metadata


def test_license_contains_the_complete_apache_2_text() -> None:
    license_text = (ROOT / "LICENSE").read_text(encoding="utf-8")

    assert license_text.startswith("Copyright 2026 Emir Huseyin Inci\n\n")
    assert "TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION" in license_text
    assert "END OF TERMS AND CONDITIONS" in license_text


def test_crate_publishes_only_the_public_testing_document() -> None:
    package = (ROOT / "Cargo.toml").read_text(encoding="utf-8")

    assert '"/docs/testing.md"' in package
    assert '"/docs/**"' not in package


@pytest.mark.parametrize("tag", ["0.5.1", "v0.5.0", "v0.5.1-rc.1"])
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


def _run_commands(source: str) -> list[str]:
    """Every `run:` script in a workflow as one line, folded YAML blocks joined.

    A folded block puts the command on the lines after `run: >`, so a naive search of
    single lines misses it entirely.
    """
    commands: list[str] = []
    lines = source.splitlines()
    for index, line in enumerate(lines):
        stripped = line.strip()
        if not stripped.startswith("- run:") and not stripped.startswith("run:"):
            continue
        head = stripped.split(":", 1)[1].strip()
        if head not in (">", "|", ">-", "|-"):
            commands.append(head)
            continue
        indent = len(line) - len(line.lstrip())
        body = []
        for following in lines[index + 1 :]:
            if not following.strip():
                continue
            if len(following) - len(following.lstrip()) <= indent:
                break
            body.append(following.strip())
        commands.append(" ".join(body))
    return commands


def test_ci_installs_dependencies_needed_during_full_test_collection() -> None:
    """Both Python jobs name every package the suite needs at collection time.

    Checked per package rather than as one run of words. The install lines carry
    version pins and an environment marker, so a contiguous string would break on a
    pin while nothing was actually missing — and would keep passing if a package were
    dropped from a line it had stopped matching anyway.
    """
    ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    installs = [
        command
        for command in _run_commands(ci)
        if "pip install" in command and "pytest" in command
    ]

    # Partitioned rather than searched with `next()`: an unmatched search would end
    # the test with StopIteration, which says nothing about what was wrong.
    quality = [command for command in installs if "ruff" in command]
    matrix = [command for command in installs if "ruff" not in command]
    assert len(quality) == 1, quality
    assert len(matrix) == 1, matrix

    for package in ("pytest", "pyarrow", "pandas", "polars", "psutil", "cryptography"):
        assert package in matrix[0], f"{package} missing from the matrix job"
    for package in ("maturin", "pytest-cov", "ruff", "twine", "pyarrow", "pandas", "cryptography"):
        assert package in quality[0], f"{package} missing from the coverage job"


def test_release_gate_remains_compatible_with_python_310() -> None:
    source = (ROOT / "benchmarks/release_gate.py").read_text(encoding="utf-8")

    assert "from datetime import UTC" not in source
    assert "datetime.now(UTC)" not in source
    assert "datetime.now(timezone.utc)" in source


def test_ci_and_manual_release_benchmarks_require_seven_samples() -> None:
    ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    benchmarks = (ROOT / ".github/workflows/benchmarks.yml").read_text(encoding="utf-8")

    assert "--runs 7" in ci
    assert 'default: "7"' in benchmarks
    assert "minimum 7" in benchmarks


def test_release_artifacts_are_checksummed_sbomed_and_attested_before_publish() -> None:
    publish = (ROOT / ".github/workflows/publish.yml").read_text(encoding="utf-8")

    assert "attestations: write" in publish
    assert "id-token: write" in publish
    assert "SHA256SUMS" in publish
    assert "format: spdx-json" in publish
    assert "anchore/sbom-action@e22c389904149dbc22b58101806040fa8d37a610" in publish
    assert (
        "actions/attest-build-provenance@e8998f949152b193b063cb0ec769d69d929409be"
        in publish
    )
    assert "actions/attest-sbom@bd218ad0dbcb3e146bd073d1d9c6d78e08aa8a0b" in publish
    assert "subject-path: \"dist/*\"" in publish
    assert "sbom-path: release-metadata/proofframe.spdx.json" in publish


def test_crate_and_python_publication_depend_on_the_same_verified_release_gate() -> None:
    publish = (ROOT / ".github/workflows/publish.yml").read_text(encoding="utf-8")

    assert "cargo publish --locked" in publish
    assert "CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}" in publish
    assert "needs: [gate, wheels, sdist]" in publish


def test_crate_is_packaged_checksummed_sbomed_and_attested_before_publication() -> None:
    publish = (ROOT / ".github/workflows/publish.yml").read_text(encoding="utf-8")
    crate_package = publish.index("cargo package --locked")
    crate_attestation = publish.index(
        'subject-path: "${{ steps.crate-source.outputs.root }}/target/package/*.crate"'
    )
    crate_publish = publish.index("cargo publish --locked")

    assert "crate-SHA256SUMS" in publish
    assert "proofframe-crate.spdx.json" in publish
    assert crate_package < crate_attestation < crate_publish
    assert publish.count("attestations: write") >= 2
    assert publish.count("id-token: write") >= 2
