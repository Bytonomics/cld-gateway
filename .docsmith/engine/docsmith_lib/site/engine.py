"""Generalized MkDocs doc-site pipeline engine for `docsmith collect`.

Fork of smritea-cloud's docs-site/docs_kb.py, restructured around a
SiteContext and the docsmith config's `site` section:

    collect -> transform -> links -> hubs/nav -> landing -> mkdocs.yml

Stages (dispatched by `run` on args.stage):

    render  - collect + transform + links + nav + landing + mkdocs.yml
              (equivalent of the old `collect-render`)
    build   - render + `mkdocs build --clean`
    serve   - render + `mkdocs serve` (blocking dev server)
    check   - validate the collected/built tree (9 checks, read-only)
    clean   - remove all generated output (autogen dir + mkdocs.yml)
    package - build + tarball the built static site
    publish - package + upload the tarball as a GitHub Release asset
    deploy  - package + release-dir deploy to a custom server over ssh/scp

The generated output tree under `<workdir>/autogen/` and the rendered
`<workdir>/mkdocs.yml` are build artifacts and must never be hand-edited.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import shutil
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

from jinja2 import Environment, FileSystemLoader

from .. import config as config_lib
from . import nav as nav_lib
from .collectors import collect_markdown_tree, collect_openapi, collect_readme_discovery
from .transforms import (
    apply_frontmatter_transform,
    apply_links_transform,
    apply_provenance_transform,
    validate_mermaid_fences,
)

_TEMPLATES_DIR = Path(__file__).resolve().parent / "templates"

_COLLECTOR_DISPATCH: dict[str, Callable[[dict, Path, Path, dict], list[dict]]] = {
    "markdown_tree": collect_markdown_tree,
    "readme_discovery": collect_readme_discovery,
    "openapi": collect_openapi,
}

# Stages that exit 0 with a notice when site.enabled is false; every other
# stage exits 2 (it makes no sense to build/serve/ship a disabled site).
_STAGES_OK_WHEN_DISABLED = ("render", "check", "clean")


@dataclass
class SiteContext:
    """Resolved paths + config for one site pipeline invocation."""

    project_root: Path
    workdir: Path  # config site.workdir resolved against project_root
    autogen_docs_dir: Path  # workdir / "autogen/docs"
    site_dir: Path  # workdir / "autogen/site"
    config: dict  # full docsmith config


def build_site_context(
    project_root: Path,
    config: dict,
    workdir_override: Optional[str] = None,
) -> SiteContext:
    """Build a SiteContext from the effective config (and an optional
    --site-workdir override, resolved against the project root)."""
    root = Path(project_root).resolve()
    site_config = config.get("site", {}) or {}
    workdir_setting = workdir_override or site_config.get("workdir") or ".docsmith/site"
    workdir = Path(workdir_setting)
    if not workdir.is_absolute():
        workdir = root / workdir
    return SiteContext(
        project_root=root,
        workdir=workdir,
        autogen_docs_dir=workdir / "autogen" / "docs",
        site_dir=workdir / "autogen" / "site",
        config=config,
    )


# --- Collection + annotation --------------------------------------------------


def collect_all_sources(ctx: SiteContext) -> list[dict]:
    """Run every configured source's collector and return the flattened
    list of source-map entries."""
    site_config = ctx.config.get("site", {}) or {}
    exclude_config = site_config.get("exclude", {}) or {}
    all_entries: list[dict] = []
    for source in site_config.get("sources", []) or []:
        collector_name = source.get("collector")
        collector_fn = _COLLECTOR_DISPATCH.get(collector_name)
        if collector_fn is None:
            raise ValueError(
                f"Unknown collector '{collector_name}' for source '{source.get('id', '?')}'. "
                f"Known collectors: {sorted(_COLLECTOR_DISPATCH)}"
            )
        print(f"[docsmith-site] collecting source '{source['id']}' via '{collector_name}'...")
        entries = collector_fn(source, ctx.project_root, ctx.autogen_docs_dir, exclude_config)
        print(f"[docsmith-site]   collected {len(entries)} file(s) for '{source['id']}'")
        all_entries.extend(entries)
    return all_entries


def annotate_and_filter_entries(entries: list[dict], ctx: SiteContext) -> list[dict]:
    """Resolve each entry's docsmith category (stored as entry['category'],
    okf_type is kept too) and drop entries whose category has
    site.include=false -- removed from the collection output entirely,
    including the already-copied file on disk."""
    kept: list[dict] = []
    for entry in entries:
        rel = nav_lib.rel_source_path(entry, ctx)
        category = config_lib.resolve_category(rel, entry.get("okf_type"), ctx.config)
        entry["category"] = category
        if category is not None:
            category_config = ctx.config.get("categories", {}).get(category, {}) or {}
            if category_config.get("site", {}).get("include", True) is False:
                output_file = ctx.autogen_docs_dir / entry["output_path"]
                if output_file.exists():
                    output_file.unlink()
                print(
                    f"[docsmith-site] excluding '{rel}' "
                    f"(category '{category}' has site.include=false)"
                )
                continue
        kept.append(entry)
    return kept


def transform_all_entries(entries: list[dict], ctx: SiteContext) -> None:
    """Apply the frontmatter and provenance transforms to every collected
    file, in order."""
    print(f"[docsmith-site] transforming {len(entries)} collected file(s)...")
    for entry in entries:
        file_path = ctx.autogen_docs_dir / entry["output_path"]
        apply_frontmatter_transform(file_path, entry)
        apply_provenance_transform(file_path, entry)


def _apply_links_to_all_entries(entries: list[dict], ctx: SiteContext) -> list[str]:
    """Run the links transform over every collected entry, aggregating
    warnings. Also used by stage_check (check #8) in dry-run mode."""
    source_map = {entry["source_path"]: entry for entry in entries}
    all_warnings: list[str] = []
    for entry in entries:
        file_path = ctx.autogen_docs_dir / entry["output_path"]
        all_warnings.extend(
            apply_links_transform(
                file_path,
                source_map,
                ctx.autogen_docs_dir,
                current_source_path=entry["source_path"],
            )
        )
    return all_warnings


# --- Artifacts: source map, manifest, landing page ---------------------------


def write_source_map(ctx: SiteContext, entries: list[dict]) -> Path:
    """Write `source-map.json`: the full flat list of source-map entries."""
    source_map_path = ctx.autogen_docs_dir / "source-map.json"
    source_map_path.write_text(json.dumps(entries, indent=2), encoding="utf-8")
    print(f"[docsmith-site] wrote source map: {source_map_path}")
    return source_map_path


def _source_title(source: dict) -> str:
    return source.get("title") or source.get("id", "?")


def write_manifest(ctx: SiteContext, entries: list[dict], build_timestamp: str) -> Path:
    """Write `manifest.json`: build metadata + per-source file counts."""
    entries_by_source_id: dict[str, list[dict]] = {}
    for entry in entries:
        entries_by_source_id.setdefault(entry["source_id"], []).append(entry)

    sources_summary = [
        {
            "id": source["id"],
            "title": _source_title(source),
            "collector": source.get("collector"),
            "file_count": len(entries_by_source_id.get(source["id"], [])),
        }
        for source in (ctx.config.get("site", {}) or {}).get("sources", []) or []
    ]

    manifest = {
        "generated_at": build_timestamp,
        "git_commit": _resolve_git_commit_sha(ctx.project_root),
        "sources": sources_summary,
    }

    manifest_path = ctx.autogen_docs_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(f"[docsmith-site] wrote manifest: {manifest_path}")
    return manifest_path


def _resolve_git_commit_sha(project_root: Path) -> str:
    """Best-effort resolution of the current git commit SHA via plain file
    reads. Never shells out to git and never raises."""
    try:
        git_dir = project_root / ".git"
        head_content = (git_dir / "HEAD").read_text(encoding="utf-8").strip()
        if head_content.startswith("ref:"):
            ref_relative = head_content.split(" ", 1)[1].strip()
            return (git_dir / ref_relative).read_text(encoding="utf-8").strip()
        return head_content
    except OSError:
        return "unknown"


def _first_leaf_path(nav_items: list[dict]) -> Optional[str]:
    """Recurse into nested nav groups to find the first leaf page's path."""
    for item in nav_items:
        if "path" in item:
            return item["path"]
        found = _first_leaf_path(item["children"])
        if found is not None:
            return found
    return None


def _landing_title(config: dict) -> str:
    site_config = config.get("site", {}) or {}
    return (
        site_config.get("landing_title")
        or site_config.get("name")
        or (config.get("project", {}) or {}).get("name")
        or "Documentation"
    )


def generate_landing_page(
    ctx: SiteContext,
    entries: list[dict],
    navigation_sections: list[dict],
    build_timestamp: str,
) -> None:
    """Generate the autogen landing page at `autogen_docs_dir / 'index.md'`."""
    entries_by_source_id: dict[str, list[dict]] = {}
    for entry in entries:
        entries_by_source_id.setdefault(entry["source_id"], []).append(entry)

    lines: list[str] = []
    lines.append(f"# {_landing_title(ctx.config)}")
    lines.append("")
    lines.append(
        "> **This page and everything under `autogen/` is generated. Do not hand-edit -- "
        "the source repository is the source of truth. Regenerate with "
        "`docsmith collect build`.**"
    )
    lines.append("")
    lines.append(f"- Built: `{build_timestamp}`")
    lines.append(f"- Commit: `{_resolve_git_commit_sha(ctx.project_root)}`")
    lines.append("")
    lines.append("## Sources")
    lines.append("")
    lines.append("| Source ID | Title | Files Collected |")
    lines.append("|-----------|-------|------------------|")
    for source in (ctx.config.get("site", {}) or {}).get("sources", []) or []:
        count = len(entries_by_source_id.get(source["id"], []))
        lines.append(f"| `{source['id']}` | {_source_title(source)} | {count} |")
    lines.append("")
    lines.append("## Sections")
    lines.append("")
    for section in navigation_sections:
        first_leaf_path = _first_leaf_path(section["nav_items"])
        if first_leaf_path is None:
            continue
        lines.append(f"- [{section['title']}]({first_leaf_path})")
    lines.append("")

    landing_page_path = ctx.autogen_docs_dir / "index.md"
    landing_page_path.write_text("\n".join(lines), encoding="utf-8")
    print(f"[docsmith-site] wrote landing page: {landing_page_path}")


# --- mkdocs.yml rendering -----------------------------------------------------


def render_mkdocs_config(ctx: SiteContext, navigation_sections: list[dict]) -> Path:
    """Render templates/mkdocs.yml.j2 into `<workdir>/mkdocs.yml`.

    Every identity variable is defaulted here so a sparse config renders.
    The template itself contains no timestamps, so the output is fully
    deterministic for a given config + entries.
    """
    site_config = ctx.config.get("site", {}) or {}
    project_config = ctx.config.get("project", {}) or {}
    theme_config = site_config.get("theme", {}) or {}
    plugins_config = site_config.get("plugins", {}) or {}

    repo_url = site_config.get("repo_url") or project_config.get("repo_url") or ""
    edit_branch = site_config.get("edit_branch") or "main"

    template_context = {
        "navigation_sections": navigation_sections,
        "site_name": site_config.get("name") or project_config.get("name") or "Documentation",
        "site_description": site_config.get("description") or "",
        "site_author": site_config.get("site_author") or "",
        "repo_name": site_config.get("repo_name") or "",
        "repo_url": repo_url,
        "edit_uri": f"edit/{edit_branch}/" if repo_url else "",
        "docs_dir": "./autogen/docs",
        "site_dir": "./autogen/site",
        "theme_primary": theme_config.get("primary") or "blue",
        "theme_accent": theme_config.get("accent") or "blue",
        "font_text": theme_config.get("font_text") or "Roboto",
        "font_code": theme_config.get("font_code") or "Roboto Mono",
        "copyright": site_config.get("copyright") or "",
        "social_links": site_config.get("social_links") or [],
        "enable_mermaid": plugins_config.get("mermaid", True),
        "enable_swagger_ui": plugins_config.get("swagger_ui", False),
        "enable_git_revision_date": plugins_config.get("git_revision_date", True),
        "enable_minify": plugins_config.get("minify", True),
    }

    environment = Environment(loader=FileSystemLoader(str(_TEMPLATES_DIR)))
    # Same yaml_str filter contract as the original docs_kb.py pipeline.
    environment.filters["yaml_str"] = lambda value: json.dumps(str(value))
    template = environment.get_template("mkdocs.yml.j2")
    rendered = template.render(**template_context)

    ctx.workdir.mkdir(parents=True, exist_ok=True)
    mkdocs_yml_path = ctx.workdir / "mkdocs.yml"
    mkdocs_yml_path.write_text(rendered, encoding="utf-8")
    print(f"[docsmith-site] rendered mkdocs config: {mkdocs_yml_path}")
    return mkdocs_yml_path


# --- Pipeline -----------------------------------------------------------------


def run_render_pipeline(ctx: SiteContext) -> tuple[list[dict], list[dict], list[str]]:
    """Run collect -> transform -> links -> hubs/nav -> landing -> mkdocs.yml.

    Returns (entries, navigation_sections, link_warnings).
    """
    if ctx.autogen_docs_dir.exists():
        shutil.rmtree(ctx.autogen_docs_dir, ignore_errors=False)
        print(f"[docsmith-site] cleaned stale output: {ctx.autogen_docs_dir}")
    ctx.autogen_docs_dir.mkdir(parents=True, exist_ok=True)

    print("[docsmith-site] === collecting ===")
    entries = collect_all_sources(ctx)
    entries = annotate_and_filter_entries(entries, ctx)

    print("[docsmith-site] === transforming ===")
    transform_all_entries(entries, ctx)

    print("[docsmith-site] === writing source map ===")
    write_source_map(ctx, entries)

    build_timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()

    print("[docsmith-site] === writing manifest ===")
    write_manifest(ctx, entries, build_timestamp)

    print("[docsmith-site] === rewriting internal links ===")
    link_warnings = _apply_links_to_all_entries(entries, ctx)
    if link_warnings:
        print(
            f"[docsmith-site] {len(link_warnings)} unresolved relative link(s) "
            "-- see the check stage for details"
        )

    print("[docsmith-site] === generating hub pages ===")
    hub_paths = nav_lib.generate_hubs(entries, ctx)
    if hub_paths:
        print(f"[docsmith-site] wrote {len(hub_paths)} generated index page(s)")

    print("[docsmith-site] === building navigation ===")
    navigation_sections = nav_lib.build_navigation_sections(entries, ctx)

    print("[docsmith-site] === generating landing page ===")
    generate_landing_page(ctx, entries, navigation_sections, build_timestamp)

    print("[docsmith-site] === rendering mkdocs.yml ===")
    render_mkdocs_config(ctx, navigation_sections)

    return entries, navigation_sections, link_warnings


def _resolve_mkdocs_binary(ctx: SiteContext) -> Optional[str]:
    """Resolve the `mkdocs` executable.

    Checks (in order): a sibling of the running Python interpreter (the
    original docs_kb.py venv resolution), a venv inside the site workdir
    (`<workdir>/.venv/bin/mkdocs`), and finally `mkdocs` on PATH. Returns
    None when no candidate exists.
    """
    sibling = Path(sys.executable).parent / "mkdocs"
    if sibling.exists():
        return str(sibling)
    workdir_venv = ctx.workdir / ".venv" / "bin" / "mkdocs"
    if workdir_venv.exists():
        return str(workdir_venv)
    return shutil.which("mkdocs")


# --- Stages -------------------------------------------------------------------


def stage_render(ctx: SiteContext) -> int:
    """Collect + transform + render mkdocs.yml (no mkdocs build)."""
    run_render_pipeline(ctx)
    print("[docsmith-site] render complete (mkdocs build skipped).")
    return 0


def _require_mkdocs(ctx: SiteContext, stage: str) -> Optional[str]:
    mkdocs_binary = _resolve_mkdocs_binary(ctx)
    if mkdocs_binary is None:
        print(
            f"[docsmith-site] error: mkdocs binary not found for 'collect {stage}' "
            "(checked the interpreter's bin dir, <workdir>/.venv/bin/mkdocs, and PATH) "
            "-- install mkdocs (e.g. 'pip install mkdocs-material') and retry",
            file=sys.stderr,
        )
    return mkdocs_binary


def stage_build(ctx: SiteContext) -> int:
    """Run the full pipeline and produce a static site via
    `mkdocs build --clean`."""
    mkdocs_binary = _require_mkdocs(ctx, "build")
    if mkdocs_binary is None:
        return 2

    run_render_pipeline(ctx)

    print("[docsmith-site] === running mkdocs build --clean ===")
    try:
        subprocess.run([mkdocs_binary, "build", "--clean"], cwd=ctx.workdir, check=True)
    except subprocess.CalledProcessError as exc:
        print(f"[docsmith-site] error: mkdocs build failed: {exc}", file=sys.stderr)
        return 1
    print("[docsmith-site] build complete.")
    return 0


def stage_serve(ctx: SiteContext) -> int:
    """Run the full pipeline and start a live-reloading `mkdocs serve`
    dev server (blocking)."""
    mkdocs_binary = _require_mkdocs(ctx, "serve")
    if mkdocs_binary is None:
        return 2

    run_render_pipeline(ctx)

    print("[docsmith-site] === starting mkdocs dev server (blocking) ===")
    try:
        subprocess.run([mkdocs_binary, "serve"], cwd=ctx.workdir, check=True)
    except subprocess.CalledProcessError as exc:
        print(f"[docsmith-site] error: mkdocs serve failed: {exc}", file=sys.stderr)
        return 1
    return 0


def stage_clean(ctx: SiteContext) -> int:
    """Remove all generated output: the autogen dir (docs + site + tarball)
    and the rendered mkdocs.yml (never the .j2 template)."""
    autogen_dir = ctx.workdir / "autogen"
    mkdocs_yml_path = ctx.workdir / "mkdocs.yml"

    if autogen_dir.exists():
        shutil.rmtree(autogen_dir, ignore_errors=False)
        print(f"[docsmith-site] removed: {autogen_dir}")
    else:
        print(f"[docsmith-site] nothing to remove (already absent): {autogen_dir}")

    if mkdocs_yml_path.exists():
        mkdocs_yml_path.unlink()
        print(f"[docsmith-site] removed: {mkdocs_yml_path}")
    else:
        print(f"[docsmith-site] nothing to remove (already absent): {mkdocs_yml_path}")

    print("[docsmith-site] clean complete.")
    return 0


def stage_check(ctx: SiteContext) -> int:
    """Validate the collected/built docs tree (9 checks, read-only).

    Assumes `render` (or `build`) already ran: check never re-collects, it
    only inspects what exists on disk. Prints one [PASS]/[FAIL]/[WARN] line
    per check; returns 0 when there are zero failures (warnings do not
    affect the exit code), 1 otherwise.
    """
    site_config = ctx.config.get("site", {}) or {}
    configured_sources = site_config.get("sources", []) or []

    failures: list[str] = []
    warnings: list[str] = []

    # 1. autogen landing page exists.
    autogen_index_path = ctx.autogen_docs_dir / "index.md"
    if autogen_index_path.exists():
        print(f"[PASS] autogen landing page exists: {autogen_index_path}")
    else:
        failures.append(f"autogen landing page missing: {autogen_index_path}")
        print(
            f"[FAIL] autogen landing page missing: {autogen_index_path} "
            "(did you run 'docsmith collect render' first?)"
        )

    # 2. built site home page exists (only enforced once a build exists --
    # a freshly rendered tree without `mkdocs build` must still pass check).
    site_index_path = ctx.site_dir / "index.html"
    if not ctx.site_dir.exists():
        warnings.append(f"built site not present: {ctx.site_dir}")
        print(
            f"[WARN] built site not present (run 'docsmith collect build'): {ctx.site_dir} "
            "-- skipping home-page check"
        )
    elif site_index_path.exists():
        print(f"[PASS] built site home page exists: {site_index_path}")
    else:
        failures.append(f"built site home page missing: {site_index_path}")
        print(f"[FAIL] built site home page missing: {site_index_path}")

    # 3. manifest.json exists and covers every configured source id.
    manifest_path = ctx.autogen_docs_dir / "manifest.json"
    manifest_data: Optional[dict] = None
    if not manifest_path.exists():
        failures.append(f"manifest.json missing: {manifest_path}")
        print(f"[FAIL] manifest.json missing: {manifest_path}")
    else:
        try:
            manifest_data = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            failures.append(f"manifest.json unreadable/malformed: {exc}")
            print(f"[FAIL] manifest.json unreadable/malformed: {exc}")

    if manifest_data is not None:
        configured_ids = {source["id"] for source in configured_sources}
        manifest_ids = {source["id"] for source in manifest_data.get("sources", [])}
        missing_ids = sorted(configured_ids - manifest_ids)
        if missing_ids:
            failures.append(f"manifest.json missing source id(s): {missing_ids}")
            print(f"[FAIL] manifest.json missing source id(s): {missing_ids}")
        else:
            print("[PASS] manifest.json covers every configured source id")

    # 4. source-map.json exists and parses as a JSON list.
    source_map_path = ctx.autogen_docs_dir / "source-map.json"
    source_map_entries: Optional[list[dict]] = None
    if not source_map_path.exists():
        failures.append(f"source-map.json missing: {source_map_path}")
        print(f"[FAIL] source-map.json missing: {source_map_path}")
    else:
        try:
            parsed = json.loads(source_map_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            failures.append(f"source-map.json unreadable/malformed: {exc}")
            print(f"[FAIL] source-map.json unreadable/malformed: {exc}")
        else:
            if isinstance(parsed, list):
                source_map_entries = parsed
                print(f"[PASS] source-map.json exists and parses as a list ({len(parsed)} entries)")
            else:
                failures.append("source-map.json does not parse as a JSON list")
                print("[FAIL] source-map.json does not parse as a JSON list")

    markdown_files: list[Path] = (
        sorted(ctx.autogen_docs_dir.rglob("*.md")) if ctx.autogen_docs_dir.exists() else []
    )

    # 5. mermaid fences are balanced across every collected markdown file.
    mermaid_errors: list[str] = []
    for md_file in markdown_files:
        mermaid_errors.extend(validate_mermaid_fences(md_file))
    if mermaid_errors:
        failures.extend(mermaid_errors)
        print(f"[FAIL] {len(mermaid_errors)} unbalanced mermaid fence(s):")
        for message in mermaid_errors:
            print(f"       {message}")
    else:
        print("[PASS] all mermaid fences are balanced")

    # 6. no collected source_path falls under an excluded directory.
    if source_map_entries is not None:
        excluded_dir_basenames = set(
            (site_config.get("exclude", {}) or {}).get("dir_basenames", [])
        )
        excluded_hits = [
            entry["source_path"]
            for entry in source_map_entries
            if any(part in excluded_dir_basenames for part in Path(entry["source_path"]).parts)
        ]
        if excluded_hits:
            failures.append(f"collected file(s) under excluded directories: {excluded_hits}")
            print(f"[FAIL] collected file(s) under excluded directories: {excluded_hits}")
        else:
            print("[PASS] no collected file falls under an excluded directory")

    # 7. no two entries share the same output_path.
    if source_map_entries is not None:
        output_path_counts: dict[str, int] = {}
        for entry in source_map_entries:
            output_path = entry["output_path"]
            output_path_counts[output_path] = output_path_counts.get(output_path, 0) + 1
        duplicate_output_paths = sorted(
            path for path, count in output_path_counts.items() if count > 1
        )
        if duplicate_output_paths:
            failures.append(f"duplicate output_path value(s): {duplicate_output_paths}")
            print(f"[FAIL] duplicate output_path value(s): {duplicate_output_paths}")
        else:
            print("[PASS] no duplicate output_path values")

    # 8. internal markdown links (warning-only; broken links are tolerated).
    if source_map_entries is not None:
        entries_by_source_path = {entry["source_path"]: entry for entry in source_map_entries}
        source_by_output = {
            str(ctx.autogen_docs_dir / entry["output_path"]): source_path
            for source_path, entry in entries_by_source_path.items()
        }
        link_warnings: list[str] = []
        for md_file in markdown_files:
            link_warnings.extend(
                apply_links_transform(
                    md_file,
                    entries_by_source_path,
                    ctx.autogen_docs_dir,
                    dry_run=True,
                    current_source_path=source_by_output.get(str(md_file)),
                )
            )
        if link_warnings:
            warnings.extend(link_warnings)
            print(f"[WARN] {len(link_warnings)} unresolved relative link(s):")
            for message in link_warnings:
                print(f"       {message}")
        else:
            print("[PASS] no unresolved relative links")

    # 9. no generated markdown file is empty or whitespace-only.
    empty_files: list[str] = []
    for md_file in markdown_files:
        try:
            file_content = md_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            failures.append(f"unable to read {md_file} for empty-file check: {exc}")
            continue
        if not file_content.strip():
            empty_files.append(str(md_file))
    if empty_files:
        failures.append(f"empty/whitespace-only generated file(s): {empty_files}")
        print(f"[FAIL] empty/whitespace-only generated file(s): {empty_files}")
    else:
        print("[PASS] no empty/whitespace-only generated markdown files")

    print("")
    print(f"FAILURES: {len(failures)}")
    print(f"WARNINGS: {len(warnings)}")

    return 0 if not failures else 1


def _tarball_name(config: dict) -> str:
    site_config = config.get("site", {}) or {}
    if site_config.get("tarball_name"):
        return site_config["tarball_name"]
    project_name = (config.get("project", {}) or {}).get("name") or "site"
    return f"{project_name}-docs.tar.gz"


def stage_package(ctx: SiteContext) -> int:
    """Build the site and package the built static HTML as a tarball at
    `<workdir>/autogen/<tarball_name>` (extracting yields index.html at
    the extraction root)."""
    build_result = stage_build(ctx)
    if build_result != 0:
        print(
            f"[docsmith-site] error: build failed (exit code {build_result}), refusing to package",
            file=sys.stderr,
        )
        return build_result

    tarball_path = ctx.workdir / "autogen" / _tarball_name(ctx.config)
    tarball_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"[docsmith-site] === packaging {ctx.site_dir} into {tarball_path} ===")
    with tarfile.open(tarball_path, "w:gz") as tar:
        for child in sorted(ctx.site_dir.iterdir()):
            tar.add(child, arcname=child.name)

    size_mb = tarball_path.stat().st_size / 1024 / 1024
    print(f"[docsmith-site] wrote tarball: {tarball_path} ({size_mb:.1f} MB)")
    return 0


def stage_deploy(ctx: SiteContext) -> int:
    """Package the site and deploy it to a custom server (release-dir model).

    Environment variables (never hardcoded, the target is per-environment):

    - DOCS_DEPLOY_HOST (required) -- e.g. 'user@docs-server.internal'.
    - DOCS_DEPLOY_BASE_PATH (optional) -- defaults to '/srv/<project>-docs'.
    - DOCS_DEPLOY_SSH_KEY (optional) -- ssh private key path ('-i <path>').
    - DOCS_DEPLOY_RELOAD_CMD (optional) -- remote command run after
      activation (e.g. 'sudo systemctl reload nginx'); skipped when unset.
    """
    package_result = stage_package(ctx)
    if package_result != 0:
        print(
            f"[docsmith-site] error: package failed (exit code {package_result}), "
            "refusing to deploy",
            file=sys.stderr,
        )
        return package_result

    host = os.environ.get("DOCS_DEPLOY_HOST")
    if not host:
        print(
            "[docsmith-site] error: DOCS_DEPLOY_HOST is required "
            "(e.g. 'user@docs-server.internal') but was not set -- refusing to deploy to nowhere",
            file=sys.stderr,
        )
        return 1

    tarball_name = _tarball_name(ctx.config)
    default_base_path = f"/srv/{tarball_name.removesuffix('.tar.gz')}"
    base_path = os.environ.get("DOCS_DEPLOY_BASE_PATH", default_base_path)
    ssh_key_path = os.environ.get("DOCS_DEPLOY_SSH_KEY")
    reload_cmd = os.environ.get("DOCS_DEPLOY_RELOAD_CMD")

    ssh_key_flags = ["-i", ssh_key_path] if ssh_key_path else []

    tarball_path = ctx.workdir / "autogen" / tarball_name
    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S")
    remote_tmp_tarball = f"/tmp/{tarball_name.removesuffix('.tar.gz')}-{timestamp}.tar.gz"
    remote_release_dir = f"{base_path}/releases/{timestamp}"

    print(f"[docsmith-site] === uploading {tarball_path} to {host}:{remote_tmp_tarball} ===")
    subprocess.run(
        ["scp", *ssh_key_flags, str(tarball_path), f"{host}:{remote_tmp_tarball}"],
        check=True,
    )

    print(f"[docsmith-site] === extracting release into {remote_release_dir} ===")
    subprocess.run(
        [
            "ssh",
            *ssh_key_flags,
            host,
            f"mkdir -p {remote_release_dir} && tar -xzf {remote_tmp_tarball} "
            f"-C {remote_release_dir} && rm {remote_tmp_tarball}",
        ],
        check=True,
    )

    print("[docsmith-site] === validating extraction (checking for index.html) ===")
    subprocess.run(
        ["ssh", *ssh_key_flags, host, f"test -f {remote_release_dir}/index.html"],
        check=True,
    )

    print(f"[docsmith-site] === activating release: {base_path}/current -> {remote_release_dir} ===")
    subprocess.run(
        ["ssh", *ssh_key_flags, host, f"ln -sfn {remote_release_dir} {base_path}/current"],
        check=True,
    )

    if reload_cmd:
        print(f"[docsmith-site] === reloading remote server: {reload_cmd} ===")
        subprocess.run(["ssh", *ssh_key_flags, host, reload_cmd], check=True)
    else:
        print("[docsmith-site] DOCS_DEPLOY_RELOAD_CMD not set, skipping reload step")

    print(
        f"[docsmith-site] deploy complete: {host}:{remote_release_dir} is now live "
        f"at {base_path}/current"
    )
    return 0


# --- GitHub Release publish ---------------------------------------------------


def _github_api_request(
    url: str,
    token: str,
    method: str = "GET",
    json_body: Optional[dict] = None,
    raw_body: Optional[bytes] = None,
    content_type: Optional[str] = None,
) -> tuple[int, bytes]:
    """Issue a single GitHub REST API request via stdlib urllib.

    Returns (status_code, response_body_bytes). On HTTPError, prints the
    status and body for debuggability, then re-raises."""
    headers = {
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
    }
    data: Optional[bytes] = None
    if json_body is not None:
        data = json.dumps(json_body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    elif raw_body is not None:
        data = raw_body
        if content_type:
            headers["Content-Type"] = content_type

    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as exc:
        error_body = exc.read().decode("utf-8", errors="replace")
        print(f"[docsmith-site] GitHub API error: {method} {url} -> HTTP {exc.code}")
        print(f"[docsmith-site] response body: {error_body}")
        raise


def _get_or_create_github_release(repo: str, token: str, tag: str) -> dict:
    """Return the release dict for `tag`, creating it if it doesn't exist."""
    print(f"[docsmith-site] === checking for existing release '{tag}' ===")
    try:
        _status, body = _github_api_request(
            f"https://api.github.com/repos/{repo}/releases/tags/{tag}",
            token,
            method="GET",
        )
        print(f"[docsmith-site] found existing release '{tag}'")
        return json.loads(body)
    except urllib.error.HTTPError as exc:
        if exc.code != 404:
            raise

    print(f"[docsmith-site] === creating release '{tag}' (none found) ===")
    _status, body = _github_api_request(
        f"https://api.github.com/repos/{repo}/releases",
        token,
        method="POST",
        json_body={
            "tag_name": tag,
            "name": tag,
            "body": "Automated documentation site publish.",
            "draft": False,
            "prerelease": False,
        },
    )
    return json.loads(body)


def _delete_stale_asset_if_present(repo: str, token: str, release_id: int, asset_name: str) -> None:
    """Delete any existing release asset named `asset_name`, making the
    upload idempotent."""
    print(
        f"[docsmith-site] === checking for existing asset '{asset_name}' "
        f"on release {release_id} ==="
    )
    _status, body = _github_api_request(
        f"https://api.github.com/repos/{repo}/releases/{release_id}/assets",
        token,
        method="GET",
    )
    for asset in json.loads(body):
        if asset.get("name") == asset_name:
            asset_id = asset["id"]
            print(f"[docsmith-site] === deleting stale asset '{asset_name}' (id {asset_id}) ===")
            _github_api_request(
                f"https://api.github.com/repos/{repo}/releases/assets/{asset_id}",
                token,
                method="DELETE",
            )
            return


def _upload_release_asset(
    upload_url_template: str, token: str, asset_name: str, tarball_path: Path
) -> None:
    """Upload `tarball_path` as a release asset named `asset_name`."""
    base_upload_url = upload_url_template.split("{", 1)[0]
    upload_url = f"{base_upload_url}?name={asset_name}"

    print(
        f"[docsmith-site] === uploading '{tarball_path.name}' as release asset "
        f"'{asset_name}' ==="
    )
    _github_api_request(
        upload_url,
        token,
        method="POST",
        raw_body=tarball_path.read_bytes(),
        content_type="application/gzip",
    )
    print(f"[docsmith-site] uploaded release asset: '{asset_name}'")


def stage_publish(ctx: SiteContext) -> int:
    """Package the site and publish the tarball as a GitHub Release asset.

    Environment variables:

    - DOCS_PUBLISH_MODE (optional) -- defaults to 'github-release'; the only
      implemented mode (others fail loudly rather than no-op-ing).
    - GITHUB_TOKEN (required) -- token with repo scope.
    - GITHUB_REPOSITORY (required) -- 'owner/repo' form.
    - DOCS_RELEASE_TAG (optional) -- defaults to 'docs-latest'.

    Safely re-runnable: an existing release with the tag is reused, and a
    same-named stale asset is deleted before re-uploading.
    """
    package_result = stage_package(ctx)
    if package_result != 0:
        print(
            f"[docsmith-site] error: package failed (exit code {package_result}), "
            "refusing to publish",
            file=sys.stderr,
        )
        return package_result

    publish_mode = os.environ.get("DOCS_PUBLISH_MODE", "github-release")
    if publish_mode != "github-release":
        print(
            f"[docsmith-site] error: DOCS_PUBLISH_MODE='{publish_mode}' is not implemented -- "
            "only 'github-release' is currently implemented",
            file=sys.stderr,
        )
        return 1

    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        print(
            "[docsmith-site] error: GITHUB_TOKEN is required for github-release publish mode "
            "but was not set -- refusing to publish",
            file=sys.stderr,
        )
        return 1

    repo = os.environ.get("GITHUB_REPOSITORY")
    if not repo:
        print(
            "[docsmith-site] error: GITHUB_REPOSITORY is required (e.g. 'org/repo') "
            "but was not set -- refusing to publish",
            file=sys.stderr,
        )
        return 1

    tag = os.environ.get("DOCS_RELEASE_TAG", "docs-latest")
    asset_name = _tarball_name(ctx.config)
    tarball_path = ctx.workdir / "autogen" / asset_name

    release = _get_or_create_github_release(repo, token, tag)
    release_id = release["id"]
    upload_url_template = release["upload_url"]

    _delete_stale_asset_if_present(repo, token, release_id, asset_name)
    _upload_release_asset(upload_url_template, token, asset_name, tarball_path)

    print(
        f"[docsmith-site] publish complete: '{asset_name}' attached to release '{tag}' on {repo}"
    )
    return 0


# --- Entry point --------------------------------------------------------------


_STAGE_HANDLERS: dict[str, Callable[[SiteContext], int]] = {
    "render": stage_render,
    "build": stage_build,
    "serve": stage_serve,
    "check": stage_check,
    "clean": stage_clean,
    "package": stage_package,
    "publish": stage_publish,
    "deploy": stage_deploy,
}


def run(args: argparse.Namespace, project_root: Path, config: dict) -> int:
    """CLI entry: dispatch on args.stage, honoring --site-workdir and
    site.enabled."""
    stage = getattr(args, "stage", None) or "render"
    ctx = build_site_context(project_root, config, getattr(args, "site_workdir", None))

    if not (config.get("site", {}) or {}).get("enabled", False):
        if stage in _STAGES_OK_WHEN_DISABLED:
            print(
                f"docsmith: site is disabled (site.enabled=false); "
                f"nothing to do for 'collect {stage}'"
            )
            return 0
        print(
            f"docsmith: site is disabled (site.enabled=false); cannot run 'collect {stage}' "
            "-- enable it in .docsmith/config.json first",
            file=sys.stderr,
        )
        return 2

    handler = _STAGE_HANDLERS.get(stage)
    if handler is None:
        print(f"docsmith: unknown collect stage '{stage}'", file=sys.stderr)
        return 2
    return handler(ctx)
