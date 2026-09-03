# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Collector that copies markdown files from a directory tree or file list.

This module implements the `markdown_tree` collector referenced in
`docs-site/sources.yml`. It is the "HOW" for two kinds of source entries:

- Directory mode: `source['path']` points at a directory (relative to the
  repo root) that is walked recursively for `.md` files, optionally
  following symlinks (`source['follow_symlinks']`) and filtered by
  `include`/`exclude` glob patterns.
- Discrete-files mode: `source['paths']` lists individual file paths
  (relative to the repo root) to collect verbatim, flattened into the
  destination directory (no nested structure preserved).

Collected files are copied immediately into
`autogen_docs_dir / source['destination']`, preserving relative directory
structure in directory mode. For each collected file a source-map entry
dict is returned describing where it came from, where it was written, its
extracted title, and its OKF `type` (if any). The dict shape returned here
is a contract consumed by downstream pipeline code (navigation and
provenance builders):

    {
        'source_id': str,
        'source_path': str,
        'output_path': str,
        'title': str,
        'okf_type': str | None,
    }
"""

from __future__ import annotations

import fnmatch
import os
import re
import shutil
from dataclasses import asdict, dataclass
from pathlib import Path

import yaml

H1_HEADING_PATTERN = re.compile(r'^#\s+(.+)$', re.MULTILINE)
FRONTMATTER_DELIMITER = '---'
NUMERIC_PREFIX_PATTERN = re.compile(r'^\d+-')

# README.md is deliberately NEVER collected by this module -- that is
# structurally `readme_discovery`'s job (it has collision-safe renaming
# logic for the many identically-named README.md files across the repo).
README_FILENAME = 'README.md'


@dataclass
class SourceMapEntry:
    """Describes one markdown file collected by this module.

    Field names intentionally mirror the source-map dict contract consumed
    by downstream pipeline code (navigation and provenance builders).
    """

    source_id: str
    source_path: str
    output_path: str
    title: str
    okf_type: str | None


def collect_markdown_tree(
    source: dict,
    repo_root: Path,
    autogen_docs_dir: Path,
    exclude_config: dict,
) -> list[dict]:
    """Collect markdown files described by a `sources.yml` entry.

    `source` is a plain dict because it is parsed directly from YAML and
    its shape varies by collector (some fields are collector-specific);
    it is not re-modeled as a dataclass here to avoid duplicating the
    YAML schema.

    Args:
        source: A single parsed source entry from `sources.yml`. Must
            contain `id` and `destination`, and either `path` (directory
            mode) or `paths` (discrete-files mode). May also contain
            `follow_symlinks`, `include`, and `exclude`.
        repo_root: Absolute path to the repository root that `path`/
            `paths` are relative to.
        autogen_docs_dir: Absolute path to the generated docs root that
            `destination` is relative to.
        exclude_config: The `exclude:` section from `sources.yml`
            (`dir_basenames`, `dir_paths`, `filenames` lists) -- the single
            shared blacklist, not maintained separately per collector.

    Returns:
        A list of source-map entry dicts, one per successfully collected
        file. Files that fail to copy or don't exist are skipped and
        logged via `print()` rather than raising.
    """
    candidate_files = _discover_candidate_files(source, repo_root, exclude_config)
    entries: list[SourceMapEntry] = []
    for source_path, relative_path in candidate_files:
        entry = _collect_single_file(source, source_path, relative_path, autogen_docs_dir)
        if entry is None:
            continue
        entries.append(entry)
    return [asdict(entry) for entry in entries]


def _discover_candidate_files(source: dict, repo_root: Path, exclude_config: dict) -> list[tuple[Path, Path]]:
    """Find files eligible for collection, before include/exclude filtering.

    Returns a list of (absolute_source_path, relative_path) tuples. The
    relative path is relative to the source's `path` in directory mode, or
    is just the filename in discrete-files mode.
    """
    if 'paths' in source:
        return _discover_discrete_files(source, repo_root, exclude_config)
    return _discover_directory_tree(source, repo_root, exclude_config)


def _discover_discrete_files(source: dict, repo_root: Path, exclude_config: dict) -> list[tuple[Path, Path]]:
    excluded_filenames = set(exclude_config.get('filenames', []))
    candidates: list[tuple[Path, Path]] = []
    for relative in source.get('paths', []):
        absolute = repo_root / relative
        if not absolute.exists():
            print(f'[markdown_tree] skipping missing file: {absolute}')
            continue
        if absolute.name in excluded_filenames:
            continue
        candidates.append((absolute, Path(absolute.name)))
    return candidates


def _discover_directory_tree(source: dict, repo_root: Path, exclude_config: dict) -> list[tuple[Path, Path]]:
    base_dir = repo_root / source['path']
    follow_symlinks = source.get('follow_symlinks', False)
    excluded_dir_basenames = set(exclude_config.get('dir_basenames', []))
    excluded_dir_paths = list(exclude_config.get('dir_paths', []))
    excluded_filenames = set(exclude_config.get('filenames', []))

    candidates: list[tuple[Path, Path]] = []
    for dirpath, dirnames, filenames in os.walk(base_dir, followlinks=follow_symlinks):
        current_dir = Path(dirpath)
        dirnames[:] = [
            d for d in dirnames
            if d not in excluded_dir_basenames
            and not _is_excluded_dir_path(current_dir / d, repo_root, excluded_dir_paths)
        ]
        for filename in filenames:
            if not filename.endswith('.md'):
                continue
            if filename == README_FILENAME or filename in excluded_filenames:
                continue
            absolute = current_dir / filename
            relative = absolute.relative_to(base_dir)
            candidates.append((absolute, relative))
    return candidates


def _is_excluded_dir_path(candidate_dir: Path, repo_root: Path, excluded_dir_paths: list[str]) -> bool:
    """Check whether candidate_dir (relative to repo_root) matches an excluded path prefix."""
    try:
        relative = candidate_dir.relative_to(repo_root).as_posix()
    except ValueError:
        return False
    return any(relative == excluded or relative.startswith(excluded + '/') for excluded in excluded_dir_paths)


def _matches_any_glob(relative_path: Path, patterns: list[str]) -> bool:
    path_str = str(relative_path)
    for pattern in patterns:
        if fnmatch.fnmatch(path_str, pattern):
            return True
    return False


def _passes_include_exclude(source: dict, relative_path: Path) -> bool:
    include_patterns = source.get('include')
    if include_patterns:
        if not _matches_any_glob(relative_path, include_patterns):
            return False
    exclude_patterns = source.get('exclude')
    if not exclude_patterns:
        return True
    if _matches_any_glob(relative_path, exclude_patterns):
        return False
    return True


def _resolve_destination_path(source: dict, relative_path: Path, autogen_docs_dir: Path) -> Path:
    if 'paths' in source:
        return autogen_docs_dir / source['destination'] / relative_path.name
    return autogen_docs_dir / source['destination'] / relative_path


def _read_and_copy_file(source_path: Path, destination_path: Path) -> str | None:
    """Read source content and copy the file to its destination.

    Returns the file's text content on success, or None if reading or
    copying failed (the failure is logged, not raised).
    """
    try:
        content = source_path.read_text(encoding='utf-8')
        destination_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source_path, destination_path)
        return content
    except OSError as error:
        print(f'[markdown_tree] failed to copy {source_path} -> {destination_path}: {error}')
        return None


def _split_frontmatter(content: str) -> tuple[str | None, str]:
    """Split a leading YAML frontmatter block from the rest of the content.

    A frontmatter block starts with a `---` line as literally the first
    line and ends at the next `---` line. Returns a tuple of
    (frontmatter_text_or_None, remaining_content).
    """
    lines = content.splitlines(keepends=True)
    if not lines:
        return None, content
    if lines[0].strip() != FRONTMATTER_DELIMITER:
        return None, content

    closing_index = None
    for index in range(1, len(lines)):
        if lines[index].strip() == FRONTMATTER_DELIMITER:
            closing_index = index
            break
    if closing_index is None:
        return None, content

    frontmatter_text = ''.join(lines[1:closing_index])
    remaining_content = ''.join(lines[closing_index + 1:])
    return frontmatter_text, remaining_content


def _title_from_filename(source_path: Path) -> str:
    stem = source_path.stem
    stem = NUMERIC_PREFIX_PATTERN.sub('', stem)
    stem = stem.replace('-', ' ').replace('_', ' ')
    return stem.title()


def _extract_title(content: str, source_path: Path) -> str:
    """Extract the first ATX H1 heading, falling back to the filename."""
    _frontmatter_text, body = _split_frontmatter(content)
    match = H1_HEADING_PATTERN.search(body)
    if match:
        return match.group(1).strip()
    return _title_from_filename(source_path)


def _extract_okf_type(content: str) -> str | None:
    """Extract the OKF `type` value from the file's YAML frontmatter."""
    frontmatter_text, _body = _split_frontmatter(content)
    if not frontmatter_text:
        return None
    try:
        parsed = yaml.safe_load(frontmatter_text)
    except yaml.YAMLError as error:
        print(f'[markdown_tree] failed to parse frontmatter: {error}')
        return None
    if not isinstance(parsed, dict):
        return None
    okf_type = parsed.get('type')
    if not isinstance(okf_type, str):
        return None
    return okf_type


def _collect_single_file(
    source: dict,
    source_path: Path,
    relative_path: Path,
    autogen_docs_dir: Path,
) -> SourceMapEntry | None:
    if not _passes_include_exclude(source, relative_path):
        return None

    destination_path = _resolve_destination_path(source, relative_path, autogen_docs_dir)
    content = _read_and_copy_file(source_path, destination_path)
    if content is None:
        return None

    title = _extract_title(content, source_path)
    okf_type = _extract_okf_type(content)

    return SourceMapEntry(
        source_id=source['id'],
        source_path=str(source_path),
        output_path=str(destination_path.relative_to(autogen_docs_dir)),
        title=title,
        okf_type=okf_type,
    )
