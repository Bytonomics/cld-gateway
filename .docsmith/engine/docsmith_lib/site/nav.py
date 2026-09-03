"""Navigation building and hub-page generation for the docsmith site pipeline.

Two navigation styles, selected by config `site.nav.style`:

- "subsystem-first" (default): entries are grouped into one section per
  subsystem (derived from each entry's source path), with a generated hub
  index page prepended to every section. Decision and plan entries are
  pulled out into trailing global "Decisions" / "Plans" sections (each with
  a generated index table page), and category-less entries land in a
  trailing "Uncategorized" section so nothing is ever silently dropped.
- "type-first": the legacy shape ported from smritea-cloud's docs_kb.py --
  one section per OKF type from `okf_compat.allowed_types`, sub-grouped by
  top-level repo directory when a type spans several of them. No hubs.

Entries passed into this module must already be annotated with a
`category` key (see engine.annotate_and_filter_entries) and their collected
files must already exist under `ctx.autogen_docs_dir` (hub tables read each
leaf's frontmatter `status` from the collected file on disk).
"""

from __future__ import annotations

import datetime
import posixpath
import re
from pathlib import Path
from typing import TYPE_CHECKING, Optional

from .. import frontmatter_spec

if TYPE_CHECKING:
    from .engine import SiteContext

# --- Legacy type-first constants (ported verbatim from docs_kb.py) ----------

# Types kept as a single flat, path-sorted list even when their entries span
# several top-level repo directories.
_TYPES_WITHOUT_SUBGROUPING = frozenset({"ADR", "Plan"})

# A type section is only worth sub-grouping once it spans more than one
# top-level repo directory.
_SUBGROUP_MIN_DISTINCT_DIRS = 2

# --- Subsystem-first constants ----------------------------------------------

# Fixed category order for within-section subgrouping. Categories not listed
# here (custom ones) sort alphabetically after these; None entries last.
_CATEGORY_SUBGROUP_ORDER = (
    "tutorial",
    "how-to",
    "reference",
    "explanation",
    "runbook",
    "external",
)

_CATEGORY_DISPLAY_TITLES = {
    "tutorial": "Tutorial",
    "how-to": "How-To",
    "reference": "Reference",
    "explanation": "Explanation",
    "runbook": "Runbook",
    "external": "External",
}

_UNCATEGORIZED = "Uncategorized"

_HUB_NAV_TITLE = "Overview"
_INDEX_NAV_TITLE = "Index"

_DECISIONS_INDEX_FILENAME = "decisions-index.md"
_PLANS_INDEX_FILENAME = "plans-index.md"


# --- Shared helpers ----------------------------------------------------------


def rel_source_path(entry: dict, ctx: "SiteContext") -> str:
    """Posix-style source path of `entry` relative to the project root.

    Falls back to the bare filename when the source path is (unexpectedly)
    outside the project root, so grouping never raises.
    """
    source_path = Path(entry["source_path"])
    root = Path(ctx.project_root)
    # Try the walked (unresolved) path first: when the collector descended a
    # symlinked docs_dir, the walked path is still <project_root>/docs/...,
    # whereas .resolve() would rewrite it to the symlink target (outside the
    # project root) and lose the subsystem grouping. Fall back to the resolved
    # path, then the bare filename, so grouping never raises.
    for candidate in (source_path, source_path.resolve()):
        try:
            return candidate.relative_to(root).as_posix()
        except ValueError:
            continue
    return source_path.name


def _humanize(segment: str) -> str:
    """Humanize a path segment, e.g. 'cloud_frontend' -> 'Cloud Frontend'.

    Port of docs_kb.py's _top_level_dir_title humanization.
    """
    return segment.replace("-", " ").replace("_", " ").title()


def _slugify(title: str) -> str:
    """Slug for hub directory names: lowercase, non-alphanumeric runs -> '-'."""
    slug = re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")
    return slug or "section"


def _disambiguate_titles(entries: list[dict]) -> list[dict]:
    """Disambiguate duplicate titles by appending the parent dir name.

    Port of docs_kb.py's _disambiguate_titles: only titles that actually
    collide within `entries` are touched. Returns {'title', 'path'} leaf
    dicts sorted by output_path for deterministic ordering.
    """
    sorted_entries = sorted(entries, key=lambda entry: entry["output_path"])

    title_counts: dict[str, int] = {}
    for entry in sorted_entries:
        title_counts[entry["title"]] = title_counts.get(entry["title"], 0) + 1

    nav_items: list[dict] = []
    for entry in sorted_entries:
        title = entry["title"]
        if title_counts[title] > 1:
            parent_name = Path(entry["output_path"]).parent.name
            if parent_name and parent_name != ".":
                title = f"{title} ({parent_name})"
        nav_items.append({"title": title, "path": entry["output_path"]})
    return nav_items


def _entry_status(entry: dict, ctx: "SiteContext") -> Optional[str]:
    """Read the collected file's frontmatter `status`, or None."""
    output_file = ctx.autogen_docs_dir / entry["output_path"]
    try:
        text = output_file.read_text(encoding="utf-8")
    except OSError:
        return None
    frontmatter, _error = frontmatter_spec.parse_frontmatter(text)
    if not frontmatter:
        return None
    status = frontmatter.get("status")
    return status if isinstance(status, str) else None


def _nav_config(ctx: "SiteContext") -> dict:
    return (ctx.config.get("site", {}) or {}).get("nav", {}) or {}


def _nav_style(ctx: "SiteContext") -> str:
    return _nav_config(ctx).get("style", "subsystem-first")


# --- Subsystem-first grouping -----------------------------------------------


def _subsystem_title(entry: dict, ctx: "SiteContext") -> str:
    """Derive the subsystem section title for an entry from its source path.

    - Under `project.docs_dir`/X/...: humanize the first `subsystem_depth`
      directory segments after the docs dir (joined with ' / ' when > 1).
    - Directly in the docs_dir root (or at the repo root): "General".
    - Anywhere else (READMEs, code docs): humanized top-level repo dir.
    """
    project_config = ctx.config.get("project", {}) or {}
    docs_dir_parts = Path(project_config.get("docs_dir", "docs")).parts
    subsystem_depth = project_config.get("subsystem_depth", 1)

    parts = Path(rel_source_path(entry, ctx)).parts
    if docs_dir_parts and parts[: len(docs_dir_parts)] == docs_dir_parts:
        dir_segments = parts[len(docs_dir_parts) : -1]
        if not dir_segments:
            return "General"
        chosen = dir_segments[:subsystem_depth]
        return " / ".join(_humanize(segment) for segment in chosen)

    if len(parts) <= 1:
        return "General"
    return _humanize(parts[0])


def _partition_for_subsystem_nav(
    entries: list[dict], ctx: "SiteContext"
) -> tuple[dict[str, list[dict]], list[dict], list[dict], list[dict]]:
    """Partition entries into (subsystem groups, decisions, active plans,
    uncategorized).

    Plans under the plan category's `archive_dir`, or with frontmatter
    `status: superseded`, are excluded from navigation entirely.
    """
    plan_config = ctx.config.get("categories", {}).get("plan", {}) or {}
    archive_dir = (plan_config.get("archive_dir") or "").rstrip("/")

    subsystems: dict[str, list[dict]] = {}
    decisions: list[dict] = []
    plans_active: list[dict] = []
    uncategorized: list[dict] = []

    for entry in entries:
        category = entry.get("category")
        if category == "decision":
            decisions.append(entry)
            continue
        if category == "plan":
            rel = rel_source_path(entry, ctx)
            if archive_dir and (rel == archive_dir or rel.startswith(archive_dir + "/")):
                continue
            if _entry_status(entry, ctx) == "superseded":
                continue
            plans_active.append(entry)
            continue
        if category is None:
            uncategorized.append(entry)
            continue
        subsystems.setdefault(_subsystem_title(entry, ctx), []).append(entry)

    return subsystems, decisions, plans_active, uncategorized


def _ordered_subsystem_titles(subsystems: dict[str, list[dict]], ctx: "SiteContext") -> list[str]:
    """`site.nav.section_order` pinned first (exact title match), remaining
    titles alphabetical."""
    section_order = _nav_config(ctx).get("section_order", []) or []
    pinned = [title for title in section_order if title in subsystems]
    remaining = sorted(title for title in subsystems if title not in pinned)
    return pinned + remaining


def _category_subgroup_titles(categories: list[Optional[str]]) -> list[Optional[str]]:
    """Order category keys for within-section subgrouping: fixed built-in
    order, then custom categories alphabetically, then None (Uncategorized)."""
    ordered: list[Optional[str]] = [c for c in _CATEGORY_SUBGROUP_ORDER if c in categories]
    ordered.extend(
        sorted(c for c in categories if c is not None and c not in _CATEGORY_SUBGROUP_ORDER)
    )
    if None in categories:
        ordered.append(None)
    return ordered


def _category_display_title(category: Optional[str]) -> str:
    if category is None:
        return _UNCATEGORIZED
    return _CATEGORY_DISPLAY_TITLES.get(category, _humanize(category))


def _section_items(section_entries: list[dict], ctx: "SiteContext") -> list[dict]:
    """Leaf items for one subsystem section: flat when small, otherwise
    subgrouped by category in fixed order."""
    subgroup_min = _nav_config(ctx).get("subgroup_min_entries", 8)
    if len(section_entries) <= subgroup_min:
        return _disambiguate_titles(section_entries)

    by_category: dict[Optional[str], list[dict]] = {}
    for entry in section_entries:
        by_category.setdefault(entry.get("category"), []).append(entry)

    return [
        {
            "title": _category_display_title(category),
            "children": _disambiguate_titles(by_category[category]),
        }
        for category in _category_subgroup_titles(list(by_category))
    ]


# --- Public API --------------------------------------------------------------


def build_navigation_sections(entries: list[dict], ctx: "SiteContext") -> list[dict]:
    """Group annotated entries into navigation sections.

    Returns a list of {'title': str, 'nav_items': [...]} dicts where each
    nav item is either a leaf {'title', 'path'} or a nested group
    {'title', 'children': [...]} -- the exact structure consumed by the
    mkdocs.yml.j2 nav macro.
    """
    if _nav_style(ctx) == "type-first":
        return _build_type_first_sections(entries, ctx)

    subsystems, decisions, plans_active, uncategorized = _partition_for_subsystem_nav(entries, ctx)

    sections: list[dict] = []
    for title in _ordered_subsystem_titles(subsystems, ctx):
        hub_item = {"title": _HUB_NAV_TITLE, "path": f"_hubs/{_slugify(title)}/index.md"}
        sections.append(
            {"title": title, "nav_items": [hub_item, *_section_items(subsystems[title], ctx)]}
        )

    if decisions:
        index_item = {"title": _INDEX_NAV_TITLE, "path": _DECISIONS_INDEX_FILENAME}
        sections.append(
            {"title": "Decisions", "nav_items": [index_item, *_disambiguate_titles(decisions)]}
        )
    if plans_active:
        index_item = {"title": _INDEX_NAV_TITLE, "path": _PLANS_INDEX_FILENAME}
        sections.append(
            {"title": "Plans", "nav_items": [index_item, *_disambiguate_titles(plans_active)]}
        )
    if uncategorized:
        sections.append({"title": _UNCATEGORIZED, "nav_items": _disambiguate_titles(uncategorized)})

    return sections


def generate_hubs(entries: list[dict], ctx: "SiteContext") -> list[Path]:
    """Write hub index pages (one per subsystem section) plus the decisions
    and plans index pages. Returns the list of written paths.

    No-op (returns []) in legacy type-first mode, which has no hubs.
    """
    if _nav_style(ctx) == "type-first":
        return []

    subsystems, decisions, plans_active, _uncategorized = _partition_for_subsystem_nav(entries, ctx)
    generated_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
    written: list[Path] = []

    for title in _ordered_subsystem_titles(subsystems, ctx):
        hub_rel = f"_hubs/{_slugify(title)}/index.md"
        hub_path = ctx.autogen_docs_dir / hub_rel
        hub_path.parent.mkdir(parents=True, exist_ok=True)
        hub_path.write_text(
            _render_hub_page(title, subsystems[title], ctx, generated_at, hub_rel),
            encoding="utf-8",
        )
        written.append(hub_path)

    if decisions:
        index_path = ctx.autogen_docs_dir / _DECISIONS_INDEX_FILENAME
        index_path.write_text(
            _render_decisions_index(decisions, ctx, generated_at, _DECISIONS_INDEX_FILENAME),
            encoding="utf-8",
        )
        written.append(index_path)

    if plans_active:
        index_path = ctx.autogen_docs_dir / _PLANS_INDEX_FILENAME
        index_path.write_text(
            _render_plans_index(plans_active, ctx, generated_at, _PLANS_INDEX_FILENAME),
            encoding="utf-8",
        )
        written.append(index_path)

    return written


def _rel_link(output_path: str, from_page_rel: str) -> str:
    """Relative markdown link from a generated page to a collected doc, both
    given as autogen-docs-relative posix paths.

    mkdocs rewrites RELATIVE `.md` links to their final (directory) URLs, but
    leaves ABSOLUTE links (`/foo.md`) untouched — and those 404 under
    directory-URL serving. So generated index/hub pages must link relatively.
    """
    from_dir = posixpath.dirname(from_page_rel)
    return posixpath.relpath(output_path, from_dir or ".")


# --- Generated page rendering ------------------------------------------------


def _generated_frontmatter(generated_at: str) -> list[str]:
    return [
        "---",
        "generated:",
        "  by: docsmith",
        f"  at: {generated_at}",
        "---",
        "",
    ]


def _render_hub_page(
    title: str, section_entries: list[dict], ctx: "SiteContext", generated_at: str,
    from_page_rel: str,
) -> str:
    lines = _generated_frontmatter(generated_at)
    lines.append(f"# {title}")
    lines.append("")
    lines.append("| Doc | Category | Status | Updated |")
    lines.append("|-----|----------|--------|---------|")
    for entry in sorted(section_entries, key=lambda e: e["output_path"]):
        category = entry.get("category") or "-"
        status = _entry_status(entry, ctx) or "-"
        link = _rel_link(entry["output_path"], from_page_rel)
        lines.append(f"| [{entry['title']}]({link}) | {category} | {status} | - |")
    lines.append("")
    return "\n".join(lines)


def _decision_number(entry: dict, ctx: "SiteContext") -> str:
    numbering = ctx.config.get("categories", {}).get("decision", {}).get("numbering", {}) or {}
    prefix = numbering.get("prefix", "ADR-")
    pattern = re.compile(re.escape(prefix) + r"\d+")
    for candidate in (Path(entry["source_path"]).stem, entry.get("title") or ""):
        match = pattern.search(candidate)
        if match:
            return match.group(0)
    return "-"


def _render_decisions_index(
    decisions: list[dict], ctx: "SiteContext", generated_at: str, from_page_rel: str
) -> str:
    lines = _generated_frontmatter(generated_at)
    lines.append("# Decisions Index")
    lines.append("")
    lines.append("| Number | Title | Status |")
    lines.append("|--------|-------|--------|")
    for entry in sorted(decisions, key=lambda e: e["output_path"]):
        number = _decision_number(entry, ctx)
        status = _entry_status(entry, ctx) or "-"
        link = _rel_link(entry["output_path"], from_page_rel)
        lines.append(f"| {number} | [{entry['title']}]({link}) | {status} |")
    lines.append("")
    return "\n".join(lines)


def _render_plans_index(
    plans_active: list[dict], ctx: "SiteContext", generated_at: str, from_page_rel: str
) -> str:
    lines = _generated_frontmatter(generated_at)
    lines.append("# Plans Index")
    lines.append("")
    lines.append("| Plan | Status |")
    lines.append("|------|--------|")
    for entry in sorted(plans_active, key=lambda e: e["output_path"]):
        status = _entry_status(entry, ctx) or "-"
        link = _rel_link(entry["output_path"], from_page_rel)
        lines.append(f"| [{entry['title']}]({link}) | {status} |")
    lines.append("")
    return "\n".join(lines)


# --- Legacy type-first builder (ported from docs_kb.py) ----------------------


def _top_level_dir_title(entry: dict, ctx: "SiteContext") -> str:
    """Humanize the first path segment of an entry's project-relative source
    path (port of docs_kb.py's _top_level_dir_title)."""
    parts = Path(rel_source_path(entry, ctx)).parts
    top_segment = parts[0] if parts else ""
    return _humanize(top_segment)


def _build_type_section_items(type_entries: list[dict], ctx: "SiteContext") -> list[dict]:
    """Build nav items for one OKF-type section, sub-grouping by top-level
    repo dir when the type spans more than one (port of docs_kb.py)."""
    entries_by_top_dir: dict[str, list[dict]] = {}
    for entry in type_entries:
        entries_by_top_dir.setdefault(_top_level_dir_title(entry, ctx), []).append(entry)

    if len(entries_by_top_dir) < _SUBGROUP_MIN_DISTINCT_DIRS:
        return _disambiguate_titles(type_entries)

    return [
        {"title": top_dir_title, "children": _disambiguate_titles(dir_entries)}
        for top_dir_title, dir_entries in sorted(entries_by_top_dir.items())
    ]


def _build_type_first_sections(entries: list[dict], ctx: "SiteContext") -> list[dict]:
    """Legacy theme-based sections grouped by OKF `type` (port of
    docs_kb.py's build_navigation_sections). Section order follows
    `okf_compat.allowed_types`; unrecognized types land in a trailing
    'Uncategorized' section rather than being silently dropped."""
    allowed_types: list[str] = (ctx.config.get("okf_compat", {}) or {}).get("allowed_types", [])

    entries_by_type: dict[str, list[dict]] = {}
    for entry in entries:
        okf_type = entry.get("okf_type")
        type_key = okf_type if okf_type in allowed_types else _UNCATEGORIZED
        entries_by_type.setdefault(type_key, []).append(entry)

    ordered_type_keys = [t for t in allowed_types if t in entries_by_type]
    if _UNCATEGORIZED in entries_by_type:
        ordered_type_keys.append(_UNCATEGORIZED)

    navigation_sections: list[dict] = []
    for type_key in ordered_type_keys:
        type_entries = entries_by_type[type_key]
        if type_key in _TYPES_WITHOUT_SUBGROUPING:
            nav_items = _disambiguate_titles(type_entries)
        else:
            nav_items = _build_type_section_items(type_entries, ctx)
        navigation_sections.append({"title": type_key, "nav_items": nav_items})
    return navigation_sections
