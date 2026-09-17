"""Fail a release when Cargo, Python metadata, the integrations, or the Git tag disagree."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

VERSION_LINE = re.compile(r'^version\s*=\s*"([^"]+)"\s*$', re.MULTILINE)


def _section_version(path: Path, section: str) -> str:
    source = path.read_text(encoding="utf-8")
    marker = f"[{section}]"
    try:
        body = source.split(marker, 1)[1].split("\n[", 1)[0]
    except IndexError as error:
        raise ValueError(f"{path.name} has no [{section}] section") from error
    match = VERSION_LINE.search(body)
    if match is None:
        raise ValueError(f"{path.name} [{section}] has no version")
    return match.group(1)


def read_versions(root: Path) -> tuple[str, str]:
    """Return the Cargo and Python project versions from *root*."""

    return (
        _section_version(root / "Cargo.toml", "package"),
        _section_version(root / "pyproject.toml", "project"),
    )


def verify_versions(root: Path, tag: str) -> None:
    """Require exact Cargo/Python agreement and an exact ``v<version>`` tag."""

    cargo_version, python_version = read_versions(root)
    if cargo_version != python_version:
        raise ValueError(
            f"version mismatch: Cargo.toml={cargo_version}, pyproject.toml={python_version}"
        )
    expected_tag = f"v{cargo_version}"
    if tag != expected_tag:
        raise ValueError(f"release tag must be {expected_tag}, got {tag}")


#: The distributions published alongside the engine from this same tag.
INTEGRATIONS = ("integrations/airflow", "integrations/dagster")

DEPENDS_ON_ENGINE = re.compile(r'"proofframe>=([^,"]+),<([^"]+)"')


def verify_integrations(root: Path, version: str) -> None:
    """Require each scheduler package to carry the release version and require it.

    Two things can drift independently and neither shows up in a green test run: a
    package can keep the previous version string, and it can keep a lower bound that
    a resolver would satisfy with the previous engine. Both would publish happily and
    install the wrong pair.
    """

    for name in INTEGRATIONS:
        manifest = root / name / "pyproject.toml"
        declared = _section_version(manifest, "project")
        if declared != version:
            raise ValueError(f"{name} is {declared}, expected {version}")

        match = DEPENDS_ON_ENGINE.search(manifest.read_text(encoding="utf-8"))
        if match is None:
            raise ValueError(f"{name} does not pin a proofframe range")
        lower = match.group(1)
        if lower != version:
            raise ValueError(
                f"{name} requires proofframe>={lower}, which a resolver can satisfy "
                f"with an engine older than {version}"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument(
        "--integrations",
        action="store_true",
        help="also check the scheduler distributions published from this tag",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        verify_versions(root, args.tag)
        if args.integrations:
            verify_integrations(root, args.tag.removeprefix("v"))
    except ValueError as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
