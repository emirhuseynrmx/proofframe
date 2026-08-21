"""Build and verify clean, deterministic ProofFrame source archives."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import tarfile
import zipfile
from pathlib import Path, PurePosixPath


class ArchiveHygieneError(ValueError):
    """Raised when a source archive contains unsafe or generated content."""


_FORBIDDEN_SEGMENTS = {
    ".git",
    ".pytest_cache",
    ".venv",
    "__pycache__",
    "dist",
    "target",
}
_FORBIDDEN_NAMES = {".coverage", "coverage.xml"}
_FORBIDDEN_SUFFIXES = {".dll", ".dylib", ".exe", ".pdb", ".pyc", ".pyd", ".so"}
_LOCAL_PATHS = (
    re.compile(rb"[A-Za-z]:[\\/]+Users[\\/]+[^\\/\x00\r\n]+[\\/]+"),
    re.compile(rb"/home/[^/\x00\r\n]+/"),
)
_REQUIRED_PATHS = {
    "Cargo.toml",
    "pyproject.toml",
    "src/lib.rs",
    "python/proofframe/__init__.py",
}
_LOCAL_PATH_SCAN_EXEMPT = {
    "scripts/source_artifacts.py",
    "tests/test_source_artifacts.py",
}
_VERSION = re.compile(r'^version\s*=\s*"([^"]+)"\s*$', re.MULTILINE)


def _members(path: Path) -> list[tuple[str, bytes]]:
    if path.name.endswith(".tar.gz"):
        with tarfile.open(path, "r:gz") as archive:
            members = []
            for member in archive.getmembers():
                if member.issym() or member.islnk():
                    raise ArchiveHygieneError(f"archive contains a link: {member.name}")
                if not member.isfile():
                    continue
                source = archive.extractfile(member)
                if source is None:
                    raise ArchiveHygieneError(f"archive member cannot be read: {member.name}")
                members.append((member.name, source.read()))
            return members
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            return [
                (member.filename, archive.read(member))
                for member in archive.infolist()
                if not member.is_dir()
            ]
    raise ArchiveHygieneError(f"unsupported source archive: {path.name}")


def _validate_member_path(raw_name: str, expected_root: str) -> PurePosixPath:
    if "\\" in raw_name:
        raise ArchiveHygieneError(f"archive path is not POSIX-normalized: {raw_name}")
    member = PurePosixPath(raw_name)
    if member.is_absolute() or ".." in member.parts:
        raise ArchiveHygieneError(f"archive path escapes its root: {raw_name}")
    if not member.parts or member.parts[0] != expected_root:
        raise ArchiveHygieneError(f"archive member is outside {expected_root}: {raw_name}")
    return PurePosixPath(*member.parts[1:])


def _validate_member_content(raw_name: str, relative: PurePosixPath, content: bytes) -> str:
    if any(part in _FORBIDDEN_SEGMENTS for part in relative.parts):
        raise ArchiveHygieneError(f"archive contains generated directory: {raw_name}")
    if relative.name in _FORBIDDEN_NAMES or relative.suffix.lower() in _FORBIDDEN_SUFFIXES:
        raise ArchiveHygieneError(f"archive contains generated/native file: {raw_name}")
    normalized = relative.as_posix()
    if normalized not in _LOCAL_PATH_SCAN_EXEMPT and any(
        pattern.search(content) for pattern in _LOCAL_PATHS
    ):
        raise ArchiveHygieneError(f"archive contains a local absolute path: {raw_name}")
    return normalized


def _validate_required_paths(relative_paths: set[str]) -> None:
    missing = sorted(_REQUIRED_PATHS - relative_paths)
    if missing:
        raise ArchiveHygieneError(f"source archive is incomplete: missing {', '.join(missing)}")


def verify_source_archive(path: Path, *, expected_root: str) -> None:
    """Reject archives that are ambiguous, unsafe, generated, or machine-local."""

    members = _members(path)
    if not members:
        raise ArchiveHygieneError("source archive is empty")
    relative_paths: set[str] = set()
    for raw_name, content in members:
        relative = _validate_member_path(raw_name, expected_root)
        if not relative.parts:
            continue
        normalized = _validate_member_content(raw_name, relative, content)
        if normalized in relative_paths:
            raise ArchiveHygieneError(f"archive contains a duplicate path: {raw_name}")
        relative_paths.add(normalized)
    _validate_required_paths(relative_paths)


def _project_version(root: Path) -> str:
    match = _VERSION.search((root / "pyproject.toml").read_text(encoding="utf-8"))
    if match is None:
        raise ArchiveHygieneError("pyproject.toml has no project version")
    return match.group(1)


def _git_executable() -> str:
    executable = shutil.which("git")
    if executable is None:
        raise ArchiveHygieneError("Git executable is unavailable")
    return str(Path(executable).resolve())


def build_source_zip(root: Path, output_directory: Path) -> Path:
    """Archive exactly Git-tracked regular files with stable metadata."""

    root = root.resolve()
    version = _project_version(root)
    prefix = f"proofframe-{version}"
    tracked = subprocess.run(
        [_git_executable(), "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    paths = sorted(PurePosixPath(os.fsdecode(raw)) for raw in tracked if raw)
    output_directory.mkdir(parents=True, exist_ok=True)
    output = output_directory / f"{prefix}-source.zip"
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for relative in paths:
            source = (root / Path(*relative.parts)).resolve()
            if root not in source.parents or not source.is_file() or source.is_symlink():
                raise ArchiveHygieneError(f"tracked source is not a regular in-tree file: {relative}")
            info = zipfile.ZipInfo(f"{prefix}/{relative.as_posix()}", (1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, source.read_bytes(), compresslevel=9)
    verify_source_archive(output, expected_root=prefix)
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, default=Path("dist"))
    parser.add_argument("--verify", type=Path, action="append", default=[])
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    version = _project_version(root)
    expected_root = f"proofframe-{version}"
    built = build_source_zip(root, arguments.output)
    for artifact in arguments.verify:
        verify_source_archive(artifact, expected_root=expected_root)
    print(built)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
