"""Tests for cld-gateway release workflow semantics and package metadata."""

import json
import re
import stat
import tarfile
import tempfile
from pathlib import Path
import sys

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from cld_gateway_package.archive import write_archive
from cld_gateway_package.layout import (
    BIN_NAME,
    METADATA_FILENAME,
    PACKAGE_ASSET_FILENAMES,
    build_package_dir,
    prepare_package_dir,
)
from cld_gateway_package.targets import REPO_ROOT, TARGET_SPECS
from cld_gateway_package.version import read_workspace_version


class TestReleaseWorkflowSemantics:
    """Targeted regression tests for the release workflow semantics."""

    @staticmethod
    def _workflow() -> dict:
        workflow_path = REPO_ROOT / ".github" / "workflows" / "release.yml"
        with open(workflow_path, "r", encoding="utf-8") as fh:
            content = yaml.safe_load(fh)
        assert isinstance(content, dict), "Workflow must be valid YAML"
        return content

    def test_release_workflow_exists(self) -> None:
        workflow_path = REPO_ROOT / ".github" / "workflows" / "release.yml"
        assert workflow_path.is_file(), f"Release workflow not found: {workflow_path}"
        assert self._workflow().get("name") == "release"

    def test_release_workflow_supports_prerelease_tags(self) -> None:
        content = self._workflow()
        triggers = content.get("on") or content.get(True) or {}
        if not isinstance(triggers, dict):
            triggers = {}
        tags = triggers.get("push", {}).get("tags", [])
        assert "cld-gateway-v*.*.*" in tags
        assert "cld-gateway-v*.*.*-alpha*" in tags
        assert "cld-gateway-v*.*.*-beta*" in tags

        tag_check_steps = content["jobs"]["tag-check"].get("steps", [])
        validation_steps = [
            s for s in tag_check_steps if "Validate tag" in s.get("name", "")
        ]
        assert validation_steps, "tag-check job must have validation step"
        run_script = validation_steps[0].get("run", "")
        assert "-(alpha|beta)" in run_script, (
            "Tag validation regex must accept prerelease suffixes"
        )

    def test_release_workflow_concurrency_is_scoped_to_ref(self) -> None:
        content = self._workflow()
        concurrency = content.get("concurrency", {})
        assert concurrency.get("cancel-in-progress") is True
        group = concurrency.get("group", "")
        assert "github.ref" in group, "Concurrency group must be scoped by ref/tag"

    def test_release_workflow_installs_uv_before_using_it(self) -> None:
        content = self._workflow()
        build_steps = content["jobs"]["build"].get("steps", [])
        step_names = [step.get("name", "") for step in build_steps]
        assert "Install uv" in step_names
        install_uv_index = step_names.index("Install uv")
        package_index = step_names.index("Package archive")
        assert install_uv_index < package_index, "uv must be installed before package archive step"

    def test_release_workflow_does_not_publish_install_ps1(self) -> None:
        content = self._workflow()
        release_steps = content["jobs"]["release"].get("steps", [])
        publish_steps = [
            s for s in release_steps if "Publish GitHub Release" in s.get("name", "")
        ]
        assert publish_steps, "release job must publish a GitHub Release"
        files_block = publish_steps[0].get("with", {}).get("files", "")
        assert "dist/install.sh" in files_block
        assert "install.ps1" not in files_block

    def test_template_does_not_invoke_post_install(self) -> None:
        """Regression test: ensure Homebrew formula template never auto-invokes post_install.

        IMPORTANT: Setup must be MANUAL via `cld-gateway-sh setup`, not automatic during
        `brew install`. The post_install.py helper exists as a reusable utility for manual
        setup orchestration, but the Homebrew formula must NOT invoke it automatically.

        This test prevents accidental reintroduction of the bug where a `def post_install`
        method in the formula would call `system(opt_libexec/"post_install.py")` during
        installation, which:
          1. Defeats user autonomy (setup happens invisibly)
          2. Creates unpredictable file creation/modification at install time
          3. Makes troubleshooting harder (users don't know what setup step failed)
          4. Violates Homebrew best practices (formulas should not modify user home)

        See: .github/scripts/templates/cld-gateway.rb.erb
        """
        template_path = (
            REPO_ROOT
            / "homebrew-tap"
            / ".github"
            / "scripts"
            / "templates"
            / "cld-gateway.rb.erb"
        )
        if not template_path.is_file():
            raise AssertionError(
                f"Homebrew formula template not found at {template_path}"
            )

        content = template_path.read_text(encoding="utf-8")
        assert 'pkgshare.install "config.yml", "settings.json"' in content
        assert 'libexec.install "commands"' in content

        # Check that the forbidden patterns do not exist in the template
        forbidden_patterns = [
            ("def post_install", "post_install method definition"),
            (
                "system(opt_libexec/\"post_install.py\")",
                "system call to post_install.py in formula",
            ),
            (
                "system(opt_libexec/'post_install.py')",
                "system call to post_install.py in formula (single quotes)",
            ),
        ]

        for pattern, description in forbidden_patterns:
            if pattern in content:
                raise AssertionError(
                    f"Homebrew formula template contains forbidden pattern: {description}\n"
                    f"Pattern: {pattern}\n"
                    f"File: {template_path}\n"
                    f"\nReason: Setup must be manual via `cld-gateway-sh setup`, not automatic.\n"
                    f"The post_install.py helper is available for manual setup, but the\n"
                    f"formula must NOT invoke it automatically during brew install."
                )

    def test_release_workflow_verify_job_greps_commands_asset(self) -> None:
        """Regression test: release workflow verify job must assert commands/ directory.

        The verify job iterates over all built archives and checks each one with grep.
        This test ensures the commands subtree assets are included in those grep assertions,
        so a packaging regression (dropping the asset) is caught by CI before release.
        """
        content = self._workflow()
        verify_steps = content["jobs"]["verify"].get("steps", [])
        verify_run_scripts = [
            s.get("run", "") for s in verify_steps if s.get("run")
        ]
        combined = "\n".join(verify_run_scripts)
        assert 'grep -q "^commands/"' in combined, (
            "release.yml verify job must grep for commands/ directory in every archive"
        )

    def test_rendered_formula_does_not_invoke_post_install(self) -> None:
        """Regression test: ensure rendered Homebrew formula never auto-invokes post_install.

        The checked-in rendered formula must stay in lockstep with the template and must not
        define a formula-time `post_install` hook that runs the setup helper during
        `brew install`.
        """
        formula_path = REPO_ROOT / "homebrew-tap" / "Formula" / "cld-gateway.rb"
        if not formula_path.is_file():
            raise AssertionError(f"Rendered Homebrew formula not found at {formula_path}")

        content = formula_path.read_text(encoding="utf-8")
        assert 'pkgshare.install "config.yml", "settings.json"' in content
        assert 'libexec.install "commands"' in content

        forbidden_patterns = [
            ("def post_install", "post_install method definition"),
            (
                "system(opt_libexec/\"post_install.py\")",
                "system call to post_install.py in rendered formula",
            ),
            (
                "system(opt_libexec/'post_install.py')",
                "system call to post_install.py in rendered formula (single quotes)",
            ),
        ]

        for pattern, description in forbidden_patterns:
            if pattern in content:
                raise AssertionError(
                    f"Rendered Homebrew formula contains forbidden pattern: {description}\n"
                    f"Pattern: {pattern}\n"
                    f"File: {formula_path}\n"
                    f"\nReason: Setup must be manual via `cld-gateway-sh setup`, not automatic."
                )


class TestPackageMetadataValidation:
    """Regression tests for package metadata field validation."""

    def test_metadata_has_required_fields(self) -> None:
        spec = TARGET_SPECS["aarch64-apple-darwin"]
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            package_dir = tmp_path / "pkg"
            entrypoint = tmp_path / "cld-gateway"
            _make_fake_executable(entrypoint)

            prepare_package_dir(package_dir, force=False)
            build_package_dir(package_dir, "0.1.0", spec, entrypoint, cargo_profile="release")

            meta_path = package_dir / METADATA_FILENAME
            meta = json.loads(meta_path.read_text(encoding="utf-8"))

            required_fields = {
                "layoutVersion",
                "version",
                "target",
                "cargoProfile",
                "entrypoint",
            }
            actual_fields = set(meta.keys())
            assert required_fields.issubset(actual_fields)

    def test_metadata_cargo_profile_round_trips(self) -> None:
        spec = TARGET_SPECS["aarch64-apple-darwin"]
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            package_dir = tmp_path / "pkg"
            entrypoint = tmp_path / "cld-gateway"
            _make_fake_executable(entrypoint)

            prepare_package_dir(package_dir, force=False)
            build_package_dir(package_dir, "0.2.0", spec, entrypoint, cargo_profile="debug")

            meta_path = package_dir / METADATA_FILENAME
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            assert meta["cargoProfile"] == "debug"

    def test_archive_contains_binary_metadata_and_package_assets(self) -> None:
        """Test that the release archive contains all required assets including Python helper and wrapper scripts."""
        spec = TARGET_SPECS["x86_64-apple-darwin"]
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            package_dir = tmp_path / "pkg"
            entrypoint = tmp_path / "cld-gateway"
            archive_out = tmp_path / f"cld-gateway-package-{spec.target}.tar.gz"
            _make_fake_executable(entrypoint)

            prepare_package_dir(package_dir, force=False)
            build_package_dir(package_dir, "0.1.0", spec, entrypoint, cargo_profile="release")
            write_archive(package_dir, archive_out, force=False)

            with tarfile.open(archive_out, "r:gz") as tar:
                names = tar.getnames()

            expected_names = {
                f"bin/{BIN_NAME}",
                METADATA_FILENAME,
                *PACKAGE_ASSET_FILENAMES,
            }
            assert expected_names.issubset(set(names))

            # Explicitly verify the packaged Python helper and wrapper scripts
            assert "commands/gateway/status.md" in names, "Gateway status asset missing from archive"
            assert "homebrew/post_install.py" in names, "Python helper missing from archive"
            assert "bin/cldg" in names, "cldg wrapper missing from archive"
            assert "bin/clddg" in names, "clddg wrapper missing from archive"
            assert "bin/cld-gateway-sh" in names, "cld-gateway-sh wrapper missing from archive"

    def test_archive_wrapper_scripts_are_executable(self) -> None:
        """Test that wrapper scripts in the archive are executable."""
        spec = TARGET_SPECS["aarch64-apple-darwin"]
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            package_dir = tmp_path / "pkg"
            entrypoint = tmp_path / "cld-gateway"
            archive_out = tmp_path / f"package-{spec.target}.tar.gz"
            _make_fake_executable(entrypoint)

            prepare_package_dir(package_dir, force=False)
            build_package_dir(package_dir, "0.1.0", spec, entrypoint, cargo_profile="release")
            write_archive(package_dir, archive_out, force=False)

            with tarfile.open(archive_out, "r:gz") as tar:
                for wrapper in ["bin/cld-gateway-sh", "bin/cldg", "bin/clddg"]:
                    member = tar.getmember(wrapper)
                    # Check executable bit is set in tarfile mode
                    assert member.mode & stat.S_IXUSR, f"Wrapper {wrapper} not executable in archive"

    def test_archive_post_install_helper_is_executable(self) -> None:
        """Test that post_install.py helper script in the archive is executable."""
        spec = TARGET_SPECS["x86_64-apple-darwin"]
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            package_dir = tmp_path / "pkg"
            entrypoint = tmp_path / "cld-gateway"
            archive_out = tmp_path / f"package-{spec.target}.tar.gz"
            _make_fake_executable(entrypoint)

            prepare_package_dir(package_dir, force=False)
            build_package_dir(package_dir, "0.1.0", spec, entrypoint, cargo_profile="release")
            write_archive(package_dir, archive_out, force=False)

            with tarfile.open(archive_out, "r:gz") as tar:
                member = tar.getmember("homebrew/post_install.py")
                # Check executable bit is set in tarfile mode
                assert member.mode & stat.S_IXUSR, "post_install.py not executable in archive"


class TestWorkspaceVersionConsistency:
    """Regression tests for version consistency across the workspace."""

    def test_version_read_from_version_file(self) -> None:
        version = read_workspace_version()
        assert isinstance(version, str)
        assert len(version) > 0

    def test_version_follows_semver_with_optional_prerelease(self) -> None:
        version = read_workspace_version()
        pattern = r"^\d+\.\d+\.\d+(?:-(?:alpha|beta)(?:\.\d+)?)?$"
        assert re.match(pattern, version), (
            f"Version {version} does not follow semantic versioning"
        )


def _make_fake_executable(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"#!/bin/sh\necho fake\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


if __name__ == "__main__":
    import pytest

    sys.exit(pytest.main([__file__, "-v"]))
