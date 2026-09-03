"""Shared builder for the docsmith PostToolUse hook message (wave A4).

Single source of truth for the reminder printed when an edited code file
is mapped to documentation. Consumed by the hidden `hook-check` CLI
subcommand; the generic hook script (scripts/hook/docsmith_hook.py) must
stay stdlib-only and self-contained, so it carries an inlined copy of this
builder — any change here must be mirrored there.
"""

# The fixed protocol body. `<doc path>` is a literal placeholder the agent
# fills in when creating its task — it is NOT interpolated here.
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


def build(matched_docs: list[dict], config: dict | None = None) -> str:
    """Render the full hook message for a set of matched docmap entries.

    Args:
        matched_docs: Normalized entries ({path, reason, message}) as
            returned by docmap.find_matching_docs.
        config: Effective docsmith config (accepted for future use; the
            message text is currently config-independent).

    Returns:
        The message text (no trailing newline).
    """
    lines = [
        "DOCSMITH: you edited code that is mapped to documentation.",
        "",
        "Docs mapped to this file:",
    ]
    for entry in matched_docs:
        reason = entry.get("reason") or ""
        suffix = f" ({reason})" if reason else ""
        lines.append(f"  - {entry.get('path', '')}{suffix}")
    lines.append("")
    lines.append(_PROTOCOL)

    custom = [entry for entry in matched_docs if entry.get("message")]
    if custom:
        lines.append("")
        lines.append("--- CUSTOM UPDATE INSTRUCTIONS ---")
        for entry in custom:
            lines.append("")
            lines.append(f"For {entry['path']}:")
            for message_line in str(entry["message"]).split("\n"):
                lines.append(f"  {message_line}")
    return "\n".join(lines)
