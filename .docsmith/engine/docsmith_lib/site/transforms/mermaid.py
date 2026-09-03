# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Mermaid fence-balance validator for collected documentation files.

This module is a *validator*, not a content transform: it does not modify
any collected markdown file and it does not attempt to parse or validate
Mermaid diagram syntax. It only checks that every fenced code block opened
with ```mermaid has a matching bare ``` closing fence somewhere below it in
the same file. An unclosed Mermaid fence silently breaks MkDocs rendering
(everything after the unclosed fence renders as one giant code block), so
this check exists to catch that failure mode before it ships.

It is meant to be invoked by the orchestrating `docs_kb.py` `check` command,
which walks the collected doc tree (`docs-site/autogen/docs/`) and calls
`validate_mermaid_fences()` on each markdown file, aggregating the returned
error messages across the whole tree.
"""

import re
from pathlib import Path

_MERMAID_OPEN_RE = re.compile(r'^```mermaid\s*$')
_FENCE_CLOSE = '```'


def validate_mermaid_fences(file_path: Path) -> list[str]:
    """Check that every ```mermaid fence in `file_path` is properly closed.

    Scans the file line by line. For each line that opens a ```mermaid
    fence, scans forward for the next bare ``` closing fence line. If the
    end of the file (or another ```mermaid opening) is reached first, the
    original fence is considered unclosed and an error message is recorded.

    Returns a list of human-readable error strings, one per unclosed fence.
    Returns an empty list if every ```mermaid fence is properly closed,
    including the trivial case of zero mermaid fences in the file.

    This function is read-only: it never modifies `file_path`. If the file
    cannot be read, the read failure is captured as a single error message
    instead of raising an exception.
    """
    try:
        content = file_path.read_text(encoding='utf-8')
    except (OSError, UnicodeDecodeError) as exc:
        return [f'{file_path}:1: unable to read file for mermaid fence validation ({exc})']

    lines = content.splitlines()
    errors: list[str] = []
    line_count = len(lines)
    index = 0

    while index < line_count:
        if not _MERMAID_OPEN_RE.match(lines[index].strip()):
            index += 1
            continue

        opening_line_number = index + 1
        scan_index = index + 1

        while scan_index < line_count:
            stripped_line = lines[scan_index].strip()
            if stripped_line == _FENCE_CLOSE:
                break
            if _MERMAID_OPEN_RE.match(stripped_line):
                break
            scan_index += 1

        fence_was_closed = (
            scan_index < line_count and lines[scan_index].strip() == _FENCE_CLOSE
        )
        if fence_was_closed:
            index = scan_index + 1
            continue

        errors.append(
            f"{file_path}:{opening_line_number}: unclosed ```mermaid fence "
            "(no matching closing ``` found)"
        )
        index = scan_index

    return errors
