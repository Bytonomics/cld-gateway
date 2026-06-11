"""Canonical cld-gateway package directory layout."""

import json
import shutil
import stat
from pathlib import Path

from .targets import TargetSpec


LAYOUT_VERSION = 1
BIN_NAME = "cld-gateway"
METADATA_FILENAME = "cld-gateway-package.json"
# Individual files to package (preserve paths exactly)
PACKAGE_ASSET_FILENAMES = (
    "config.yml",
    "settings.json",
    "homebrew/post_install.py",
    "bin/cld-gateway-sh",
    "bin/cldg",
    "bin/clddg",
)
# Directories to package recursively (preserve relative directory structure)
PACKAGE_DIRECTORIES = (
    "commands",
)


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
    cargo_profile: str = "release",
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
        "cargoProfile": cargo_profile,
        "entrypoint": f"bin/{BIN_NAME}",
    }
    _write_json(package_dir / METADATA_FILENAME, metadata)

    package_source_dir = _package_source_dir()

    # Copy individual asset files
    for asset_filename in PACKAGE_ASSET_FILENAMES:
        src = package_source_dir / asset_filename
        dst = package_dir / asset_filename
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)
        # Preserve execute bits for executable assets
        if asset_filename in ("bin/cld-gateway-sh", "bin/cldg", "bin/clddg", "homebrew/post_install.py"):
            mode = src.stat().st_mode
            dst.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    # Copy directories recursively, preserving relative structure
    for dir_name in PACKAGE_DIRECTORIES:
        src_dir = package_source_dir / dir_name
        dst_dir = package_dir / dir_name
        if src_dir.is_dir():
            shutil.copytree(src_dir, dst_dir, dirs_exist_ok=True)


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

    # Validate cargoProfile field exists and is a non-empty string
    cargo_profile = metadata.get("cargoProfile")
    if not cargo_profile or not isinstance(cargo_profile, str):
        raise RuntimeError(
            f"Invalid package metadata field 'cargoProfile': "
            f"expected a non-empty string, got {cargo_profile!r}"
        )

    bin_path = package_dir / "bin" / BIN_NAME
    if not bin_path.is_file():
        raise RuntimeError(f"Missing binary: bin/{BIN_NAME}")
    if not _is_executable(bin_path):
        raise RuntimeError(f"Binary is not executable: bin/{BIN_NAME}")

    # Validate individual asset files
    for asset_filename in PACKAGE_ASSET_FILENAMES:
        asset_path = package_dir / asset_filename
        if not asset_path.is_file():
            raise RuntimeError(f"Missing package asset: {asset_filename}")
        # Wrapper scripts and post-install helper must be executable
        if asset_filename in ("bin/cld-gateway-sh", "bin/cldg", "bin/clddg", "homebrew/post_install.py"):
            if not _is_executable(asset_path):
                raise RuntimeError(f"Script is not executable: {asset_filename}")

    # Validate that directories exist and contain files
    for dir_name in PACKAGE_DIRECTORIES:
        dir_path = package_dir / dir_name
        if not dir_path.is_dir():
            raise RuntimeError(f"Missing package directory: {dir_name}")
        # Verify directory has content
        if not any(dir_path.rglob("*")):
            raise RuntimeError(f"Package directory is empty: {dir_name}")


def _write_json(path: Path, value: object) -> None:
    with open(path, "w", encoding="utf-8") as out:
        json.dump(value, out, indent=2)
        out.write("\n")


def _is_executable(path: Path) -> bool:
    return bool(path.stat().st_mode & stat.S_IXUSR)


def _package_source_dir() -> Path:
    return Path(__file__).resolve().parent
