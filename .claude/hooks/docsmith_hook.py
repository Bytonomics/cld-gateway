#!/usr/bin/env python3
"""Generic docsmith PostToolUse hook for Claude Code.

Reads the PostToolUse JSON payload on stdin, locates the docsmith project
that owns the edited file (nearest ancestor directory containing
.docsmith/docmap.json), and — when the edited file is mapped to
documentation — prints the docsmith reminder to stderr and exits 2 so the
agent sees it. Every other outcome exits 0 silently: this hook must never
break an edit.

SELF-CONTAINED and stdlib-only (json, os, sys, pathlib): `docsmith
scaffold` copies this file into projects, where it must run on machines
without the docsmith skill installed. It may NOT import docsmith_lib.

Exit codes:
    0 - No mapped docs (or any condition where the hook stays silent)
    2 - Mapped docs found; reminder printed to stderr
"""

# Inlined from docsmith_lib.{docmap,hookmsg} — keep in sync

import json
import sys
from pathlib import Path

DOCMAP_REL = Path(".docsmith") / "docmap.json"
CONFIG_REL = Path(".docsmith") / "config.json"

_PROTOCOL = """ACTION REQUIRED — task-queue protocol:
1. If a task titled "[docsmith] update <doc path>" already exists in your task list, update its
   description to also cover this change. Otherwise create ONE task NOW, titled
   "[docsmith] update <doc path>", with a body listing: the code file you just edited and
   what changed (one line).
2. Do NOT write documentation now. Create the task, then IMMEDIATELY return to the work
   you were doing.
3. Before committing, offer the user to run /docsmith:update-docs to process pending doc tasks.

When the doc IS eventually updated, evergreen rules apply:
- Current-state only. Never append a changelog/history section.
- Rewrite invalidated sections wholesale; do not patch around stale sentences.
- Every claim must cite a real repo path or `path#Symbol` in backticks.
- Keep the index table under the H1 in sync with the sections.
- Skip entirely if your change is trivial (formatting, comments, renames with no behavior change)."""


def normalize_entries(value):
    """Normalize a docmap value into a list of {path, reason, message} dicts."""
    if not isinstance(value, list):
        return []

    normalized = []
    for entry in value:
        if isinstance(entry, str):
            if entry:
                normalized.append({"path": entry, "reason": "", "message": ""})
        elif isinstance(entry, dict):
            path = entry.get("path")
            if path:
                normalized.append({
                    "path": path,
                    "reason": entry.get("reason", ""),
                    "message": entry.get("message", ""),
                })
    return normalized


def find_matching_docs(rel_path, doc_map):
    """Entries from every matching docmap key (sorted key order; keys ending
    in '/' prefix-match, others exact-match), deduplicated by doc path
    preserving first-seen order."""
    mapping = doc_map
    if isinstance(doc_map.get("map"), dict):
        mapping = doc_map["map"]

    matched_entries = []
    for key in sorted(mapping.keys()):
        if key.endswith("/"):
            if rel_path.startswith(key):
                matched_entries.extend(normalize_entries(mapping[key]))
        elif rel_path == key:
            matched_entries.extend(normalize_entries(mapping[key]))

    deduped = []
    seen_paths = set()
    for entry in matched_entries:
        if entry["path"] not in seen_paths:
            deduped.append(entry)
            seen_paths.add(entry["path"])
    return deduped


def build_message(matched_docs, config=None):
    """Render the hook message (mirror of docsmith_lib.hookmsg.build)."""
    lines = [
        "DOCSMITH: you edited code that is mapped to documentation.",
        "",
        "Docs mapped to this file:",
    ]
    for entry in matched_docs:
        reason = entry.get("reason") or ""
        suffix = " ({})".format(reason) if reason else ""
        lines.append("  - {}{}".format(entry.get("path", ""), suffix))
    lines.append("")
    lines.append(_PROTOCOL)

    custom = [entry for entry in matched_docs if entry.get("message")]
    if custom:
        lines.append("")
        lines.append("--- CUSTOM UPDATE INSTRUCTIONS ---")
        for entry in custom:
            lines.append("")
            lines.append("For {}:".format(entry["path"]))
            for message_line in str(entry["message"]).split("\n"):
                lines.append("  {}".format(message_line))
    return "\n".join(lines)


def _find_project_root(resolved):
    """First ancestor of `resolved` (starting at its parent, walking up to
    and including the home directory, stopping after home or the fs root)
    that contains .docsmith/docmap.json. None when there is none."""
    home = Path.home().resolve()
    current = resolved.parent
    while True:
        if (current / DOCMAP_REL).is_file():
            return current
        if current == home or current.parent == current:
            return None
        current = current.parent


def _hook_enabled(root):
    """False only when the project config explicitly sets hook.enabled to
    false; a missing/unreadable config or key means enabled."""
    config_path = root / CONFIG_REL
    if not config_path.is_file():
        return True
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return True
    if not isinstance(config, dict):
        return True
    hook = config.get("hook")
    if isinstance(hook, dict) and hook.get("enabled") is False:
        return False
    return True


def main():
    try:
        payload = json.load(sys.stdin)
    except (OSError, ValueError):
        return 0
    if not isinstance(payload, dict):
        return 0

    tool_input = payload.get("tool_input")
    file_path = tool_input.get("file_path") if isinstance(tool_input, dict) else None
    if not file_path:
        return 0
    if str(file_path).endswith(".md"):
        return 0  # anti-loop: doc edits never re-trigger the doc reminder

    resolved = Path(file_path).resolve()
    root = _find_project_root(resolved)
    if root is None:
        return 0
    if not _hook_enabled(root):
        return 0

    try:
        data = json.loads((root / DOCMAP_REL).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return 0
    if not isinstance(data, dict) or data.get("version") != 1:
        return 0
    if not isinstance(data.get("map"), dict):
        return 0

    try:
        rel = resolved.relative_to(root).as_posix()
    except ValueError:
        return 0

    matched = find_matching_docs(rel, data["map"])
    if not matched:
        return 0

    print(build_message(matched), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
