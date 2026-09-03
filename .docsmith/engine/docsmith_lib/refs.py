"""Extraction and resolution of code references cited inside markdown docs.

A "ref" is a repo-relative path (optionally with a '#symbol' suffix) cited
in a doc, either inside an inline code span (`src/main.go#HandleThing`) or
as a markdown link target ([x](src/main.go)). Fenced code blocks are
ignored entirely. Resolution checks that the path exists under the project
root and, when a symbol is present, that the symbol occurs as a whole word
in the target file.
"""

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

_CODE_SPAN_RE = re.compile(r"`([^`\n]+)`")
_LINK_TARGET_RE = re.compile(r"\]\(([^)\s]+)\)")
_PATH_RE = re.compile(r"[A-Za-z0-9_@][A-Za-z0-9_.@/-]*")
_FILE_EXT_RE = re.compile(r"\.[A-Za-z0-9_]+$")

# Only these extensions make a slashed token a *file* reference. Anything else
# with a dot after a slash (e.g. `pkg/Type.Method`, `api/v1.0`) is a symbol or
# version, not a path. A dotted token WITHOUT a slash (`service.name`,
# `django.contrib.auth`, `prod.smritea.ai`, `100.64.1.2`) is never a path.
_KNOWN_EXTS = frozenset({
    ".go", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs",
    ".md", ".mdx", ".rst", ".txt",
    ".yaml", ".yml", ".json", ".toml", ".ini", ".cfg", ".conf", ".env",
    ".xml", ".html", ".css", ".scss", ".proto",
    ".sh", ".bash", ".zsh", ".sql",
    ".jinja", ".jinja2", ".j2", ".tmpl", ".tpl",
    ".mod", ".sum", ".work", ".lock",
    ".tf", ".bicep", ".dockerfile",
})


@dataclass
class Ref:
    """A single code reference cited in a doc."""

    path: str
    symbol: "str | None"
    line_no: int


def _qualifies_as_path(path: str) -> bool:
    """A candidate qualifies as a repo path iff it matches the path charset,
    is not a URL, contains a '/' (a real path separator), and is either a
    directory-style path (no file extension) or ends in a known code/doc
    extension. Requiring the '/' keeps dotted identifiers, config keys,
    hostnames, IPs and version numbers out; the extension allowlist keeps
    `pkg/Type.Method` and `api/v1.0` out."""
    if path.startswith(("http", "mailto:")):
        return False
    if not _PATH_RE.fullmatch(path):
        return False
    if "/" not in path:
        return False
    ext_match = _FILE_EXT_RE.search(path)
    if ext_match is None:
        return True  # directory-style path, e.g. cloud_backend/studio-api
    return ext_match.group(0).lower() in _KNOWN_EXTS


def extract(markdown_text: str) -> "list[Ref]":
    """Extract all code refs from markdown text.

    Fenced code blocks (``` fences, inclusive) are removed first; line
    numbers refer to the original text. Candidates come from inline code
    spans and markdown link targets (anchor-only '#...' targets skipped).
    Identical (path, symbol, line_no) triples are deduplicated.
    """
    refs: list[Ref] = []
    seen: set[tuple[str, "str | None", int]] = set()

    in_fence = False
    for line_no, line in enumerate(markdown_text.split("\n"), start=1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue

        candidates: list[str] = list(_CODE_SPAN_RE.findall(line))
        for target in _LINK_TARGET_RE.findall(line):
            if target.startswith("#"):
                continue  # in-page anchor, not a path
            candidates.append(target)

        for candidate in candidates:
            path, _, symbol_part = candidate.partition("#")
            symbol = symbol_part if symbol_part else None
            if not _qualifies_as_path(path):
                continue
            key = (path, symbol, line_no)
            if key in seen:
                continue
            seen.add(key)
            refs.append(Ref(path=path, symbol=symbol, line_no=line_no))

    return refs


def resolve(ref: Ref, root: Path, doc_rel_path: "str | None" = None) -> str:
    """Resolve a ref against the project root.

    When ``doc_rel_path`` (the citing doc's project-relative path) is given,
    the ref is resolved against that doc's own directory first; if something
    exists there it becomes the target. Otherwise the ref falls back to being
    resolved against the project root. This mirrors the links check, letting a
    doc that cites a path relative to its own location (e.g. a README under
    ``deployment/`` citing ``docs/HLD.md`` to mean ``deployment/docs/HLD.md``)
    resolve instead of drifting.

    Returns:
        "ok"             - path exists (and symbol, if any, was found)
        "missing_path"   - path does not exist under root
        "missing_symbol" - path exists but the symbol was not found (or the
                           target is not a file, so no symbol lookup is
                           possible)
    """
    target = Path(root) / ref.path.rstrip("/")
    if doc_rel_path is not None:
        doc_dir = (Path(root) / doc_rel_path).parent
        candidate = (doc_dir / ref.path.rstrip("/")).resolve()
        if candidate.exists():
            target = candidate
    if not target.exists():
        return "missing_path"
    if ref.symbol is None:
        return "ok"
    if not target.is_file():
        return "missing_symbol"

    pattern = rf"\b{re.escape(ref.symbol)}\b"
    try:
        proc = subprocess.run(
            ["grep", "-qE", pattern, str(target)],
            capture_output=True,
            check=False,
        )
        if proc.returncode == 0:
            return "ok"
        if proc.returncode == 1:
            return "missing_symbol"
        # returncode >= 2: grep itself errored; fall through to Python search.
    except OSError:
        pass  # grep unavailable; fall through to Python search.

    try:
        text = target.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return "missing_symbol"
    return "ok" if re.search(pattern, text) else "missing_symbol"
