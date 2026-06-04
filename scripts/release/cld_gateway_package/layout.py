"""Canonical cld-gateway package directory layout."""

import json
import shutil
import stat
from pathlib import Path

from .targets import TargetSpec


LAYOUT_VERSION = 1
BIN_NAME = "cld-gateway"
METADATA_FILENAME = "cld-gateway-package.json"


def prepare_package_dir(package_dir: Path, *, force: bool) -> None:
    if package_dir.exists():
        if not package_dir.is_dir():
            raise RuntimeError(
                f"Package output exists and is not a directory: {package_dir}"
            )
        if any(package_dir.iterdir()):
            if not force:
                raise RuntimeError(
                    f"Package output directory is not empty: {package_dir}. "
                    "Pass --force to replace it."
                )
            shutil.rmtree(package_dir)
    package_dir.mkdir(parents=True, exist_ok=True)


def build_package_dir(
    package_dir: Path,
    version: str,
    spec: TargetSpec,
    entrypoint_bin: Path,
) -> None:
    bin_dir = package_dir / "bin"
    bin_dir.mkdir()

    dest = bin_dir / BIN_NAME
    shutil.copyfile(entrypoint_bin, dest)
    # Always set executable bits on the installed binary.
    mode = dest.stat().st_mode
    dest.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    metadata = {
        "layoutVersion": LAYOUT_VERSION,
        "version": version,
        "target": spec.target,
        "entrypoint": f"bin/{BIN_NAME}",
    }
    _write_json(package_dir / METADATA_FILENAME, metadata)


def validate_package_dir(package_dir: Path, spec: TargetSpec) -> None:
    bin_dir = package_dir / "bin"
    if not bin_dir.is_dir():
        raise RuntimeError("Missing package directory: bin")

    metadata_path = package_dir / METADATA_FILENAME
    if not metadata_path.is_file():
        raise RuntimeError(f"Missing package metadata: {METADATA_FILENAME}")

    with open(metadata_path, encoding="utf-8") as fh:
        metadata = json.load(fh)

    for key, expected in [
        ("layoutVersion", LAYOUT_VERSION),
        ("target", spec.target),
        ("entrypoint", f"bin/{BIN_NAME}"),
    ]:
        actual = metadata.get(key)
        if actual != expected:
            raise RuntimeError(
                f"Invalid package metadata field {key!r}: "
                f"expected {expected!r}, got {actual!r}"
            )

    bin_path = package_dir / "bin" / BIN_NAME
    if not bin_path.is_file():
        raise RuntimeError(f"Missing binary: bin/{BIN_NAME}")
    if not _is_executable(bin_path):
        raise RuntimeError(f"Binary is not executable: bin/{BIN_NAME}")


def _write_json(path: Path, value: object) -> None:
    with open(path, "w", encoding="utf-8") as out:
        json.dump(value, out, indent=2)
        out.write("\n")


def _is_executable(path: Path) -> bool:
    return bool(path.stat().st_mode & stat.S_IXUSR)
