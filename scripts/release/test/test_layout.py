"""Tests for cld_gateway_package.layout."""

import importlib.util
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
    PACKAGE_ASSET_FILENAMES,
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

        # Check package assets.
        for asset_filename in PACKAGE_ASSET_FILENAMES:
            asset_path = package_dir / asset_filename
            assert asset_path.is_file(), f"Package asset not found: {asset_path}"

        # Check wrapper scripts are executable
        for wrapper in ["bin/cld-gateway-sh", "bin/cldg", "bin/clddg"]:
            wrapper_path = package_dir / wrapper
            assert wrapper_path.is_file(), f"Wrapper not found: {wrapper_path}"
            assert bool(wrapper_path.stat().st_mode & stat.S_IXUSR), f"Wrapper not executable: {wrapper}"

        # Check that the Python post-install helper is included
        helper_path = package_dir / "homebrew/post_install.py"
        assert helper_path.is_file(), f"Helper not found: {helper_path}"

        # Check metadata.
        meta_path = package_dir / METADATA_FILENAME
        meta = json.loads(meta_path.read_text())
        assert meta["version"] == "0.1.0"
        assert meta["target"] == "aarch64-apple-darwin"
        assert meta["cargoProfile"] == "release"
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


def test_build_package_dir_with_custom_cargo_profile() -> None:
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        package_dir = tmp_path / "pkg"
        entrypoint = tmp_path / "cld-gateway"
        make_fake_executable(entrypoint)

        prepare_package_dir(package_dir, force=False)
        build_package_dir(package_dir, "0.2.0", spec, entrypoint, cargo_profile="debug")
        validate_package_dir(package_dir, spec)

        # Check metadata includes custom cargo profile.
        meta_path = package_dir / METADATA_FILENAME
        meta = json.loads(meta_path.read_text())
        assert meta["version"] == "0.2.0"
        assert meta["target"] == "aarch64-apple-darwin"
        assert meta["cargoProfile"] == "debug"
        assert meta["entrypoint"] == f"bin/{BIN_NAME}"


def test_validate_package_dir_missing_package_asset() -> None:
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    for missing_asset in PACKAGE_ASSET_FILENAMES:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            package_dir = tmp_path / "pkg"
            entrypoint = tmp_path / "cld-gateway"
            make_fake_executable(entrypoint)

            prepare_package_dir(package_dir, force=False)
            build_package_dir(package_dir, "0.3.0", spec, entrypoint)

            (package_dir / missing_asset).unlink()

            try:
                validate_package_dir(package_dir, spec)
                assert False, f"Expected RuntimeError for missing package asset {missing_asset}"
            except RuntimeError as e:
                assert missing_asset in str(e), (
                    f"Error message should mention missing asset {missing_asset}: {e}"
                )


def test_validate_package_dir_missing_cargo_profile() -> None:
    """Test that validation fails if cargoProfile is missing from metadata."""
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        package_dir = tmp_path / "pkg"
        entrypoint = tmp_path / "cld-gateway"
        make_fake_executable(entrypoint)

        prepare_package_dir(package_dir, force=False)
        build_package_dir(package_dir, "0.3.0", spec, entrypoint)

        # Manually remove cargoProfile from metadata to test validation.
        meta_path = package_dir / METADATA_FILENAME
        meta = json.loads(meta_path.read_text())
        del meta["cargoProfile"]
        with open(meta_path, "w", encoding="utf-8") as fh:
            json.dump(meta, fh, indent=2)
            fh.write("\n")

        # Validation should fail.
        try:
            validate_package_dir(package_dir, spec)
            assert False, "Expected RuntimeError for missing cargoProfile"
        except RuntimeError as e:
            assert "cargoProfile" in str(e), f"Error message should mention cargoProfile: {e}"


def test_validate_package_dir_invalid_cargo_profile() -> None:
    """Test that validation fails if cargoProfile is not a string."""
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        package_dir = tmp_path / "pkg"
        entrypoint = tmp_path / "cld-gateway"
        make_fake_executable(entrypoint)

        prepare_package_dir(package_dir, force=False)
        build_package_dir(package_dir, "0.4.0", spec, entrypoint)

        # Manually set cargoProfile to invalid value.
        meta_path = package_dir / METADATA_FILENAME
        meta = json.loads(meta_path.read_text())
        meta["cargoProfile"] = 123  # Invalid: should be string
        with open(meta_path, "w", encoding="utf-8") as fh:
            json.dump(meta, fh, indent=2)
            fh.write("\n")

        # Validation should fail.
        try:
            validate_package_dir(package_dir, spec)
            assert False, "Expected RuntimeError for invalid cargoProfile type"
        except RuntimeError as e:
            assert "cargoProfile" in str(e), f"Error message should mention cargoProfile: {e}"


def test_validate_package_dir_wrapper_not_executable() -> None:
    """Test that validation fails if wrapper scripts are not executable."""
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        package_dir = tmp_path / "pkg"
        entrypoint = tmp_path / "cld-gateway"
        make_fake_executable(entrypoint)

        prepare_package_dir(package_dir, force=False)
        build_package_dir(package_dir, "0.2.0", spec, entrypoint)

        # Remove executable bit from a wrapper script
        wrapper_path = package_dir / "bin/cldg"
        wrapper_path.chmod(0o644)  # Not executable

        try:
            validate_package_dir(package_dir, spec)
            assert False, "Expected RuntimeError for non-executable wrapper script"
        except RuntimeError as e:
            assert "executable" in str(e).lower(), f"Error should mention executable: {e}"


def test_validate_package_dir_post_install_not_executable() -> None:
    """Test that validation fails if post_install.py is not executable."""
    spec = TARGET_SPECS["aarch64-apple-darwin"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        package_dir = tmp_path / "pkg"
        entrypoint = tmp_path / "cld-gateway"
        make_fake_executable(entrypoint)

        prepare_package_dir(package_dir, force=False)
        build_package_dir(package_dir, "0.2.0", spec, entrypoint)

        # Remove executable bit from post_install.py
        helper_path = package_dir / "homebrew/post_install.py"
        helper_path.chmod(0o644)  # Not executable

        try:
            validate_package_dir(package_dir, spec)
            assert False, "Expected RuntimeError for non-executable post_install.py"
        except RuntimeError as e:
            assert "executable" in str(e).lower(), f"Error should mention executable: {e}"
            assert "post_install.py" in str(e), f"Error should mention post_install.py: {e}"




def load_post_install_module():
    module_path = Path(__file__).resolve().parents[1] / "cld_gateway_package" / "homebrew" / "post_install.py"
    spec = importlib.util.spec_from_file_location("cld_gateway_post_install", module_path)
    assert spec is not None and spec.loader is not None, f"Failed to load module spec for {module_path}"
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_post_install_deploys_codex_status_asset() -> None:
    post_install = load_post_install_module()

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        user_home = tmp_path / "home"
        formula_prefix = tmp_path / "formula"
        user_home.mkdir(parents=True)
        (formula_prefix / "libexec" / "commands" / "codex").mkdir(parents=True)
        (formula_prefix / "libexec" / "commands" / "codex" / "status.md").write_text(
            "translated status instructions\n",
            encoding="utf-8",
        )
        (formula_prefix / "share" / "cld-gateway").mkdir(parents=True)
        (formula_prefix / "share" / "cld-gateway" / "config.yml").write_text(
            "listen_addr: 127.0.0.1:8080\n",
            encoding="utf-8",
        )
        (formula_prefix / "share" / "cld-gateway" / "settings.json").write_text(
            "{}\n",
            encoding="utf-8",
        )

        original_file = post_install.__file__
        original_path_home = Path.home
        try:
            post_install.__file__ = str(formula_prefix / "libexec" / "post_install.py")
            Path.home = staticmethod(lambda: user_home)
            post_install.post_install()
        finally:
            post_install.__file__ = original_file
            Path.home = original_path_home

        installed_status = user_home / ".codex_gateway" / "commands" / "codex" / "status.md"
        assert installed_status.is_file(), f"Installed status asset not found: {installed_status}"
        assert installed_status.read_text(encoding="utf-8") == "translated status instructions\n"
