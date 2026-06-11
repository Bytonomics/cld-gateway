#!/usr/bin/env python3
"""Post-install hook for cld-gateway Homebrew formula.

Handles initialization of ~/.gateway, ~/.claude_gateway, and ~/.codex_gateway
directories, copies configuration files, and symlinks Claude Code entries.
"""

from __future__ import annotations

import sys
from pathlib import Path

# Shared Claude Code entries to symlink from ~/.claude into ~/.claude_gateway
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

# Directory-style entries that should be created on the source side if missing
DIRECTORY_STYLE_ENTRIES = frozenset({
    'agents',
    'commands',
    'debug',
    'docs',
    'downloads',
    'hooks',
    'ide',
    'output-styles',
    'plans',
    'plugins',
    'projects',
    'session-env',
    'shell-snapshots',
    'skills',
    'todos',
})


def get_user_home() -> Path:
    """Get the user's home directory.

    Returns:
        Path to user's home directory.

    Raises:
        SystemExit: If home directory cannot be determined.
    """
    home = Path.home()
    if not home.exists():
        sys.exit(f'User home directory does not exist: {home}')
    return home


def get_gateway_home(user_home: Path) -> Path:
    """Get the gateway home directory path.

    Args:
        user_home: Path to user's home directory.

    Returns:
        Path to ~/.gateway.
    """
    return user_home / '.gateway'


def get_claude_home(user_home: Path) -> Path:
    """Get the Claude home directory path.

    Args:
        user_home: Path to user's home directory.

    Returns:
        Path to ~/.claude.
    """
    return user_home / '.claude'


def get_claude_gateway_home(user_home: Path) -> Path:
    """Get the Claude gateway home directory path.

    Args:
        user_home: Path to user's home directory.

    Returns:
        Path to ~/.claude_gateway.
    """
    return user_home / '.claude_gateway'


def get_codex_gateway_home(user_home: Path) -> Path:
    """Get the Codex gateway home directory path.

    Args:
        user_home: Path to user's home directory.

    Returns:
        Path to ~/.codex_gateway.
    """
    return user_home / '.codex_gateway'


def get_packaged_config_path() -> Path:
    """Get the path to the packaged config.yml file.

    The packaged config is located under the formula's pkgshare directory,
    derived from this installed helper path.

    Returns:
        Path to packaged config.yml.

    Raises:
        SystemExit: If packaged config cannot be located.
    """
    formula_prefix = Path(__file__).resolve().parents[1]
    config_path = formula_prefix / 'share' / 'cld-gateway' / 'config.yml'
    if not config_path.exists():
        sys.exit(f'Packaged config.yml not found at {config_path}')
    return config_path


def get_packaged_settings_path() -> Path:
    """Get the path to the packaged settings.json file.

    The packaged settings is located under the formula's pkgshare directory,
    derived from this installed helper path.

    Returns:
        Path to packaged settings.json.

    Raises:
        SystemExit: If packaged settings cannot be located.
    """
    formula_prefix = Path(__file__).resolve().parents[1]
    settings_path = formula_prefix / 'share' / 'cld-gateway' / 'settings.json'
    if not settings_path.exists():
        sys.exit(f'Packaged settings.json not found at {settings_path}')
    return settings_path


def get_packaged_codex_status_path() -> Path:
    """Get the path to the packaged Codex status markdown file.

    The packaged status file is located under the formula's pkgshare directory,
    derived from this installed helper path.

    Returns:
        Path to packaged commands/codex/status.md.

    Raises:
        SystemExit: If packaged status file cannot be located.
    """
    formula_prefix = Path(__file__).resolve().parents[1]
    status_path = (
        formula_prefix
        / 'share'
        / 'cld-gateway'
        / 'commands'
        / 'codex'
        / 'status.md'
    )
    if not status_path.exists():
        sys.exit(f'Packaged commands/codex/status.md not found at {status_path}')
    return status_path


def is_directory_style_entry(entry_name: str) -> bool:
    """Check if an entry is directory-style (not file-style).

    Args:
        entry_name: Name of the entry to check.

    Returns:
        True if the entry is directory-style, False otherwise.
    """
    return entry_name in DIRECTORY_STYLE_ENTRIES


def ensure_directory_or_symlink_root(path: Path, label: str) -> None:
    """Ensure a root directory exists and is valid.

    Handles four cases:
    - symlink: accept as-is (target validation deferred to usage)
    - directory: use as-is
    - missing: create it as a directory
    - file/other: error

    Args:
        path: Path to the root directory to validate/create.
        label: Human-readable label for error messages.

    Raises:
        SystemExit: If path is invalid.
    """
    if path.is_symlink():
        return
    if path.exists():
        if not path.is_dir():
            sys.exit(f'Expected {label} to be a directory or symlink, got: {path}')
        return
    try:
        path.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        sys.exit(f'Failed to create {label} at {path}: {exc}')


def copy_text_file(src: Path, dst: Path) -> None:
    """Copy a text file from src to dst.

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


def ensure_claude_source_root(claude_home: Path) -> None:
    """Ensure ~/.claude exists as a directory or symlink.

    Handles four cases:
    - symlink: accept as-is (target validation deferred to usage)
    - directory: use as-is
    - missing: create it as a directory
    - file/other: error

    Args:
        claude_home: Path to ~/.claude.

    Raises:
        SystemExit: If creation fails.
    """
    if claude_home.is_symlink():
        return
    if claude_home.exists():
        if not claude_home.is_dir():
            sys.exit(f'Expected {claude_home} to be a directory or symlink, got: {claude_home}')
        return
    try:
        claude_home.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        sys.exit(f'Failed to create {claude_home}: {exc}')


def ensure_source_entry_exists(claude_home: Path, entry_name: str) -> None:
    """Ensure a source-side entry exists if it is directory-style.

    For directory-style entries, create the directory when missing.
    For file-style entries, do nothing (do not auto-create files).

    Args:
        claude_home: Path to ~/.claude.
        entry_name: Name of the entry to ensure.

    Raises:
        SystemExit: If creation fails.
    """
    if not is_directory_style_entry(entry_name):
        return

    entry_path = claude_home / entry_name
    if entry_path.exists():
        return

    try:
        entry_path.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        sys.exit(f'Failed to create source entry {entry_path}: {exc}')


def ensure_target_entry_linked(
    claude_home: Path,
    claude_gateway_home: Path,
    entry_name: str,
) -> None:
    """Create or preserve a symlink for a shared entry.

    For each entry:
    - Skip if source doesn't exist (including broken symlinks)
    - Skip if target already exists (directory or symlink)
    - Create symlink from target to source

    Args:
        claude_home: Path to ~/.claude.
        claude_gateway_home: Path to ~/.claude_gateway.
        entry_name: Name of the entry to link.

    Raises:
        SystemExit: If symlink creation fails.
    """
    source = claude_home / entry_name
    target = claude_gateway_home / entry_name

    source_present = source.exists() or source.is_symlink()
    if not source_present:
        return

    if target.exists() or target.is_symlink():
        return

    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.symlink_to(source)
    except OSError as exc:
        sys.exit(f'Failed to create symlink {target} -> {source}: {exc}')


def sync_shared_claude_entries(claude_home: Path, claude_gateway_home: Path) -> None:
    """Sync shared Claude entries from ~/.claude to ~/.claude_gateway.

    For each entry in SHARED_CLAUDE_ENTRIES:
    - Ensure source-side directory-style entry exists
    - Create target symlink if source exists and target is missing

    Args:
        claude_home: Path to ~/.claude.
        claude_gateway_home: Path to ~/.claude_gateway.

    Raises:
        SystemExit: On any operation failure.
    """
    for entry_name in SHARED_CLAUDE_ENTRIES:
        ensure_source_entry_exists(claude_home, entry_name)
        ensure_target_entry_linked(claude_home, claude_gateway_home, entry_name)


def install_gateway_runtime_config(gateway_home: Path) -> None:
    """Install the gateway runtime configuration.

    Copies packaged config.yml to ~/.gateway/config.yml.

    Args:
        gateway_home: Path to ~/.gateway.

    Raises:
        SystemExit: If installation fails.
    """
    src = get_packaged_config_path()
    dst = gateway_home / 'config.yml'
    copy_text_file(src, dst)


def install_claude_gateway_settings(claude_gateway_home: Path) -> None:
    """Install the Claude gateway settings.

    Copies packaged settings.json to ~/.claude_gateway/settings.json.

    Args:
        claude_gateway_home: Path to ~/.claude_gateway.

    Raises:
        SystemExit: If installation fails.
    """
    src = get_packaged_settings_path()
    dst = claude_gateway_home / 'settings.json'
    copy_text_file(src, dst)


def install_codex_gateway_status(codex_gateway_home: Path) -> None:
    """Install the Codex gateway status command.

    Copies packaged commands/codex/status.md to
    ~/.codex_gateway/commands/codex/status.md.

    Args:
        codex_gateway_home: Path to ~/.codex_gateway.

    Raises:
        SystemExit: If installation fails.
    """
    src = get_packaged_codex_status_path()
    dst = codex_gateway_home / 'commands' / 'codex' / 'status.md'
    copy_text_file(src, dst)


def post_install() -> None:
    """Execute post-install operations with zero-argument invocation.

    Orchestrates:
    - User home discovery
    - Gateway home initialization
    - Claude gateway home validation/creation
    - Codex gateway home validation/creation
    - Runtime config installation
    - Claude gateway settings installation
    - Codex gateway status installation
    - Source-side Claude directory preparation
    - Shared entry symlink synchronization

    Raises:
        SystemExit: On any validation or operation failure.
    """
    user_home = get_user_home()
    gateway_home = get_gateway_home(user_home)
    claude_home = get_claude_home(user_home)
    claude_gateway_home = get_claude_gateway_home(user_home)
    codex_gateway_home = get_codex_gateway_home(user_home)

    ensure_directory_or_symlink_root(gateway_home, '~/.gateway')
    ensure_directory_or_symlink_root(claude_gateway_home, '~/.claude_gateway')
    ensure_directory_or_symlink_root(codex_gateway_home, '~/.codex_gateway')

    install_gateway_runtime_config(gateway_home)
    install_claude_gateway_settings(claude_gateway_home)
    install_codex_gateway_status(codex_gateway_home)

    ensure_claude_source_root(claude_home)
    sync_shared_claude_entries(claude_home, claude_gateway_home)


if __name__ == '__main__':
    if len(sys.argv) != 1:
        sys.exit('post_install.py takes no arguments')

    post_install()
