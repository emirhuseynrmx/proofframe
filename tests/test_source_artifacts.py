import io
import tarfile
import zipfile

import pytest

from scripts.source_artifacts import ArchiveHygieneError, build_source_zip, verify_source_archive


def _zip(entries: dict[str, bytes]) -> bytes:
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        for name, content in entries.items():
            archive.writestr(name, content)
    return output.getvalue()


def _sdist(entries: dict[str, bytes]) -> bytes:
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for name, content in entries.items():
            info = tarfile.TarInfo(name)
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
    return output.getvalue()


@pytest.mark.parametrize("builder,suffix", [(_zip, ".zip"), (_sdist, ".tar.gz")])
def test_source_archive_accepts_one_clean_release_root(tmp_path, builder, suffix):
    artifact = tmp_path / f"proofframe-0.5.0{suffix}"
    artifact.write_bytes(
        builder(
            {
                "proofframe-0.5.0/Cargo.toml": b"[package]\nname='proofframe'\n",
                "proofframe-0.5.0/pyproject.toml": b"[project]\nname='proofframe'\n",
                "proofframe-0.5.0/src/lib.rs": b"pub fn check() {}\n",
                "proofframe-0.5.0/python/proofframe/__init__.py": b"__version__='0.5.0'\n",
            }
        )
    )

    verify_source_archive(artifact, expected_root="proofframe-0.5.0")


@pytest.mark.parametrize(
    "bad_name",
    [
        "proofframe-0.5.0/.coverage",
        "proofframe-0.5.0/python/proofframe/_proofframe.pyd",
        "proofframe-0.5.0/target/release/proofframe.pdb",
        "proofframe-0.5.0/__pycache__/api.pyc",
    ],
)
def test_source_archive_rejects_generated_or_native_files(tmp_path, bad_name):
    artifact = tmp_path / "proofframe-0.5.0.zip"
    artifact.write_bytes(
        _zip(
            {
                "proofframe-0.5.0/Cargo.toml": b"ok",
                "proofframe-0.5.0/pyproject.toml": b"ok",
                "proofframe-0.5.0/src/lib.rs": b"ok",
                "proofframe-0.5.0/python/proofframe/__init__.py": b"ok",
                bad_name: b"generated",
            }
        )
    )

    with pytest.raises(ArchiveHygieneError):
        verify_source_archive(artifact, expected_root="proofframe-0.5.0")


def test_source_archive_rejects_local_absolute_path_leaks(tmp_path):
    artifact = tmp_path / "proofframe-0.5.0.zip"
    artifact.write_bytes(
        _zip(
            {
                "proofframe-0.5.0/Cargo.toml": b"ok",
                "proofframe-0.5.0/pyproject.toml": b"ok",
                "proofframe-0.5.0/src/lib.rs": b'const BUILD: &str = "C:\\\\Users\\\\emirh\\\\repo";',
                "proofframe-0.5.0/python/proofframe/__init__.py": b"ok",
            }
        )
    )

    with pytest.raises(ArchiveHygieneError, match="local absolute path"):
        verify_source_archive(artifact, expected_root="proofframe-0.5.0")


def test_build_source_zip_fails_closed_when_git_is_unavailable(tmp_path, monkeypatch):
    monkeypatch.setattr("scripts.source_artifacts._project_version", lambda _: "0.5.0")
    monkeypatch.setattr("scripts.source_artifacts.shutil.which", lambda _: None)

    with pytest.raises(ArchiveHygieneError, match="Git executable is unavailable"):
        build_source_zip(tmp_path, tmp_path / "dist")


@pytest.mark.parametrize(
    ("member_name", "message"),
    [
        (r"proofframe-0.5.0\Cargo.toml", "POSIX-normalized"),
        ("proofframe-0.5.0/../Cargo.toml", "escapes its root"),
        ("another-root/Cargo.toml", "outside proofframe-0.5.0"),
    ],
)
def test_source_archive_rejects_unsafe_member_paths(tmp_path, member_name, message):
    artifact = tmp_path / "proofframe-0.5.0.tar.gz"
    artifact.write_bytes(_sdist({member_name: b"unsafe"}))

    with pytest.raises(ArchiveHygieneError, match=message):
        verify_source_archive(artifact, expected_root="proofframe-0.5.0")


def test_source_archive_rejects_duplicate_members(tmp_path):
    artifact = tmp_path / "proofframe-0.5.0.zip"
    with zipfile.ZipFile(artifact, "w") as archive:
        archive.writestr("proofframe-0.5.0/Cargo.toml", b"first")
        archive.writestr("proofframe-0.5.0/Cargo.toml", b"second")

    with pytest.raises(ArchiveHygieneError, match="duplicate path"):
        verify_source_archive(artifact, expected_root="proofframe-0.5.0")


def test_source_archive_rejects_missing_required_paths(tmp_path):
    artifact = tmp_path / "proofframe-0.5.0.zip"
    artifact.write_bytes(_zip({"proofframe-0.5.0/Cargo.toml": b"incomplete"}))

    with pytest.raises(ArchiveHygieneError, match="source archive is incomplete"):
        verify_source_archive(artifact, expected_root="proofframe-0.5.0")
