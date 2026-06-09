#!/usr/bin/env python3
"""Post-install hook for cld-gateway Homebrew formula.

Handles initialization of ~/.gateway and ~/.claude_codex directories,
copies configuration files, and symlinks Claude Code entries.
"""

from __future__ import annotations

import sys
from pathlib import Path

# Shared Claude Code entries to symlink from ~/.claude into ~/.claude_codex
SHARED_CLAUDE_ENTRIES = (
    '.claude.json',
    'CLAUDE.md',
    'agents',
    'commands',
    'debug',
    'docs',
    'downloads',
    'history.jsonl',
    'hookify.block-direct-go-commands.local.md',
    'hookify.block-git-add-all.local.md',
    'hookify.block-no-verify-commit.local.md',
    'hooks',
    'ide',
    'output-styles',
    'plans',
    'plugins',
    'projects',
    'session-env',
    'shell-snapshots',
    'skills',
    'statusline-command.sh',
    'todos',
    'universal_instructions.md',
)


def validate_symlink_target(symlink_path: Path) -> None:
    """Validate that a symlink target exists and is a directory.

    Args:
        symlink_path: Path to the symlink to validate.

    Raises:
        SystemExit: If symlink target doesn't exist or is not a directory.
    """
    if not symlink_path.is_symlink():
        return

    target = symlink_path.resolve()
    if not target.exists():
        sys.exit(f'Expected {symlink_path} symlink target to exist: {target}')
    if not target.is_dir():
        sys.exit(f'Expected {symlink_path} symlink target to be a directory: {target}')


def ensure_claude_codex_path(claude_codex_path: Path) -> None:
    """Ensure ~/.claude_codex exists and is valid.

    Handles four cases:
    - symlink: validate target exists and is directory
    - directory: use as-is
    - missing: create it
    - file/other: error

    Args:
        claude_codex_path: Path to ~/.claude_codex.

    Raises:
        SystemExit: If claude_codex_path is invalid.
    """
    if claude_codex_path.is_symlink():
        validate_symlink_target(claude_codex_path)
    elif claude_codex_path.exists():
        if not claude_codex_path.is_dir():
            sys.exit(f'Expected {claude_codex_path} to be a directory or symlink')
    else:
        claude_codex_path.mkdir(parents=True, exist_ok=True)


def copy_file(src: Path, dst: Path) -> None:
    """Copy a file from src to dst.

    Args:
        src: Source file path.
        dst: Destination file path.

    Raises:
        SystemExit: If source doesn't exist or copy fails.
    """
    if not src.exists():
        sys.exit(f'Source file does not exist: {src}')

    try:
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(src.read_text(encoding='utf-8'), encoding='utf-8')
    except OSError as exc:
        sys.exit(f'Failed to copy {src} to {dst}: {exc}')


def validate_claude_home(claude_home: Path) -> None:
    """Validate that ~/.claude exists as a directory.

    Args:
        claude_home: Path to ~/.claude.

    Raises:
        SystemExit: If ~/.claude doesn't exist or isn't a directory.
    """
    if not claude_home.is_dir():
        sys.exit(f'Expected {claude_home} to exist as a directory')


def create_symlinks(
    claude_home: Path,
    claude_codex_path: Path,
) -> None:
    """Create symlinks from ~/.claude/* to ~/.claude_codex/* for missing targets.

    For each entry in SHARED_CLAUDE_ENTRIES:
    - Skip if source doesn't exist (including broken symlinks)
    - Skip if target already exists (including symlinks)
    - Otherwise create symlink from source to target

    Args:
        claude_home: Path to ~/.claude.
        claude_codex_path: Path to ~/.claude_codex.

    Raises:
        SystemExit: If symlink creation fails.
    """
    for entry_name in SHARED_CLAUDE_ENTRIES:
        source = claude_home / entry_name
        target = claude_codex_path / entry_name

        # Check if source exists (resolves symlinks to check actual existence)
        source_present = source.exists() or source.is_symlink()
        if not source_present:
            continue

        # Skip if target already exists
        if target.exists() or target.is_symlink():
            continue

        # Create symlink
        try:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(source)
        except OSError as exc:
            sys.exit(f'Failed to create symlink {target} -> {source}: {exc}')


def post_install(
    user_home: str,
    gateway_config_src: str,
    settings_json_src: str,
) -> None:
    """Execute post-install operations.

    Args:
        user_home: Path to user's home directory.
        gateway_config_src: Path to source config.yml (from pkgshare).
        settings_json_src: Path to source settings.json (from pkgshare).

    Raises:
        SystemExit: On any validation or operation failure.
    """
    user_home_path = Path(user_home).expanduser().resolve()
    gateway_home = user_home_path / '.gateway'
    claude_home = user_home_path / '.claude'
    claude_codex_path = user_home_path / '.claude_codex'

    gateway_config_dst = gateway_home / 'config.yml'
    settings_json_dst = claude_codex_path / 'settings.json'

    # Create ~/.gateway directory
    try:
        gateway_home.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        sys.exit(f'Failed to create {gateway_home}: {exc}')

    # Validate/handle ~/.claude_codex
    ensure_claude_codex_path(claude_codex_path)

    # Copy config.yml to ~/.gateway/config.yml
    copy_file(Path(gateway_config_src), gateway_config_dst)

    # Copy settings.json to ~/.claude_codex/settings.json
    copy_file(Path(settings_json_src), settings_json_dst)

    # Validate ~/.claude exists as directory
    validate_claude_home(claude_home)

    # Create symlinks for shared Claude entries
    create_symlinks(claude_home, claude_codex_path)


if __name__ == '__main__':
    if len(sys.argv) != 4:
        sys.exit(
            'Usage: post_install.py <user_home> <gateway_config_src> <settings_json_src>'
        )

    post_install(sys.argv[1], sys.argv[2], sys.argv[3])
