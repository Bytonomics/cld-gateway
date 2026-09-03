# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Frontmatter transform: stamp collection metadata onto collected docs.

This module adds `source_path` / `source_id` provenance keys to the YAML
frontmatter of an already-collected markdown file, without disturbing any of
the OKF v0.2 frontmatter fields (`type`, `title`, `status`, `tags`,
`description`, `sources`, `generated`, `verified`, `stale_after`, or any other
pre-existing key) that a prior one-time migration already wrote into the
source docs.
"""

from pathlib import Path
from typing import Any, Dict, Tuple

import yaml


def apply_frontmatter_transform(file_path: Path, source_map_entry: dict) -> None:
    """Add collection metadata to a collected markdown file's frontmatter.

    Contract (load-bearing -- do not weaken): this function PRESERVES every
    existing frontmatter key exactly as parsed. It never overwrites, renames,
    or removes `type`, `title`, `status`, `tags`, `description`, `sources`,
    `generated`, `verified`, `stale_after`, or any other pre-existing key. The
    ONLY keys this function ever adds or overwrites are `source_path` and
    `source_id`, which are (re)stamped with the current collection metadata on
    every run so that a rebuild always reflects the latest collection source.
    If the file has no existing frontmatter block, an empty frontmatter dict
    is used as the starting point (defensive fallback -- OKF-migrated source
    docs are expected to already carry frontmatter).

    Args:
        file_path: Path to the already-collected markdown file, in place.
        source_map_entry: Dict with at least 'source_path' and 'source_id'
            describing where this file was collected from.
    """
    content = file_path.read_text(encoding='utf-8')
    frontmatter, body = _split_frontmatter(content)

    frontmatter['source_path'] = source_map_entry['source_path']
    frontmatter['source_id'] = source_map_entry['source_id']

    frontmatter_block = yaml.safe_dump(frontmatter, default_flow_style=False, sort_keys=False)
    new_content = f'---\n{frontmatter_block}---\n{body}'
    file_path.write_text(new_content, encoding='utf-8')


def _split_frontmatter(content: str) -> Tuple[Dict[str, Any], str]:
    """Split raw file content into a frontmatter dict and the remaining body.

    A frontmatter block is a `---` delimiter as literally the first line of
    the file, followed by YAML, followed by a line that is exactly `---`.
    If no such block is present, returns an empty dict and the full original
    content as the body (never raises).
    """
    lines = content.split('\n')
    if not lines or lines[0].strip() != '---':
        return {}, content

    end_index = None
    for index in range(1, len(lines)):
        if lines[index].strip() == '---':
            end_index = index
            break

    if end_index is None:
        return {}, content

    yaml_block = '\n'.join(lines[1:end_index])
    body = '\n'.join(lines[end_index + 1:])

    parsed = yaml.safe_load(yaml_block)
    if not isinstance(parsed, dict):
        parsed = {}

    return parsed, body
