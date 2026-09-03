"""The `docsmith scaffold` command (wave A4).

Bootstraps a project for docsmith:

    1. Resolves the project root (--project-root, else `git rev-parse
       --show-toplevel`).
    2. Collects answers (defaults -> --answers file -> interactive prompts
       when on a tty and not --non-interactive).
    3. Writes .docsmith/config.json (full materialized
       DEFAULT_CONFIG with the answers applied) and docmap.json (migrated
       from a legacy .claude/hooks/doc_sync_map.json when adopting, else
       empty), refusing to overwrite either without --force.
    4. Vendors the engine (docsmith.py + docsmith_lib/) into
       .docsmith/engine/ so the project is self-contained.
    5. Writes the state.json skeleton, .gitignore lines, the generic
       PostToolUse hook (+ .claude/settings.json entry), pre-commit hooks
       under the first `repo: local` block, and the category directories.

`--sync-engine` re-copies ONLY the vendored engine (and refreshes the
config's engine_version) in an already-scaffolded project.

Adopt mode (--adopt, or auto-detected from .claude/hooks/doc_sync_map.json
/ docs-site/sources.yml) additionally harvests the legacy docs-site
sources.yml into the config's `site` and `okf_compat` sections.
"""

import argparse
import copy
import datetime
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Optional

import yaml

from . import ENGINE_VERSION
from . import config as config_lib
from . import docmap as docmap_lib

STATE_REL_PATH = Path(".docsmith") / "state.json"
ENGINE_REL_PATH = Path(".docsmith") / "engine"

LEGACY_MAP_REL_PATH = Path(".claude") / "hooks" / "doc_sync_map.json"
LEGACY_SOURCES_REL_PATH = Path("docs-site") / "sources.yml"

_HOOK_MATCHER = "Edit|Write|MultiEdit|Update|NotebookEdit"
_HOOK_COMMAND = "python3 .claude/hooks/docsmith_hook.py"

_GITIGNORE_LINES = [
    ".docsmith/state.json",
    ".docsmith/site/autogen/",
    ".docsmith/site/.venv/",
    ".docsmith/site/site/",
]

_VENDOR_HEADER_PREFIX = "# VENDORED by docsmith scaffold"
_VENDOR_HEADER = (
    f"{_VENDOR_HEADER_PREFIX} — source: ~/.claude/skills/docsmith — "
    f"engine {ENGINE_VERSION} — resync: docsmith scaffold --sync-engine\n"
)

_ENGINE_COPY_IGNORE = shutil.ignore_patterns("__pycache__", "tests", ".pytest_cache")

# Legacy sources.yml `site:` keys copied verbatim into config site (only
# those that are present in the yaml AND known to the config schema).
_SITE_IDENTITY_KEYS = [
    "name",
    "description",
    "site_author",
    "repo_name",
    "repo_url",
    "edit_branch",
    "copyright",
]

# Answer keys in prompt order.
_ANSWER_KEYS = [
    "project_name",
    "repo_url",
    "docs_dir",
    "profile",
    "site_enabled",
    "site_name",
    "install_hook",
    "wire_precommit",
]

_GLOB_CHARS = set("*?[")

_PRECOMMIT_VALIDATE_ID = "docsmith-validate"
_PRECOMMIT_COLLECT_ID = "docsmith-collect-render"
_PRECOMMIT_VALIDATE_ENTRY = "uv run .docsmith/engine/docsmith.py validate"


class _Summary:
    """Accumulates written / skipped / notice lines for the final table."""

    def __init__(self) -> None:
        self.written: list[str] = []
        self.skipped: list[str] = []
        self.notices: list[str] = []

    def render(self) -> str:
        lines = ["docsmith scaffold summary:"]
        for title, items in (
            ("written", self.written),
            ("skipped", self.skipped),
            ("notices", self.notices),
        ):
            if items:
                lines.append(f"  {title}:")
                lines.extend(f"    - {item}" for item in items)
        lines.append("")
        lines.append("Next steps:")
        lines.append("  make setup      # provision uv, the doc-site venv + deps, and pre-commit hooks")
        lines.append("  make validate   # (or: uv run .docsmith/engine/docsmith.py validate)")
        lines.append("  make docs-build # build the static doc site")
        return "\n".join(lines)


# --- root + answers ----------------------------------------------------------


def _resolve_root(args: argparse.Namespace) -> Optional[Path]:
    """--project-root when given, else the enclosing git toplevel. None when
    neither yields a usable directory."""
    if getattr(args, "project_root", None):
        root = Path(args.project_root).resolve()
        return root if root.is_dir() else None
    proc = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0 or not proc.stdout.strip():
        return None
    return Path(proc.stdout.strip()).resolve()


def _normalized_repo_url(root: Path) -> Optional[str]:
    """origin remote URL normalized to https form (no .git suffix), or None."""
    proc = subprocess.run(
        ["git", "-C", str(root), "remote", "get-url", "origin"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    url = proc.stdout.strip()
    if not url:
        return None
    if url.endswith(".git"):
        url = url[: -len(".git")]
    scp_like = re.match(r"^git@([^:/]+):(.+)$", url)
    if scp_like:
        return f"https://{scp_like.group(1)}/{scp_like.group(2)}"
    ssh_url = re.match(r"^ssh://git@([^/]+)/(.+)$", url)
    if ssh_url:
        return f"https://{ssh_url.group(1)}/{ssh_url.group(2)}"
    return url


def _prompt_answers(defaults: dict) -> dict:
    """One input() prompt per answer key showing the default; empty input
    keeps the default. Booleans accept y/yes/true/1 (anything else: no)."""
    answers = dict(defaults)
    for key in _ANSWER_KEYS:
        default = answers[key]
        raw = input(f"{key} [{default}]: ").strip()
        if not raw:
            continue
        if isinstance(default, bool):
            answers[key] = raw.lower() in ("y", "yes", "true", "1")
        else:
            answers[key] = raw
    return answers


def _collect_answers(args: argparse.Namespace, root: Path) -> dict:
    project_name = root.name
    answers = {
        "project_name": project_name,
        "repo_url": _normalized_repo_url(root),
        "docs_dir": "docs",
        "profile": args.profile,
        "site_enabled": True,
        "site_name": f"{project_name} Docs",
        "install_hook": True,
        "wire_precommit": True,
    }
    if args.answers:
        with open(args.answers, encoding="utf-8") as f:
            overrides = json.load(f)
        for key, value in overrides.items():
            if key in answers:
                answers[key] = value
    if not args.non_interactive and sys.stdin.isatty():
        answers = _prompt_answers(answers)
    return answers


# --- config + docmap ---------------------------------------------------------


def _harvest_legacy_site(root: Path, cfg: dict, summary: _Summary) -> None:
    """Adopt-mode harvesting of docs-site/sources.yml into the config."""
    sources_path = root / LEGACY_SOURCES_REL_PATH
    if not sources_path.is_file():
        return
    with open(sources_path, encoding="utf-8") as f:
        legacy = yaml.safe_load(f)
    if not isinstance(legacy, dict):
        return

    site = legacy.get("site")
    if isinstance(site, dict):
        for key in _SITE_IDENTITY_KEYS:
            if key in site:
                cfg["site"][key] = site[key]
    if isinstance(legacy.get("exclude"), dict):
        cfg["site"]["exclude"] = legacy["exclude"]
    if isinstance(legacy.get("sources"), list):
        cfg["site"]["sources"] = legacy["sources"]
    okf = legacy.get("okf")
    if isinstance(okf, dict):
        cfg["okf_compat"]["allowed_types"] = okf.get("allowed_types", [])
        cfg["okf_compat"]["mapping_rules"] = okf.get("mapping_rules", [])
    cfg["site"]["workdir"] = "docs-site"
    cfg["site"]["enabled"] = True
    summary.notices.append(
        "adopted docs-site/sources.yml into config site/okf_compat "
        "(workdir docs-site, site enabled)"
    )


def _build_config(root: Path, answers: dict, adopt: bool, summary: _Summary) -> dict:
    cfg = copy.deepcopy(config_lib.DEFAULT_CONFIG)
    cfg["engine_version"] = ENGINE_VERSION
    cfg["project"]["name"] = answers["project_name"]
    cfg["project"]["repo_url"] = answers["repo_url"]
    cfg["project"]["docs_dir"] = answers["docs_dir"]
    cfg["profile"] = answers["profile"]
    cfg["site"]["enabled"] = bool(answers["site_enabled"])
    cfg["site"]["name"] = answers["site_name"]
    if adopt:
        _harvest_legacy_site(root, cfg, summary)
    # When the site is enabled but no sources are configured (a fresh,
    # non-adopt scaffold), default to collecting the whole docs tree so the
    # built site actually contains the docs instead of an empty landing page.
    if cfg["site"]["enabled"] and not cfg["site"].get("sources"):
        docs_dir = str(answers["docs_dir"]).strip("/") or "docs"
        cfg["site"]["sources"] = [
            {
                "id": "docs",
                "collector": "markdown_tree",
                "path": docs_dir,
                "destination": ".",
            }
        ]
    return cfg


def _write_json_guarded(
    path: Path,
    rel_label: str,
    payload: dict,
    force: bool,
    summary: _Summary,
) -> bool:
    """Write a JSON file, refusing to overwrite without --force."""
    if path.exists() and not force:
        print(
            f"docsmith: {rel_label} already exists at '{path}' "
            "(use --force to overwrite)",
            file=sys.stderr,
        )
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    summary.written.append(rel_label)
    return True


def _build_docmap(root: Path, now_iso: str, summary: _Summary) -> dict:
    legacy_path = root / LEGACY_MAP_REL_PATH
    if legacy_path.is_file():
        with open(legacy_path, encoding="utf-8") as f:
            legacy = json.load(f)
        summary.notices.append(
            "legacy map migrated — review, then delete "
            ".claude/hooks/doc_sync_map.json after the hook swap"
        )
        return docmap_lib.migrate_legacy(legacy, now_iso)
    return {"version": 1, "updated_at": now_iso, "map": {}}


# --- engine vendoring --------------------------------------------------------


def _skill_scripts_dir() -> Path:
    """The scripts/ directory of THIS engine install (skill dir or an
    already-vendored engine)."""
    return Path(__file__).resolve().parent.parent


def _vendor_engine(root: Path) -> None:
    """Copy docsmith.py + docsmith_lib/ into <root>/.docsmith/engine/
    (side by side, so the CLI's sibling-package import keeps working)."""
    scripts_dir = _skill_scripts_dir()
    engine_dir = root / ENGINE_REL_PATH
    lib_source = scripts_dir / "docsmith_lib"
    lib_dest = engine_dir / "docsmith_lib"
    if lib_source.resolve() == lib_dest.resolve():
        return  # running FROM this project's vendored engine: nothing to copy
    if lib_dest.exists():
        shutil.rmtree(lib_dest)
    engine_dir.mkdir(parents=True, exist_ok=True)
    shutil.copytree(lib_source, lib_dest, ignore=_ENGINE_COPY_IGNORE)

    source_text = (scripts_dir / "docsmith.py").read_text(encoding="utf-8")
    if not source_text.startswith(_VENDOR_HEADER_PREFIX):
        source_text = _VENDOR_HEADER + source_text
    (engine_dir / "docsmith.py").write_text(source_text, encoding="utf-8")


def _run_sync_engine(root: Path, quiet: bool) -> int:
    """--sync-engine: refresh the vendored engine only."""
    config_path = root / config_lib.CONFIG_REL_PATH
    if not config_path.is_file():
        print(
            "docsmith: --sync-engine requires an existing "
            f"{config_lib.CONFIG_REL_PATH} (run 'docsmith scaffold' first)",
            file=sys.stderr,
        )
        return 2
    _vendor_engine(root)

    with open(config_path, encoding="utf-8") as f:
        raw_config = json.load(f)
    raw_config["engine_version"] = ENGINE_VERSION
    config_path.write_text(json.dumps(raw_config, indent=2) + "\n", encoding="utf-8")

    if not quiet:
        summary = _Summary()
        summary.written.append(f"{ENGINE_REL_PATH}/ (engine {ENGINE_VERSION})")
        summary.written.append(f"{config_lib.CONFIG_REL_PATH} (engine_version)")
        print(summary.render())
    return 0


# --- state, gitignore, hook, pre-commit, category dirs -----------------------


def _write_state_skeleton(root: Path, summary: _Summary) -> None:
    state_path = root / STATE_REL_PATH
    if state_path.exists():
        summary.skipped.append(f"{STATE_REL_PATH} (exists)")
        return
    payload = {"version": 1, "last_scan": None, "scores": {}, "validate": None}
    state_path.parent.mkdir(parents=True, exist_ok=True)
    state_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    summary.written.append(str(STATE_REL_PATH))


def _append_gitignore(root: Path, summary: _Summary) -> None:
    gitignore_path = root / ".gitignore"
    existing = (
        gitignore_path.read_text(encoding="utf-8") if gitignore_path.exists() else ""
    )
    existing_lines = existing.split("\n")
    to_add = [line for line in _GITIGNORE_LINES if line not in existing_lines]
    if not to_add:
        summary.skipped.append(".gitignore (lines present)")
        return
    text = existing
    if text and not text.endswith("\n"):
        text += "\n"
    text += "\n".join(to_add) + "\n"
    gitignore_path.write_text(text, encoding="utf-8")
    summary.written.append(f".gitignore (+{len(to_add)} line(s))")


def _install_hook(root: Path, summary: _Summary) -> None:
    """Copy the generic hook and register it in .claude/settings.json."""
    legacy_hook = root / ".claude" / "hooks" / "posttooluse_file.py"
    if legacy_hook.is_file():
        content = legacy_hook.read_text(encoding="utf-8")
        if "doc-sync" in content or "docsmith" in content:
            summary.notices.append(
                ".claude/hooks/posttooluse_file.py already handles doc sync — "
                "integrate .claude/hooks/docsmith_hook.py manually"
            )
            return

    hooks_dir = root / ".claude" / "hooks"
    hooks_dir.mkdir(parents=True, exist_ok=True)
    hook_source = _skill_scripts_dir() / "hook" / "docsmith_hook.py"
    shutil.copyfile(hook_source, hooks_dir / "docsmith_hook.py")
    summary.written.append(".claude/hooks/docsmith_hook.py")
    _merge_settings(root, summary)


def _merge_settings(root: Path, summary: _Summary) -> None:
    """Merge the PostToolUse hook entry into .claude/settings.json,
    preserving all existing content. Idempotent."""
    settings_path = root / ".claude" / "settings.json"
    settings: dict = {}
    if settings_path.is_file():
        try:
            settings = json.loads(settings_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            summary.notices.append(
                ".claude/settings.json is not valid JSON — add the "
                "docsmith_hook.py PostToolUse entry manually"
            )
            return
        if not isinstance(settings, dict):
            summary.notices.append(
                ".claude/settings.json is not a JSON object — add the "
                "docsmith_hook.py PostToolUse entry manually"
            )
            return

    hooks = settings.setdefault("hooks", {})
    post_tool_use = hooks.setdefault("PostToolUse", [])
    for entry in post_tool_use:
        if not isinstance(entry, dict):
            continue
        for hook in entry.get("hooks", []):
            if isinstance(hook, dict) and "docsmith_hook.py" in str(
                hook.get("command", "")
            ):
                summary.skipped.append(".claude/settings.json (hook entry present)")
                return

    post_tool_use.append({
        "matcher": _HOOK_MATCHER,
        "hooks": [{"type": "command", "command": _HOOK_COMMAND}],
    })
    settings_path.write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")
    summary.written.append(".claude/settings.json (PostToolUse entry)")


def _precommit_block(indent: str, hook_id: str, name: str, entry: str) -> list[str]:
    # `name` is required by pre-commit for `repo: local` hooks, so it is
    # emitted even though it is derived, not configured.
    return [
        f"{indent}- id: {hook_id}",
        f"{indent}  name: {name}",
        f"{indent}  entry: {entry}",
        f"{indent}  language: system",
        f"{indent}  types: [markdown]",
        f"{indent}  pass_filenames: false",
    ]


def _collect_render_entry(workdir: str) -> str:
    return (
        "bash -c 'uv run .docsmith/engine/docsmith.py collect render "
        f"&& git diff --quiet {workdir}/mkdocs.yml "
        f"|| git add {workdir}/mkdocs.yml'"
    )


def _project_templates_dir() -> Path:
    """The directory of shipped project-file templates copied on scaffold."""
    return _skill_scripts_dir() / "project_templates"


def _wire_precommit(root: Path, cfg: dict, summary: _Summary) -> None:
    """Idempotently ensure the docsmith hooks exist in .pre-commit-config.yaml.

    Same end result whether or not the file exists:
      - absent            -> copy the shipped template, then append the
                             collect-render hook when the site is enabled
      - no `repo: local`  -> append a `repo: local` block with the hooks
      - existing block    -> insert the missing hooks under its `hooks:` list
    Re-running is a no-op once the hook ids are present (deduped by id).
    """
    site = cfg.get("site", {})
    site_enabled = bool(site.get("enabled"))
    workdir = str(site.get("workdir", ".docsmith/site")).strip("/")

    def _missing_hooks(indent: str, existing: str) -> list[str]:
        blk: list[str] = []
        if f"id: {_PRECOMMIT_VALIDATE_ID}" not in existing:
            blk += _precommit_block(
                indent, _PRECOMMIT_VALIDATE_ID, "docsmith validate",
                _PRECOMMIT_VALIDATE_ENTRY,
            )
        if site_enabled and f"id: {_PRECOMMIT_COLLECT_ID}" not in existing:
            blk += _precommit_block(
                indent, _PRECOMMIT_COLLECT_ID, "docsmith collect render",
                _collect_render_entry(workdir),
            )
        return blk

    precommit_path = root / ".pre-commit-config.yaml"

    # Case A: absent -> copy the shipped template, then top up the site hook.
    if not precommit_path.is_file():
        shutil.copyfile(
            _project_templates_dir() / "pre-commit-config.yaml", precommit_path
        )
        text = precommit_path.read_text(encoding="utf-8")
        add = _missing_hooks("      ", text)
        if add:
            sep = "" if text.endswith("\n") else "\n"
            precommit_path.write_text(
                text + sep + "\n".join(add) + "\n", encoding="utf-8"
            )
        summary.written.append(".pre-commit-config.yaml (created from template)")
        return

    text = precommit_path.read_text(encoding="utf-8")
    lines = text.split("\n")

    repo_local_idx = None
    for i, line in enumerate(lines):
        if re.match(r"^\s*-\s+repo:\s*local\s*$", line):
            repo_local_idx = i
            break

    hooks_idx = None
    if repo_local_idx is not None:
        for i in range(repo_local_idx + 1, len(lines)):
            if re.match(r"^\s*-\s+repo:", lines[i]):
                break
            if re.match(r"^\s*hooks:\s*$", lines[i]):
                hooks_idx = i
                break

    # Case B: file exists but no usable `repo: local` + `hooks:` -> append one.
    if repo_local_idx is None or hooks_idx is None:
        add = _missing_hooks("      ", text)
        if not add:
            summary.skipped.append(".pre-commit-config.yaml (docsmith hooks present)")
            return
        block: list[str] = []
        if not re.search(r"(?m)^repos:\s*$", text):
            block.append("repos:")
        block += ["  - repo: local", "    hooks:"] + add
        sep = "" if (text == "" or text.endswith("\n")) else "\n"
        precommit_path.write_text(
            text + sep + "\n".join(block) + "\n", encoding="utf-8"
        )
        summary.written.append(
            ".pre-commit-config.yaml (appended docsmith repo: local block)"
        )
        return

    # Case C: existing `repo: local` with a `hooks:` list -> insert (deduped).
    indent = None
    for i in range(hooks_idx + 1, len(lines)):
        if re.match(r"^\s*-\s+repo:", lines[i]):
            break
        id_match = re.match(r"^(\s*)-\s+id:", lines[i])
        if id_match:
            indent = id_match.group(1)
            break
    if indent is None:
        hooks_indent = re.match(r"^(\s*)", lines[hooks_idx]).group(1)
        indent = hooks_indent + "  "

    insert_lines = _missing_hooks(indent, text)
    if not insert_lines:
        summary.skipped.append(".pre-commit-config.yaml (docsmith hooks present)")
        return
    lines[hooks_idx + 1 : hooks_idx + 1] = insert_lines
    precommit_path.write_text("\n".join(lines), encoding="utf-8")
    summary.written.append(".pre-commit-config.yaml (docsmith hooks)")


def _copy_template(
    root: Path, template_name: str, dest_rel: str, summary: _Summary,
    *, marker: Optional[str] = None, executable: bool = False,
) -> None:
    """Copy a shipped project template to dest_rel when absent (idempotent).

    When the destination already exists it is left untouched; if `marker` is
    given and missing from the existing file, a notice is emitted instead of
    clobbering the user's file."""
    dest = root / dest_rel
    if dest.exists():
        if marker is not None and marker not in dest.read_text(encoding="utf-8"):
            summary.notices.append(
                f"{dest_rel} exists without the docsmith section — "
                f"merge from the shipped {template_name} template manually"
            )
        else:
            summary.skipped.append(f"{dest_rel} (exists)")
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(_project_templates_dir() / template_name, dest)
    if executable:
        dest.chmod(0o755)
    summary.written.append(dest_rel)


def _scaffold_makefile(root: Path, summary: _Summary) -> None:
    _copy_template(
        root, "Makefile", "Makefile", summary,
        marker="uv run .docsmith/engine",
    )


def _scaffold_site_requirements(root: Path, summary: _Summary) -> None:
    _copy_template(
        root, "site-requirements.txt", ".docsmith/site-requirements.txt", summary,
    )


def _scaffold_setup_script(root: Path, summary: _Summary) -> None:
    _copy_template(
        root, "setup.sh", ".docsmith/setup.sh", summary, executable=True,
    )


def _static_prefix(pattern: str) -> Optional[str]:
    """Leading glob-free path components of a dir pattern:
    'docs/decisions/**' -> 'docs/decisions'. None when the pattern starts
    with a glob component."""
    parts: list[str] = []
    for part in pattern.split("/"):
        if any(char in _GLOB_CHARS for char in part):
            break
        if part:
            parts.append(part)
    return "/".join(parts) if parts else None


def _create_category_dirs(root: Path, cfg: dict, summary: _Summary) -> None:
    """Create each enabled category's first-pattern static prefix dir (plus
    any archive_dir) with a .gitkeep — ONLY when the dir doesn't exist.
    Never seeds content docs."""
    for category in cfg.get("categories", {}).values():
        if not category.get("enabled", True):
            continue
        targets: list[str] = []
        patterns = category.get("dir_patterns") or []
        if patterns:
            prefix = _static_prefix(patterns[0])
            if prefix:
                targets.append(prefix)
        archive_dir = category.get("archive_dir")
        if archive_dir:
            targets.append(str(archive_dir).strip("/"))
        for rel in targets:
            target_dir = root / rel
            if target_dir.exists():
                continue
            target_dir.mkdir(parents=True)
            (target_dir / ".gitkeep").write_text("", encoding="utf-8")
            summary.written.append(f"{rel}/ (.gitkeep)")


# --- entry point -------------------------------------------------------------


def run(
    args: argparse.Namespace,
    project_root_ignored: Optional[Path],
    config_ignored: Optional[dict],
) -> int:
    """Execute `docsmith scaffold`. Does its own root resolution (the CLI
    skips config discovery for scaffold, so both extra params are None)."""
    root = _resolve_root(args)
    if root is None:
        print(
            "docsmith: scaffold requires a git repository "
            "(or an existing directory via --project-root)",
            file=sys.stderr,
        )
        return 2

    if args.sync_engine:
        return _run_sync_engine(root, args.quiet)

    adopt = (
        args.adopt
        or (root / LEGACY_MAP_REL_PATH).is_file()
        or (root / LEGACY_SOURCES_REL_PATH).is_file()
    )
    answers = _collect_answers(args, root)
    summary = _Summary()
    now_iso = datetime.datetime.now(datetime.timezone.utc).isoformat()

    cfg = _build_config(root, answers, adopt, summary)
    if not _write_json_guarded(
        root / config_lib.CONFIG_REL_PATH,
        str(config_lib.CONFIG_REL_PATH),
        cfg,
        args.force,
        summary,
    ):
        return 2

    docmap_payload = _build_docmap(root, now_iso, summary)
    if not _write_json_guarded(
        root / docmap_lib.DOCMAP_REL_PATH,
        str(docmap_lib.DOCMAP_REL_PATH),
        docmap_payload,
        args.force,
        summary,
    ):
        return 2

    _vendor_engine(root)
    summary.written.append(f"{ENGINE_REL_PATH}/ (engine {ENGINE_VERSION})")

    _write_state_skeleton(root, summary)
    _append_gitignore(root, summary)

    if answers["install_hook"]:
        _install_hook(root, summary)

    if answers["wire_precommit"]:
        _wire_precommit(root, cfg, summary)

    _scaffold_makefile(root, summary)
    if bool(cfg.get("site", {}).get("enabled")):
        _scaffold_site_requirements(root, summary)
    _scaffold_setup_script(root, summary)

    _create_category_dirs(root, cfg, summary)

    if not args.quiet:
        print(summary.render())
    return 0
