# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Internal link rewrite transform for collected documentation files.

This module rewrites relative Markdown links (`[text](target)`) inside an
already-collected file so that cross-source links keep working once files
have been moved into the flattened `autogen/docs/` tree. It relies on the
source-map (built once collection is complete, since only then do we know
every file's final destination path).

Link targets are classified before any rewrite is attempted:

- External links (`http://`, `https://`, `mailto:`) are left untouched.
- Anchor-only links (`#section`) are left untouched.
- Relative links that already resolve to an existing file inside the
  collected tree (relative to the file's own current location) are left
  untouched -- they already work.
- Relative links that do NOT resolve locally, but whose filename uniquely
  matches exactly one other collected source, are rewritten to a
  bundle-absolute path (`/<output_path>`), which is what this pipeline (and
  the OKF spec it follows) prefers over relative links.
- Relative links that cannot be resolved unambiguously are left untouched
  and reported back as unresolved warnings -- per the OKF spec, broken links
  are explicitly tolerated, not a conformance failure.
"""

import re
from pathlib import Path

_LINK_RE = re.compile(r'\[([^\]]*)\]\(([^)]+)\)')
_EXTERNAL_PREFIXES = ('http://', 'https://', 'mailto:')
# Markdown/document extensions. A link to one of these is a DOCUMENT link and is
# validated (a broken one still fails the strict build). Everything else is a
# non-document reference and is de-linked in the collected copy instead of
# erroring, so links to source code / config / not-yet-written planning targets
# never fail the build.
_DOC_EXTENSIONS = ('.md', '.markdown', '.mdx')


def apply_links_transform(
    file_path: Path,
    source_map: dict[str, dict],
    autogen_docs_dir: Path,
    dry_run: bool = False,
    *,
    current_source_path: str | None = None,
) -> list[str]:
    """Rewrite relative Markdown links in `file_path` using `source_map`.

    Args:
        file_path: Path to the already-collected markdown file, in place.
        source_map: Dict keyed by each entry's ORIGINAL `source_path`,
            mapping to that entry's full source-map dict (`source_id`,
            `source_path`, `output_path`, `title`, `okf_type`).
        autogen_docs_dir: Root of the collected/autogen docs tree, used to
            determine whether a relative link already resolves correctly
            within that tree.
        dry_run: If True, detect and report unresolved/rewritable links
            exactly as normal, but never write `file_path` -- used by
            `cmd_check`, which must only inspect the docs tree, never
            mutate it.

    Returns:
        A list of human-readable warning strings, one per unresolved
        relative link target found. Never raises: file I/O failures are
        captured as a single-item warning list instead.
    """
    try:
        content = file_path.read_text(encoding='utf-8')
    except OSError as exc:
        return [f'{file_path}: unable to read file for link transform ({exc})']

    warnings: list[str] = []
    changed = False
    resolved_autogen_root = autogen_docs_dir.resolve()
    source_dir = Path(current_source_path).resolve().parent if current_source_path else None

    def _delink(link_text: str, target_path: str) -> str:
        """Drop the link wrapper, keeping the visible text (or the target path
        as inline code when the text is empty)."""
        nonlocal changed
        changed = True
        return link_text if link_text else f'`{target_path}`'

    def _replace(match: re.Match) -> str:
        nonlocal changed

        link_text = match.group(1)
        link_target = match.group(2)

        if link_target.startswith(_EXTERNAL_PREFIXES):
            return match.group(0)
        if link_target.startswith('#'):
            return match.group(0)

        candidate = (file_path.parent / link_target).resolve()
        if candidate.exists() and candidate.is_relative_to(resolved_autogen_root):
            return match.group(0)

        # Split off any #anchor / ?query for path resolution.
        target_path = link_target.split('#', 1)[0].split('?', 1)[0]
        _, _, fragment = link_target.partition('#')
        anchor = f'#{fragment}' if fragment else ''

        if not target_path:
            return match.group(0)

        # 1) Precise: resolve against the ORIGINAL source location and, if the
        #    target is a collected doc, repoint to its in-site page. This fixes
        #    cross-file document links the flattening broke; they stay working
        #    links to the correct page.
        if source_dir is not None:
            resolved = (source_dir / target_path).resolve()
            entry = source_map.get(str(resolved))
            if entry is not None:
                changed = True
                return f'[{link_text}](/{entry["output_path"]}{anchor})'

        # 2) Fallback: unique filename match among collected docs.
        target_name = Path(target_path).name
        filename_matches = [
            entry for entry in source_map.values() if Path(entry['source_path']).name == target_name
        ]
        if len(filename_matches) == 1:
            changed = True
            return f'[{link_text}](/{filename_matches[0]["output_path"]}{anchor})'

        # 3) Not a collected doc. Classify by target type.
        extension = Path(target_path).suffix.lower()
        if extension in _DOC_EXTENSIONS:
            target_exists = source_dir is not None and (source_dir / target_path).resolve().exists()
            if target_exists:
                # A real document intentionally outside the built site (e.g. an
                # excluded CLAUDE.md). Keep the reference as text; do not error.
                return _delink(link_text, target_path)
            # A markdown target that exists nowhere is a genuinely broken
            # DOCUMENT link -- leave it so mkdocs --strict fails, and report it.
            warnings.append(f"{file_path}: unresolved document link target '{link_target}'")
            return match.group(0)

        # Non-document target (source code, config, extensionless path, or a
        # planning target not written yet) -- de-link; never an error.
        return _delink(link_text, target_path)

    new_content = _LINK_RE.sub(_replace, content)

    if not changed or dry_run:
        return warnings

    try:
        file_path.write_text(new_content, encoding='utf-8')
    except OSError as exc:
        return [f'{file_path}: unable to write file for link transform ({exc})']

    return warnings
