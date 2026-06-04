"""Supported package targets for cld-gateway."""

import platform
import stat
from dataclasses import dataclass
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = Path(__file__).resolve().parents[3]


@dataclass(frozen=True)
class TargetSpec:
    target: str
    is_linux: bool

    @property
    def exe_suffix(self) -> str:
        return ""  # macOS and Linux only for v1


TARGET_SPECS: dict[str, TargetSpec] = {
    "aarch64-apple-darwin": TargetSpec(
        target="aarch64-apple-darwin",
        is_linux=False,
    ),
    "x86_64-apple-darwin": TargetSpec(
        target="x86_64-apple-darwin",
        is_linux=False,
    ),
    "aarch64-unknown-linux-musl": TargetSpec(
        target="aarch64-unknown-linux-musl",
        is_linux=True,
    ),
    "x86_64-unknown-linux-musl": TargetSpec(
        target="x86_64-unknown-linux-musl",
        is_linux=True,
    ),
}


HOST_RELEASE_TARGETS: dict[tuple[str, str], str] = {
    ("darwin", "aarch64"): "aarch64-apple-darwin",
    ("darwin", "x86_64"): "x86_64-apple-darwin",
    ("linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("linux", "x86_64"): "x86_64-unknown-linux-musl",
}


def default_target() -> str:
    system = platform.system().lower()
    machine = _normalize_machine(platform.machine())
    target = HOST_RELEASE_TARGETS.get((system, machine))
    if target is None:
        supported = ", ".join(sorted(TARGET_SPECS))
        raise RuntimeError(
            f"Unsupported host platform {platform.system()}/{platform.machine()}. "
            f"Pass --target explicitly. Supported targets: {supported}"
        )
    return target


def resolve_input_path(explicit_path: Path, description: str, flag_name: str) -> Path:
    path = explicit_path.resolve()
    if not path.is_file():
        raise RuntimeError(f"{description} does not exist: {path}")
    if not _is_executable(path):
        raise RuntimeError(f"{description} is not executable: {path}")
    return path


def _is_executable(path: Path) -> bool:
    return bool(path.stat().st_mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH))


def _normalize_machine(machine: str) -> str:
    machine = machine.lower()
    if machine in ("amd64", "x86_64"):
        return "x86_64"
    if machine in ("aarch64", "arm64"):
        return "aarch64"
    return machine
