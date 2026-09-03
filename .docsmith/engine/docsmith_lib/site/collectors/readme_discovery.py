# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""README discovery collector for the docs-site autogen pipeline.

This collector recursively searches a configured list of root directories
(relative to the repo root) for files literally named ``README.md``. Because
many such files share the exact same filename, they cannot be copied flat
into the autogen docs tree without colliding. Instead, each discovered
``README.md`` is copied to a destination path derived from its parent
directory path, with the final path segment becoming the new ``.md``
filename and all preceding segments preserved as nested subdirectories
(with underscores hyphenated for readability).

Example:
    ``cloud_backend/dataplane-api/README.md``
    -> ``readmes/cloud-backend/dataplane-api.md``

    ``python/README.md`` (single parent segment, no subdirectory)
    -> ``readmes/python.md``

    ``smritea-oss/oss_backend/smritea-oss-core/README.md``
    -> ``readmes/smritea-oss/oss-backend/smritea-oss-core.md``
"""

import fnmatch
import os
import re
import shutil
from pathlib import Path

import yaml

_FRONTMATTER_DELIMITER = '---'
_ATX_H1_PATTERN = re.compile(r'^#\s+(.+?)\s*$', re.MULTILINE)
_NUMERIC_PREFIX_PATTERN = re.compile(r'^\d+[-_.]*')
_README_FILENAME = 'README.md'


def collect_readme_discovery(source: dict, repo_root: Path, autogen_docs_dir: Path, exclude_config: dict) -> list[dict]:
    """Discover README.md files under configured roots and copy them into the autogen docs tree.

    Args:
        source: Source configuration dict with keys ``id``, ``paths``,
            ``destination``, and ``exclude``.
        repo_root: Absolute path to the repository root.
        autogen_docs_dir: Absolute path to the autogen docs output directory.
        exclude_config: The `exclude:` section from `sources.yml`
            (`dir_basenames`, `dir_paths`, `filenames` lists) -- the single
            shared blacklist, not maintained separately per collector.

    Returns:
        A list of dicts, one per successfully collected README, each with
        keys: ``source_id``, ``source_path``, ``output_path``, ``title``,
        ``okf_type``.
    """
    source_id = source['id']
    root_paths = source['paths']
    destination_subdir = source['destination']
    exclude_patterns = source.get('exclude', [])
    excluded_dir_basenames = set(exclude_config.get('dir_basenames', []))
    excluded_dir_paths = list(exclude_config.get('dir_paths', []))

    collected: list[dict] = []
    for root_path in root_paths:
        root_dir = repo_root / root_path
        collected.extend(
            _collect_from_root(
                source_id=source_id,
                root_dir=root_dir,
                repo_root=repo_root,
                autogen_docs_dir=autogen_docs_dir,
                destination_subdir=destination_subdir,
                exclude_patterns=exclude_patterns,
                excluded_dir_basenames=excluded_dir_basenames,
                excluded_dir_paths=excluded_dir_paths,
            )
        )
    return collected


def _collect_from_root(
    source_id: str,
    root_dir: Path,
    repo_root: Path,
    autogen_docs_dir: Path,
    destination_subdir: str,
    exclude_patterns: list[str],
    excluded_dir_basenames: set[str],
    excluded_dir_paths: list[str],
) -> list[dict]:
    """Walk a single root directory and collect all README.md files under it."""
    if not root_dir.exists():
        print(f'[readme_discovery] root path does not exist, skipping: {root_dir}')
        return []

    collected: list[dict] = []
    for dirpath, dirnames, filenames in os.walk(root_dir, followlinks=False):
        current_dir = Path(dirpath)
        _prune_excluded_dirs(
            current_dir, dirnames, repo_root, exclude_patterns, excluded_dir_basenames, excluded_dir_paths
        )

        if _README_FILENAME not in filenames:
            continue

        readme_path = current_dir / _README_FILENAME
        relative_readme_path = readme_path.relative_to(repo_root)
        if _is_excluded(relative_readme_path, exclude_patterns):
            continue

        item = _collect_single_readme(
            source_id=source_id,
            readme_path=readme_path,
            relative_readme_path=relative_readme_path,
            autogen_docs_dir=autogen_docs_dir,
            destination_subdir=destination_subdir,
        )
        if item is not None:
            collected.append(item)

    return collected


def _prune_excluded_dirs(
    current_dir: Path,
    dirnames: list[str],
    repo_root: Path,
    exclude_patterns: list[str],
    excluded_dir_basenames: set[str],
    excluded_dir_paths: list[str],
) -> None:
    """Remove excluded subdirectories from dirnames in-place so os.walk skips them."""
    kept_dirnames = []
    for dirname in dirnames:
        if dirname in excluded_dir_basenames:
            continue
        candidate_dir = current_dir / dirname
        relative_candidate_dir = candidate_dir.relative_to(repo_root)
        if _is_excluded(relative_candidate_dir, exclude_patterns):
            continue
        if _is_excluded_dir_path(candidate_dir, repo_root, excluded_dir_paths):
            continue
        kept_dirnames.append(dirname)
    dirnames[:] = kept_dirnames


def _is_excluded_dir_path(candidate_dir: Path, repo_root: Path, excluded_dir_paths: list[str]) -> bool:
    """Check whether candidate_dir (relative to repo_root) matches an excluded path prefix."""
    try:
        relative = candidate_dir.relative_to(repo_root).as_posix()
    except ValueError:
        return False
    return any(relative == excluded or relative.startswith(excluded + '/') for excluded in excluded_dir_paths)


def _is_excluded(relative_path: Path, exclude_patterns: list[str]) -> bool:
    """Check whether a repo-root-relative path matches any exclude glob pattern."""
    relative_path_str = str(relative_path)
    for pattern in exclude_patterns:
        if fnmatch.fnmatch(relative_path_str, pattern):
            return True
    return False


def _collect_single_readme(
    source_id: str,
    readme_path: Path,
    relative_readme_path: Path,
    autogen_docs_dir: Path,
    destination_subdir: str,
) -> dict | None:
    """Copy a single README.md to its derived destination and extract metadata."""
    destination_path = _derive_destination_path(relative_readme_path, autogen_docs_dir, destination_subdir)

    try:
        destination_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(readme_path, destination_path)
        content = readme_path.read_text(encoding='utf-8')
    except OSError as exc:
        print(f'[readme_discovery] failed to copy {readme_path}: {exc}')
        return None

    _, body = _split_frontmatter(content)
    okf_type = _extract_okf_type(content)
    parent_segment = relative_readme_path.parent.parts[-1] if relative_readme_path.parent.parts else ''
    title = _extract_title(body) or _fallback_title(parent_segment)

    output_path = destination_path.relative_to(autogen_docs_dir)
    return {
        'source_id': source_id,
        'source_path': str(readme_path),
        'output_path': str(output_path),
        'title': title,
        'okf_type': okf_type,
    }


def _derive_destination_path(relative_readme_path: Path, autogen_docs_dir: Path, destination_subdir: str) -> Path:
    """Derive the destination path for a README.md based on its parent directory path.

    All path segments of the parent directory except the last are preserved
    as nested subdirectories (with underscores hyphenated). The last segment
    becomes the new filename stem (with a .md extension), and the original
    README.md filename is discarded.
    """
    relative_parent = relative_readme_path.parent
    parts = relative_parent.parts

    if not parts:
        # README.md directly at repo_root (no parent segments at all).
        return autogen_docs_dir / destination_subdir / 'index.md'

    dir_parts = [part.replace('_', '-') for part in parts[:-1]]
    filename_stem = parts[-1]

    if not dir_parts:
        return autogen_docs_dir / destination_subdir / f'{filename_stem}.md'

    return autogen_docs_dir / destination_subdir / Path(*dir_parts) / f'{filename_stem}.md'


def _split_frontmatter(content: str) -> tuple[dict | None, str]:
    """Split leading YAML frontmatter (delimited by ---) from the Markdown body."""
    if not content.startswith(_FRONTMATTER_DELIMITER):
        return None, content

    lines = content.splitlines()
    closing_index = None
    for index in range(1, len(lines)):
        if lines[index].strip() == _FRONTMATTER_DELIMITER:
            closing_index = index
            break

    if closing_index is None:
        return None, content

    frontmatter_text = '\n'.join(lines[1:closing_index])
    body = '\n'.join(lines[closing_index + 1:])

    try:
        frontmatter = yaml.safe_load(frontmatter_text)
    except yaml.YAMLError:
        return None, body

    if not isinstance(frontmatter, dict):
        return None, body

    return frontmatter, body


def _extract_okf_type(content: str) -> str | None:
    """Extract the frontmatter 'type' key, or None if absent/unparseable."""
    frontmatter, _ = _split_frontmatter(content)
    if not frontmatter:
        return None
    okf_type = frontmatter.get('type')
    if not isinstance(okf_type, str):
        return None
    return okf_type


def _extract_title(body: str) -> str | None:
    """Extract the first Markdown ATX H1 heading from the document body."""
    match = _ATX_H1_PATTERN.search(body)
    if not match:
        return None
    return match.group(1).strip()


def _fallback_title(parent_segment: str) -> str:
    """Derive a fallback title from a parent directory segment name.

    Strips any leading numeric prefix (e.g. '01-', '02_'), replaces
    hyphens/underscores with spaces, and title-cases the result.
    """
    if not parent_segment:
        return 'Readme'

    without_prefix = _NUMERIC_PREFIX_PATTERN.sub('', parent_segment)
    spaced = without_prefix.replace('-', ' ').replace('_', ' ').strip()
    if not spaced:
        return 'Readme'

    return spaced.title()
