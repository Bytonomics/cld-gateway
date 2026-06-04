"""Archive writer for canonical cld-gateway package directories."""

import tarfile
from pathlib import Path


def write_archive(package_dir: Path, archive_path: Path, *, force: bool) -> None:
    if _is_relative_to(archive_path, package_dir):
        raise RuntimeError(
            f"Archive output must be outside the package directory: {archive_path}"
        )

    archive_path.parent.mkdir(parents=True, exist_ok=True)
    if archive_path.exists():
        if not force:
            raise RuntimeError(f"Archive output already exists: {archive_path}")
        archive_path.unlink()

    suffix = archive_path.name
    if not (suffix.endswith(".tar.gz") or suffix.endswith(".tgz")):
        raise RuntimeError(
            f"Unsupported archive suffix for {archive_path}. Use .tar.gz or .tgz."
        )

    _write_tar_gz(package_dir, archive_path)


def _write_tar_gz(package_dir: Path, archive_path: Path) -> None:
    with tarfile.open(archive_path, "w:gz") as archive:
        for path in _package_entries(package_dir):
            archive.add(
                path,
                arcname=path.relative_to(package_dir),
                recursive=False,
            )


def _package_entries(package_dir: Path) -> list[Path]:
    return sorted(
        package_dir.rglob("*"),
        key=lambda p: p.relative_to(package_dir).as_posix(),
    )


def _is_relative_to(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False
