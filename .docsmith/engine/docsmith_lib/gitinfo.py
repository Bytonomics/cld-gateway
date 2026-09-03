"""Best-effort git metadata helpers for docsmith.

Everything here degrades to None on failure (no git binary, not a repo,
untracked file, packed refs, worktree gitdir files, ...). Callers must
treat None as "unknown", never as an error.
"""

import re
import subprocess
from pathlib import Path
from typing import Optional

_SHA_RE = re.compile(r"[0-9a-f]{40,64}")

_GIT_TIMEOUT_SECONDS = 15


def _git_rev_parse_head(root: Path) -> Optional[str]:
    """Subprocess fallback for HEAD resolution."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=False,
            timeout=_GIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    sha = proc.stdout.strip()
    if proc.returncode == 0 and _SHA_RE.fullmatch(sha):
        return sha
    return None


def head_sha(root: Path) -> Optional[str]:
    """Return the current HEAD commit sha, or None if it cannot be resolved.

    Reads .git/HEAD directly (following a 'ref: <ref>' indirection to the
    loose ref file) and falls back to `git rev-parse HEAD` for anything the
    direct read cannot handle (packed refs, worktree gitdir files, ...).
    """
    head_file = Path(root) / ".git" / "HEAD"
    try:
        content = head_file.read_text(encoding="utf-8").strip()
    except OSError:
        content = ""

    if content.startswith("ref: "):
        ref = content[len("ref: "):].strip()
        ref_file = Path(root) / ".git" / ref
        try:
            sha = ref_file.read_text(encoding="utf-8").strip()
            if _SHA_RE.fullmatch(sha):
                return sha
        except OSError:
            pass  # loose ref absent (e.g. packed refs); fall back below.
    elif _SHA_RE.fullmatch(content):
        return content  # detached HEAD

    return _git_rev_parse_head(root)


def last_commit_ts(root: Path, rel_path: str) -> Optional[int]:
    """Unix timestamp of the last commit touching rel_path, or None for
    untracked files / errors."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(root), "log", "-1", "--format=%ct", "--", rel_path],
            capture_output=True,
            text=True,
            check=False,
            timeout=_GIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    output = proc.stdout.strip()
    if proc.returncode == 0 and output.isdigit():
        return int(output)
    return None


def last_commit_ts_many(root: Path, rel_paths: "list[str]") -> "dict[str, Optional[int]]":
    """last_commit_ts for each path in rel_paths."""
    return {rel_path: last_commit_ts(root, rel_path) for rel_path in rel_paths}
