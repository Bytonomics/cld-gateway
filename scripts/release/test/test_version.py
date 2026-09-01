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


def test_read_workspace_version_matches_version_file() -> None:
    version_file = REPO_ROOT / "VERSION"
    expected = version_file.read_text(encoding="utf-8").strip()

    assert expected, "VERSION file is empty"
    assert read_workspace_version() == expected


if __name__ == "__main__":
    test_read_workspace_version_returns_semver()
    test_read_workspace_version_matches_version_file()
    print("All tests passed.")
