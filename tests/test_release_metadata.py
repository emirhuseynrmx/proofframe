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
