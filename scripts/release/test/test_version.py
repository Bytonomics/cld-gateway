"""Tests for cld_gateway_package.version."""

import re
import sys
from pathlib import Path

# Allow running directly from the scripts/ directory.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from cld_gateway_package.targets import REPO_ROOT
from cld_gateway_package.version import read_workspace_version


def test_read_workspace_version_returns_semver() -> None:
    version = read_workspace_version()
    assert re.match(r"^\d+\.\d+\.\d+", version), (
        f"Expected a semver version, got: {version!r}"
    )


def test_read_workspace_version_matches_cargo_toml() -> None:
    cargo_toml = REPO_ROOT / "Cargo.toml"
    content = cargo_toml.read_text(encoding="utf-8")

    # Extract version from [workspace.package] section manually.
    in_section = False
    expected: str | None = None
    for line in content.splitlines():
        stripped = line.strip()
        if stripped == "[workspace.package]":
            in_section = True
            continue
        if in_section and stripped.startswith("["):
            break
        if in_section:
            m = re.match(r'^version\s*=\s*"([^"]+)"', stripped)
            if m:
                expected = m.group(1)
                break

    assert expected is not None, "Could not find version in [workspace.package]"
    assert read_workspace_version() == expected


if __name__ == "__main__":
    test_read_workspace_version_returns_semver()
    test_read_workspace_version_matches_cargo_toml()
    print("All tests passed.")
