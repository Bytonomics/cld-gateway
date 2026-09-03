# VENDORED by docsmith scaffold — source: ~/.claude/skills/docsmith — engine 1.0.0 — resync: docsmith scaffold --sync-engine
#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyyaml", "jinja2"]
# ///
"""
docsmith — deterministic, config-driven documentation tooling CLI.

Subcommands:
    validate    Run documentation validators (frontmatter, links, drift,
                docmap, coverage, plan-ttl, decisions)
    score       Score documentation freshness
    collect     Collect docs into the site workdir (render|build|serve|
                check|clean|package|publish|deploy)
    scaffold    Scaffold docsmith config into a project
    hook-check  (hidden) Gate a single file edit against the docmap; used
                by the editor hook, not meant for interactive use

Global flags (accepted before or after the subcommand):
    --project-root PATH   Project root (skips upward config discovery)
    --json                Emit machine-readable JSON output
    --quiet               Suppress non-essential output

Config discovery runs for every command EXCEPT scaffold: the project root
is the first directory (walking up from --project-root or cwd) containing
.docsmith/config.json.

Exit codes:
    0 - Pass
    1 - Errors found / threshold failed
    2 - Internal error / command not implemented
    3 - Config not found
"""

import argparse
import json
import sys
import traceback
from collections.abc import Callable
from pathlib import Path
from typing import Optional

# When executed as a script, Python prepends this file's directory to
# sys.path automatically, which makes the sibling `docsmith_lib` package
# importable without any installation step.
from docsmith_lib import config as config_lib
from docsmith_lib import docmap as docmap_lib
from docsmith_lib import hookmsg as hookmsg_lib
from docsmith_lib import scaffold as scaffold_lib
from docsmith_lib import score as score_lib
from docsmith_lib import validate as validate_lib
from docsmith_lib.site import engine as site_engine

_COLLECT_STAGES = [
    "render",
    "build",
    "serve",
    "check",
    "clean",
    "package",
    "publish",
    "deploy",
]

_VALIDATE_CHECKS = "frontmatter,links,drift,docmap,coverage,plan-ttl,decisions"


def _add_global_flags(parser: argparse.ArgumentParser, *, suppress_defaults: bool) -> None:
    """Register the global flags on a parser.

    The same flags are registered on both the main parser (real defaults)
    and on each subparser via a parent parser (argparse.SUPPRESS defaults),
    so they are accepted before or after the subcommand. SUPPRESS keeps a
    subparser from clobbering a value parsed before the subcommand.
    """
    parser.add_argument(
        "--project-root",
        metavar="PATH",
        default=argparse.SUPPRESS if suppress_defaults else None,
        help="Project root (skips upward config discovery)",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        default=argparse.SUPPRESS if suppress_defaults else False,
        help="Emit machine-readable JSON output",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        default=argparse.SUPPRESS if suppress_defaults else False,
        help="Suppress non-essential output",
    )


def _build_parser() -> argparse.ArgumentParser:
    """Build the full argument parser (all subcommands, all flags)."""
    parser = argparse.ArgumentParser(
        prog="docsmith",
        description="Deterministic, config-driven documentation tooling.",
    )
    _add_global_flags(parser, suppress_defaults=False)

    global_flags = argparse.ArgumentParser(add_help=False)
    _add_global_flags(global_flags, suppress_defaults=True)

    # hook-check is registered but hidden: the explicit metavar lists only
    # the public subcommands, and hook-check gets no help text.
    subparsers = parser.add_subparsers(
        dest="command",
        required=True,
        metavar="{validate,score,collect,scaffold}",
    )

    validate = subparsers.add_parser(
        "validate",
        parents=[global_flags],
        help="Run documentation validators",
    )
    validate.add_argument(
        "--only",
        metavar="CSV",
        help=f"Comma-separated subset of checks to run: {_VALIDATE_CHECKS}",
    )
    validate.add_argument(
        "--paths",
        metavar="GLOB",
        action="append",
        default=[],
        help="Limit validation to paths matching GLOB (repeatable)",
    )
    validate.add_argument(
        "--strict",
        action="store_true",
        help="Treat warnings as failures",
    )

    score = subparsers.add_parser(
        "score",
        parents=[global_flags],
        help="Score documentation freshness",
    )
    score.add_argument(
        "--all",
        action="store_true",
        default=True,
        help="Score all docs (default)",
    )
    score.add_argument(
        "--doc",
        metavar="PATH",
        action="append",
        default=[],
        help="Score a specific doc (repeatable)",
    )
    score.add_argument(
        "--threshold",
        metavar="INT",
        type=int,
        help="Per-doc freshness threshold override",
    )
    score.add_argument(
        "--fail-under",
        metavar="INT",
        type=int,
        help="Exit non-zero when the aggregate score is below INT",
    )
    score.add_argument(
        "--update-state",
        action="store_true",
        help="Persist computed scores to the state file",
    )
    score.add_argument(
        "--report",
        action="store_true",
        help="Emit a full per-doc report",
    )

    collect = subparsers.add_parser(
        "collect",
        parents=[global_flags],
        help="Collect docs into the site workdir",
    )
    collect.add_argument(
        "stage",
        nargs="?",
        choices=_COLLECT_STAGES,
        default="render",
        help="Site pipeline stage to run (default: render)",
    )
    collect.add_argument(
        "--site-workdir",
        metavar="PATH",
        help="Override the site workdir from config",
    )

    scaffold = subparsers.add_parser(
        "scaffold",
        parents=[global_flags],
        help="Scaffold docsmith config into a project",
    )
    scaffold.add_argument(
        "--profile",
        choices=["full", "standard", "minimal"],
        default="standard",
        help="Config profile to scaffold (default: standard)",
    )
    scaffold.add_argument(
        "--adopt",
        action="store_true",
        help="Adopt an existing docs tree instead of creating a fresh one",
    )
    scaffold.add_argument(
        "--non-interactive",
        action="store_true",
        help="Never prompt; use defaults and --answers",
    )
    scaffold.add_argument(
        "--answers",
        metavar="FILE",
        help="JSON file of scaffold answers (for --non-interactive)",
    )
    scaffold.add_argument(
        "--sync-engine",
        action="store_true",
        help="Refresh engine-managed files in an already-scaffolded project",
    )
    scaffold.add_argument(
        "--force",
        action="store_true",
        help="Overwrite existing files",
    )

    hook_check = subparsers.add_parser("hook-check", parents=[global_flags])
    hook_check.add_argument(
        "--file",
        metavar="ABS_PATH",
        required=True,
        help="Absolute path of the edited file to gate against the docmap",
    )

    return parser


def _cmd_validate(
    args: argparse.Namespace,
    project_root: Optional[Path],
    config: Optional[dict],
) -> int:
    """Delegate to docsmith_lib.validate."""
    return validate_lib.run(args, project_root, config)


def _cmd_score(
    args: argparse.Namespace,
    project_root: Optional[Path],
    config: Optional[dict],
) -> int:
    """Delegate to docsmith_lib.score."""
    return score_lib.run(args, project_root, config)


def _cmd_collect(
    args: argparse.Namespace,
    project_root: Optional[Path],
    config: Optional[dict],
) -> int:
    """Delegate to docsmith_lib.site.engine."""
    if project_root is None or config is None:
        print("docsmith: internal error: collect requires a loaded config", file=sys.stderr)
        return 2
    return site_engine.run(args, project_root, config)


def _cmd_scaffold(
    args: argparse.Namespace,
    project_root: Optional[Path],
    config: Optional[dict],
) -> int:
    """Delegate to docsmith_lib.scaffold (does its own root resolution)."""
    return scaffold_lib.run(args, project_root, config)


def _cmd_hook_check(
    args: argparse.Namespace,
    project_root: Optional[Path],
    config: Optional[dict],
) -> int:
    """Gate a single file edit against the docmap (parity seam for the
    generic PostToolUse hook in scripts/hook/docsmith_hook.py).

    Exit codes: 0 no mapped docs (or .md / outside-project / no docmap),
    2 mapped docs found (message printed to stdout, or JSON with --json).
    """
    if project_root is None or config is None:
        print("docsmith: internal error: hook-check requires a loaded config", file=sys.stderr)
        return 2
    root = Path(project_root).resolve()

    file_path = Path(args.file)
    if not file_path.is_absolute():
        file_path = root / file_path
    file_path = file_path.resolve()

    def _emit_empty(note: str = "") -> None:
        if args.json:
            print(json.dumps({"matched": []}))
        elif note and not args.quiet:
            print(note)

    if file_path.name.endswith(".md"):
        _emit_empty("skipped (.md)")
        return 0

    try:
        rel_path = file_path.relative_to(root).as_posix()
    except ValueError:
        _emit_empty()
        return 0

    try:
        data = docmap_lib.load_docmap(root)
    except (FileNotFoundError, ValueError):
        _emit_empty()
        return 0

    matched = docmap_lib.find_matching_docs(rel_path, data["map"])
    if not matched:
        _emit_empty()
        return 0

    message = hookmsg_lib.build(matched, config)
    if args.json:
        print(json.dumps({"matched": matched, "message": message}))
    else:
        print(message)
    return 2


_COMMAND_HANDLERS: dict[str, Callable[[argparse.Namespace, Optional[Path], Optional[dict]], int]] = {
    "validate": _cmd_validate,
    "score": _cmd_score,
    "collect": _cmd_collect,
    "scaffold": _cmd_scaffold,
    "hook-check": _cmd_hook_check,
}


def main(argv: Optional[list[str]] = None) -> int:
    """Parse arguments, discover config (except for scaffold), dispatch."""
    parser = _build_parser()
    args = parser.parse_args(argv)

    project_root: Optional[Path] = None
    config: Optional[dict] = None
    if args.command != "scaffold":
        try:
            if args.project_root:
                project_root, config = config_lib.load_config(
                    project_root=Path(args.project_root)
                )
            else:
                project_root, config = config_lib.load_config(start=Path.cwd())
        except config_lib.ConfigNotFoundError as e:
            print(f"docsmith: {e}", file=sys.stderr)
            return 3
        except ValueError as e:
            print(f"docsmith: invalid config: {e}", file=sys.stderr)
            return 2

    handler = _COMMAND_HANDLERS[args.command]
    return handler(args, project_root, config)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        print(f"docsmith: internal error: {e}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        sys.exit(2)
