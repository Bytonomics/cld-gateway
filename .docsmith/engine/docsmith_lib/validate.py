"""The `docsmith validate` command (wave A2).

Runs up to seven deterministic checks over two target sets:

    Category docs    Every .md file matching an ENABLED category's
                     dir_patterns, plus every .md under project.docs_dir
                     (category resolved via frontmatter `type` through
                     config.resolve_category). Directories named in
                     site.exclude.dir_basenames are skipped.
    Extra-gate files Files matched by the config's extra_gate_paths globs
                     that are not already category docs.

Both target sets exclude anything inside a git submodule (any path declared
in the project root's .gitmodules). A submodule is checked out as an
ordinary directory, so an unqualified glob like extra_gate_paths' `**/
README.md` would otherwise sweep in every submodule's own README and
project-level files (AGENTS.md, CLAUDE.md, etc.) too. Those files describe
paths relative to the submodule's own root, not this project's root, so
resolving them against this project's tree produces false passes (a path
that only "resolves" because the submodule happens to be checked out at
that location) and invites edits that silently rewrite a submodule's docs to
be superproject-relative. A submodule's own docs are validated by its own
docsmith run, not the superproject's.

Checks (config key in validate section / --only token):
    V1 frontmatter / frontmatter   frontmatter parse + field rules; V1b
                                   index-table warning for evergreen docs
                                   when validate.require_index_table
    V2 links       / links         relative markdown link targets resolve;
                                   #fragments match GitHub-slugged headings
    V3 drift       / drift         cited code refs (refs.extract) resolve
    V4 docmap      / docmap        docmap keys and entry paths exist
    V5 coverage    / coverage      every extra_gate_paths glob matches
    V6 plan-ttl    / plan_ttl      ephemeral docs older than ttl_days warn
    V7 decisions   / decisions     numbered-category filename, unique
                                   numbers, required H2 sections

V4/V5 are project-level checks: they run regardless of --paths filtering.
"""

import fnmatch
import glob
import json
import os
import re
import sys
import time
from pathlib import Path
from typing import Optional

from . import config as config_lib
from . import docmap as docmap_lib
from . import frontmatter_spec
from . import gitinfo
from . import refs as refs_lib
from .report import Report

# --only token -> config["validate"] key.
CHECK_TOKENS: dict[str, str] = {
    "frontmatter": "frontmatter",
    "links": "links",
    "drift": "drift",
    "docmap": "docmap",
    "coverage": "coverage",
    "plan-ttl": "plan_ttl",
    "decisions": "decisions",
}

_MD_LINK_RE = re.compile(r"\]\(([^)\s]+)\)")
_HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*$")
_SLUG_STRIP_RE = re.compile(r"[^a-z0-9\-_]")
# Inline code spans are masked before link scanning: a real markdown link
# never lives inside backticks, so `mgr.Register[Team](...)` (Go generics) or
# `arr[i](x)` must not be mis-read as a `](...)` link target.
_INLINE_CODE_RE = re.compile(r"`+[^`\n]*`+")

_DECISION_SECTIONS = ["Status", "Context", "Options Considered", "Decision", "Consequences"]

_SECONDS_PER_DAY = 86400


# --- shared helpers ----------------------------------------------------------


def _read_text(root: Path, rel_path: str) -> Optional[str]:
    """Read a project-relative file, or None when it cannot be read."""
    try:
        return (root / rel_path).read_text(encoding="utf-8")
    except OSError:
        return None


def _exclude_basenames(config: dict) -> set:
    return set(config.get("site", {}).get("exclude", {}).get("dir_basenames", []))


def _submodule_prefixes(root: Path) -> "list[str]":
    """Posix rel-path prefixes of every git submodule declared in the
    project root's .gitmodules ([]  when none). Read directly (no git
    subprocess), mirroring gitinfo.py's approach of reading git's own
    plumbing files rather than shelling out."""
    gitmodules_path = root / ".gitmodules"
    if not gitmodules_path.is_file():
        return []
    prefixes: list[str] = []
    for line in gitmodules_path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped.startswith("path"):
            continue
        _, _, value = stripped.partition("=")
        value = value.strip().strip("/")
        if value:
            prefixes.append(value)
    return prefixes


def _is_under_submodule(rel_path: str, submodule_prefixes: "list[str]") -> bool:
    return any(
        rel_path == prefix or rel_path.startswith(prefix + "/")
        for prefix in submodule_prefixes
    )


def _iter_markdown_files(
    root: Path, exclude_basenames: set, submodule_prefixes: "list[str]" = ()
) -> "list[str]":
    """All .md files under root as sorted posix rel paths, pruning any
    directory whose basename is in exclude_basenames or that is itself a
    declared git submodule."""
    found: list[str] = []
    # followlinks=True so a symlinked docs_dir (e.g. a repo whose docs/ points
    # at a shared bundle) is walked. The realpath-visited guard stops a
    # second symlink to the same tree from double-counting and breaks cycles.
    visited: set[str] = set()
    for dirpath, dirnames, filenames in os.walk(root, followlinks=True):
        real = os.path.realpath(dirpath)
        if real in visited:
            dirnames[:] = []
            continue
        visited.add(real)
        rel_dir = Path(dirpath).relative_to(root).as_posix()
        if rel_dir != "." and _is_under_submodule(rel_dir, submodule_prefixes):
            dirnames[:] = []
            continue
        dirnames[:] = sorted(d for d in dirnames if d not in exclude_basenames)
        for filename in filenames:
            if not filename.endswith(".md"):
                continue
            rel = (Path(dirpath) / filename).relative_to(root).as_posix()
            found.append(rel)
    return sorted(found)


def _headings(text: str) -> "list[tuple[int, str, int]]":
    """(level, text, line_no) for every ATX heading outside fenced blocks."""
    headings: list[tuple[int, str, int]] = []
    in_fence = False
    for line_no, line in enumerate(text.split("\n"), start=1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = _HEADING_RE.match(line)
        if match:
            headings.append((len(match.group(1)), match.group(2), line_no))
    return headings


def _slugify(heading: str) -> str:
    """GitHub-style anchor slug: lowercase, spaces to '-', strip everything
    outside [a-z0-9-_]."""
    return _SLUG_STRIP_RE.sub("", heading.strip().lower().replace(" ", "-"))


def _matches_any(rel_path: str, patterns: "list[str]") -> bool:
    return any(fnmatch.fnmatch(rel_path, pattern) for pattern in patterns)


def _glob_pattern_files(root: Path, pattern: str) -> "list[str]":
    """Posix rel paths of the FILES a posix glob (with **) matches under root."""
    matches = glob.glob(pattern, root_dir=str(root), recursive=True)
    return sorted(
        Path(match).as_posix()
        for match in matches
        if (root / match).is_file()
    )


# --- target-set builders -----------------------------------------------------


def category_docs(project_root: Path, config: dict) -> "list[tuple[str, Optional[str]]]":
    """(rel_path, category) for every category doc in the project.

    A file is a category doc when its posix rel path matches any ENABLED
    category's dir_patterns, or when it lives under project.docs_dir. The
    category is always resolved through config.resolve_category using the
    frontmatter `type` (best-effort parse), so it may be None for docs_dir
    files whose type resolves nowhere. Archive files are included (only V6
    skips them).
    """
    root = Path(project_root)
    categories: dict[str, dict] = config.get("categories", {})
    docs_dir = str(config.get("project", {}).get("docs_dir", "docs")).strip("/")

    docs: list[tuple[str, Optional[str]]] = []
    for rel_path in _iter_markdown_files(
        root, _exclude_basenames(config), _submodule_prefixes(root)
    ):
        matches_pattern = any(
            category.get("enabled", True)
            and any(
                config_lib._pattern_matches(rel_path, pattern)
                for pattern in category.get("dir_patterns", [])
            )
            for category in categories.values()
        )
        under_docs_dir = rel_path == docs_dir or rel_path.startswith(docs_dir + "/")
        if not (matches_pattern or under_docs_dir):
            continue

        fm_type: Optional[str] = None
        text = _read_text(root, rel_path)
        if text is not None:
            fm, _ = frontmatter_spec.parse_frontmatter(text)
            if fm is not None and isinstance(fm.get("type"), str):
                fm_type = fm["type"]
        docs.append((rel_path, config_lib.resolve_category(rel_path, fm_type, config)))
    return docs


def extra_gate_files(project_root: Path, config: dict) -> "list[str]":
    """Files matched by extra_gate_paths globs, de-duplicated, excluding
    anything already in category_docs, anything inside an excluded dir, and
    anything inside a git submodule."""
    root = Path(project_root)
    category_set = {rel_path for rel_path, _ in category_docs(root, config)}
    exclude_basenames = _exclude_basenames(config)
    submodule_prefixes = _submodule_prefixes(root)

    gated: list[str] = []
    seen: set = set(category_set)
    for pattern in config.get("extra_gate_paths", []):
        for rel_path in _glob_pattern_files(root, pattern):
            if rel_path in seen:
                continue
            if set(Path(rel_path).parts[:-1]) & exclude_basenames:
                continue
            if _is_under_submodule(rel_path, submodule_prefixes):
                continue
            seen.add(rel_path)
            gated.append(rel_path)
    return gated


# --- checks ------------------------------------------------------------------


def _check_frontmatter(
    report: Report,
    root: Path,
    config: dict,
    cat_docs: "list[tuple[str, Optional[str]]]",
) -> None:
    """V1 (+V1b): frontmatter parse, field rules, evergreen index table."""
    categories: dict[str, dict] = config.get("categories", {})
    for rel_path, category in cat_docs:
        text = _read_text(root, rel_path)
        if text is None:
            report.add("frontmatter", "error", rel_path, "file could not be read")
            continue

        fm, parse_error = frontmatter_spec.parse_frontmatter(text)
        if fm is None:
            report.add("frontmatter", "error", rel_path, parse_error)
        else:
            for severity, message in frontmatter_spec.check_fields(fm, config, category):
                report.add("frontmatter", severity, rel_path, message)

        # V1b: evergreen docs need an index table under the H1.
        if (
            config.get("validate", {}).get("require_index_table")
            and category is not None
            and categories.get(category, {}).get("lifecycle") == "evergreen"
            and not frontmatter_spec.has_index_table(text)
        ):
            report.add("frontmatter", "warning", rel_path, "missing index table under H1")


def _resolve_link_target(root: Path, rel_path: str, target: str) -> Optional[Path]:
    """Resolve a relative link target against the doc's own directory first,
    then the project root. None when neither exists."""
    for base in ((root / rel_path).parent, root):
        candidate = base / target
        if candidate.exists():
            return candidate
    return None


def _check_links(
    report: Report,
    root: Path,
    cat_docs: "list[tuple[str, Optional[str]]]",
    gate_files: "list[str]",
) -> None:
    """V2: relative markdown link targets exist; fragments match headings."""
    for rel_path in [rel for rel, _ in cat_docs] + gate_files:
        text = _read_text(root, rel_path)
        if text is None:
            continue  # unreadable category docs are reported by V1
        in_fence = False
        for line_no, line in enumerate(text.split("\n"), start=1):
            if line.lstrip().startswith("```"):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            scan_line = _INLINE_CODE_RE.sub(" ", line)
            for target in _MD_LINK_RE.findall(scan_line):
                if target.startswith(("http://", "https://", "mailto:", "#")):
                    continue
                path_part, _, fragment = target.partition("#")
                if not path_part:
                    continue
                resolved = _resolve_link_target(root, rel_path, path_part)
                if resolved is None:
                    report.add(
                        "links", "error", rel_path,
                        f"link target '{target}' does not exist",
                        line=line_no,
                    )
                    continue
                if fragment and resolved.is_file():
                    try:
                        target_text = resolved.read_text(encoding="utf-8")
                    except OSError:
                        continue
                    slugs = {_slugify(heading) for _, heading, _ in _headings(target_text)}
                    if fragment not in slugs:
                        report.add(
                            "links", "warning", rel_path,
                            f"link fragment '#{fragment}' not found in '{path_part}'",
                            line=line_no,
                        )


def _check_drift(
    report: Report,
    root: Path,
    config: dict,
    cat_docs: "list[tuple[str, Optional[str]]]",
    gate_files: "list[str]",
) -> None:
    """V3: cited code refs resolve. Drift feeds the freshness score, so both
    severities default to "warning" (a broken citation is a staleness signal,
    not a build-breaker); a project may raise either to "error" via
    validate.path_drift_severity / validate.symbol_drift_severity. missing_path
    only ever applies to evergreen category docs; extra-gate files are always
    warning-level."""
    categories: dict[str, dict] = config.get("categories", {})
    symbol_severity = (
        "error"
        if config.get("validate", {}).get("symbol_drift_severity") == "error"
        else "warning"
    )
    path_severity = (
        "error"
        if config.get("validate", {}).get("path_drift_severity") == "error"
        else "warning"
    )

    targets: list[tuple[str, bool]] = []  # (rel_path, is_evergreen_category_doc)
    for rel_path, category in cat_docs:
        lifecycle = categories.get(category, {}).get("lifecycle") if category else None
        targets.append((rel_path, lifecycle == "evergreen"))
    targets.extend((rel_path, False) for rel_path in gate_files)

    for rel_path, is_evergreen in targets:
        text = _read_text(root, rel_path)
        if text is None:
            continue
        for ref in refs_lib.extract(text):
            state = refs_lib.resolve(ref, root, rel_path)
            if state == "ok":
                continue
            if state == "missing_path":
                severity = path_severity if is_evergreen else "warning"
                message = f"cited path '{ref.path}' does not exist"
            else:  # missing_symbol
                severity = symbol_severity
                message = f"symbol '{ref.symbol}' not found in '{ref.path}'"
            report.add("drift", severity, rel_path, message, line=ref.line_no)


def _check_docmap(report: Report, root: Path) -> None:
    """V4: docmap loads, keys point at existing files/dirs, entry paths
    exist, no entry is missing a path, no duplicate {key, path} pairs."""
    docmap_rel = docmap_lib.DOCMAP_REL_PATH.as_posix()
    try:
        data = docmap_lib.load_docmap(root)
    except FileNotFoundError:
        report.add("docmap", "error", docmap_rel, "docmap.json missing")
        return
    except (ValueError, json.JSONDecodeError) as e:
        report.add("docmap", "error", docmap_rel, f"docmap.json malformed: {e}")
        return

    mapping: dict = data["map"]
    for key in sorted(mapping):
        if key.endswith("/"):
            if not (root / key).is_dir():
                report.add(
                    "docmap", "error", docmap_rel,
                    f"key '{key}' does not match an existing directory",
                )
        elif not (root / key).is_file():
            report.add(
                "docmap", "error", docmap_rel,
                f"key '{key}' does not match an existing file",
            )

        raw_value = mapping[key]
        if isinstance(raw_value, list):
            for entry in raw_value:
                if isinstance(entry, dict) and not entry.get("path"):
                    report.add(
                        "docmap", "error", docmap_rel,
                        f"key '{key}' has an entry missing 'path'",
                    )

        seen_paths: set = set()
        for entry in docmap_lib.normalize_entries(raw_value):
            doc_path = entry["path"]
            if not (root / doc_path).is_file():
                report.add(
                    "docmap", "error", docmap_rel,
                    f"key '{key}' maps to nonexistent doc '{doc_path}'",
                )
            if doc_path in seen_paths:
                report.add(
                    "docmap", "warning", docmap_rel,
                    f"key '{key}' lists doc '{doc_path}' more than once",
                )
            seen_paths.add(doc_path)


def _check_coverage(report: Report, root: Path, config: dict) -> None:
    """V5: every extra_gate_paths glob must match at least one file."""
    for pattern in config.get("extra_gate_paths", []):
        if not _glob_pattern_files(root, pattern):
            report.add("coverage", "warning", pattern, "pattern matches no files")


def _check_plan_ttl(
    report: Report,
    root: Path,
    config: dict,
    cat_docs: "list[tuple[str, Optional[str]]]",
) -> None:
    """V6: ephemeral docs (status draft/stable, not archived) whose last
    commit is older than the category ttl_days get an expiry warning."""
    categories: dict[str, dict] = config.get("categories", {})
    now = time.time()
    for rel_path, category in cat_docs:
        if category is None:
            continue
        category_config = categories.get(category, {})
        if category_config.get("lifecycle") != "ephemeral":
            continue
        ttl_days = category_config.get("ttl_days")
        if not ttl_days:
            continue
        archive_dir = str(category_config.get("archive_dir") or "").strip("/")
        if archive_dir and rel_path.startswith(archive_dir + "/"):
            continue

        text = _read_text(root, rel_path)
        if text is None:
            continue
        fm, _ = frontmatter_spec.parse_frontmatter(text)
        status = fm.get("status") if fm else None
        if status not in ("draft", "stable"):
            continue

        commit_ts = gitinfo.last_commit_ts(root, rel_path)
        if commit_ts is None:
            continue  # untracked: no reliable age
        age_days = (now - commit_ts) / _SECONDS_PER_DAY
        if age_days > ttl_days:
            report.add(
                "plan-ttl", "warning", rel_path,
                f"plan expired ({int(age_days)}d > ttl {ttl_days}d); "
                f"move to {archive_dir} or mark superseded",
            )


def _check_decisions(
    report: Report,
    root: Path,
    config: dict,
    cat_docs: "list[tuple[str, Optional[str]]]",
) -> None:
    """V7: docs in numbered categories follow the filename pattern, keep
    numbers unique, and contain the required H2 sections."""
    categories: dict[str, dict] = config.get("categories", {})
    numbers_seen: dict[str, dict[str, str]] = {}  # category -> number -> first rel_path

    for rel_path, category in cat_docs:
        if category is None:
            continue
        numbering = categories.get(category, {}).get("numbering")
        if not isinstance(numbering, dict):
            continue
        prefix = numbering.get("prefix", "")
        digits = int(numbering.get("digits", 3))

        filename = rel_path.rsplit("/", 1)[-1]
        name_re = re.compile(rf"^{re.escape(prefix)}(\d{{{digits}}})-[a-z0-9][a-z0-9-]*\.md$")
        match = name_re.match(filename)
        if match is None:
            report.add(
                "decisions", "error", rel_path,
                f"filename '{filename}' does not match "
                f"'{prefix}{'N' * digits}-kebab-case.md'",
            )
        else:
            number = match.group(1)
            first = numbers_seen.setdefault(category, {}).setdefault(number, rel_path)
            if first != rel_path:
                report.add(
                    "decisions", "error", rel_path,
                    f"duplicate decision number '{number}' (also used by '{first}')",
                )

        text = _read_text(root, rel_path)
        if text is None:
            continue
        fm, _ = frontmatter_spec.parse_frontmatter(text)
        status = fm.get("status") if fm else None
        # New (draft) decisions must be complete; pre-existing ones are
        # grandfathered down to warnings.
        severity = "error" if status == "draft" else "warning"

        headings = _headings(text)
        h2_texts = [heading for level, heading, _ in headings if level == 2]
        for section in _DECISION_SECTIONS:
            if section not in h2_texts:
                report.add(
                    "decisions", severity, rel_path,
                    f"missing required section '## {section}'",
                )
        if "Options Considered" in h2_texts:
            option_count = 0
            inside_options = False
            for level, heading, _ in headings:
                if level == 2:
                    inside_options = heading == "Options Considered"
                elif level == 3 and inside_options:
                    option_count += 1
            if option_count < 2:
                report.add(
                    "decisions", severity, rel_path,
                    "'## Options Considered' must contain at least 2 '###' "
                    "option subsections",
                )


# --- entry point -------------------------------------------------------------


def _enabled_checks(args, config: dict) -> Optional[set]:
    """Tokens to run: config-enabled checks, intersected with --only.

    Returns None (caller exits 2) when --only names an unknown token.
    """
    validate_config = config.get("validate", {})
    enabled = {
        token
        for token, config_key in CHECK_TOKENS.items()
        if validate_config.get(config_key, True)
    }
    if not args.only:
        return enabled

    requested = {token.strip() for token in args.only.split(",") if token.strip()}
    unknown = requested - set(CHECK_TOKENS)
    if unknown:
        print(
            f"docsmith: unknown --only check(s): {', '.join(sorted(unknown))} "
            f"(valid: {', '.join(CHECK_TOKENS)})",
            file=sys.stderr,
        )
        return None
    return enabled & requested


def run(args, project_root: Path, config: dict) -> int:
    """Execute `docsmith validate`. Returns the process exit code."""
    root = Path(project_root)
    enabled = _enabled_checks(args, config)
    if enabled is None:
        return 2

    cat_docs = category_docs(root, config)
    gate_files = extra_gate_files(root, config)
    if args.paths:
        cat_docs = [(rel, cat) for rel, cat in cat_docs if _matches_any(rel, args.paths)]
        gate_files = [rel for rel in gate_files if _matches_any(rel, args.paths)]

    report = Report(command="validate", project_root=root)
    report.checked = len(cat_docs) + len(gate_files)

    if "frontmatter" in enabled:
        _check_frontmatter(report, root, config, cat_docs)
    if "links" in enabled:
        _check_links(report, root, cat_docs, gate_files)
    if "drift" in enabled:
        _check_drift(report, root, config, cat_docs, gate_files)
    if "docmap" in enabled:
        _check_docmap(report, root)
    if "coverage" in enabled:
        _check_coverage(report, root, config)
    if "plan-ttl" in enabled:
        _check_plan_ttl(report, root, config, cat_docs)
    if "decisions" in enabled:
        _check_decisions(report, root, config, cat_docs)

    if args.json:
        print(report.to_json())
    elif not args.quiet:
        print(report.to_human())
    return report.exit_code(strict=args.strict)
