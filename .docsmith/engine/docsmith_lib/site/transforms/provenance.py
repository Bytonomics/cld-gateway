# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Provenance transform: append a source footer to collected docs.

This module appends a small, idempotent Markdown footer noting where a
collected file originally came from. It reads the file's current on-disk
content and only ever appends to the very end of the file -- it never touches
the frontmatter block or any other part of the body, and it does not assume
any particular transform ran before it.
"""

import re
from pathlib import Path

_FOOTER_PATTERN = re.compile(r'\n---\n\n\*Source: `[^`]*` \(collected via `[^`]*`\)\*\n?\Z')


def apply_provenance_transform(file_path: Path, source_map_entry: dict) -> None:
    """Append a provenance footer noting the file's collection source.

    Appends a footer of the form:

        ---

        *Source: `{source_path}` (collected via `{source_id}`)*

    to the end of the file body. Only the end of the file is modified;
    everything else (including any existing frontmatter block) is left
    untouched.

    This function is idempotent: if a matching provenance footer is already
    present at the end of the file (e.g. from a prior pipeline run that
    wasn't cleaned first), it does NOT append a second footer and leaves the
    file unchanged.

    Args:
        file_path: Path to the already-collected markdown file, in place.
        source_map_entry: Dict with at least 'source_path' and 'source_id'
            describing where this file was collected from.
    """
    content = file_path.read_text(encoding='utf-8')

    if _has_provenance_footer(content):
        return

    source_path = source_map_entry['source_path']
    source_id = source_map_entry['source_id']
    footer = f'\n---\n\n*Source: `{source_path}` (collected via `{source_id}`)*\n'

    new_content = content.rstrip('\n') + '\n' + footer
    file_path.write_text(new_content, encoding='utf-8')


def _has_provenance_footer(content: str) -> bool:
    """Check whether the content already ends with a provenance footer."""
    tail = content[-500:]
    return bool(_FOOTER_PATTERN.search(tail))
