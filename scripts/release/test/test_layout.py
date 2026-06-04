"""Tests for cld_gateway_package.layout."""

import json
import os
import stat
import tempfile
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from cld_gateway_package.layout import (
    build_package_dir,
    prepare_package_dir,
    validate_package_dir,
    BIN_NAME,
    METADATA_FILENAME,
)
from cld_gateway_package.targets import TARGET_SPECS


def make_fake_executable(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"#!/bin/sh\necho fake\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def test_build_and_validate_package_dir() -> None:
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        package_dir = tmp_path / "pkg"
        entrypoint = tmp_path / "cld-gateway"
        make_fake_executable(entrypoint)

        prepare_package_dir(package_dir, force=False)
        build_package_dir(package_dir, "0.1.0", spec, entrypoint)
        validate_package_dir(package_dir, spec)

        # Check binary exists and is executable.
        bin_path = package_dir / "bin" / BIN_NAME
        assert bin_path.is_file(), f"Binary not found: {bin_path}"
        assert bool(bin_path.stat().st_mode & stat.S_IXUSR), "Binary not executable"

        # Check metadata.
        meta_path = package_dir / METADATA_FILENAME
        meta = json.loads(meta_path.read_text())
        assert meta["version"] == "0.1.0"
        assert meta["target"] == "aarch64-apple-darwin"
        assert meta["entrypoint"] == f"bin/{BIN_NAME}"


def test_prepare_package_dir_force_replaces() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        package_dir = Path(tmp) / "pkg"
        package_dir.mkdir()
        (package_dir / "leftover.txt").write_text("old")
        prepare_package_dir(package_dir, force=True)
        assert not (package_dir / "leftover.txt").exists()


def test_prepare_package_dir_no_force_raises_on_non_empty() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        package_dir = Path(tmp) / "pkg"
        package_dir.mkdir()
        (package_dir / "leftover.txt").write_text("old")
        try:
            prepare_package_dir(package_dir, force=False)
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass


if __name__ == "__main__":
    test_build_and_validate_package_dir()
    test_prepare_package_dir_force_replaces()
    test_prepare_package_dir_no_force_raises_on_non_empty()
    print("All tests passed.")
