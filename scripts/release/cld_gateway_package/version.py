"""Version discovery for cld-gateway packages."""

import re
from pathlib import Path

from .targets import REPO_ROOT


WORKSPACE_VERSION_PATTERN = re.compile(r'^version\s*=\s*"([^"]+)"')


def read_workspace_version() -> str:
    cargo_toml = REPO_ROOT / "Cargo.toml"
    in_workspace_package = False
    with open(cargo_toml, encoding="utf-8") as fh:
        for line in fh:
            stripped = line.strip()
            if stripped == "[workspace.package]":
                in_workspace_package = True
                continue
            if in_workspace_package and stripped.startswith("["):
                break
            if in_workspace_package:
                match = WORKSPACE_VERSION_PATTERN.match(stripped)
                if match is not None:
                    return match.group(1)
    raise RuntimeError(
        f"Could not find [workspace.package].version in {cargo_toml}"
    )
