"""Configuration loading, merging, and category resolution for docsmith.

The single source of truth for a project's docsmith setup is
`.docsmith/config.json` at the project root. Effective configuration
is assembled in this exact order (later steps win over earlier ones for the
keys they touch):

    1. DEFAULT_CONFIG (engine defaults, defined here)
    2. The project's config.json file (deep-merged over defaults; file wins)
    3. The selected profile's defaults, applied ONLY to keys the file did
       not set (profile fills gaps; explicit file values win)
    4. The file's `overrides` dotted-path map, applied LAST
       (e.g. "validate.require_index_table": false)

A config file with `docsmith_version` != 1 is rejected with ValueError.
"""

import copy
import fnmatch
import json
from pathlib import Path
from typing import Any, Optional

from . import ENGINE_VERSION

# Relative location of the config file that marks a project root.
CONFIG_REL_PATH = Path(".docsmith") / "config.json"


class ConfigNotFoundError(Exception):
    """Raised when no .docsmith/config.json can be located."""


# Profile defaults. Applied AFTER the file merge, filling only the keys the
# project's config file left unset (see load_config step 3 above).
PROFILES: dict[str, dict[str, dict[str, Any]]] = {
    "full": {
        "validate": {
            "frontmatter": True,
            "links": True,
            "drift": True,
            "docmap": True,
            "coverage": True,
            "plan_ttl": True,
            "decisions": True,
            "require_index_table": True,
            "symbol_drift_severity": "warning",
        },
        "freshness": {
            "threshold": 70,
        },
    },
    "standard": {
        "validate": {
            "frontmatter": True,
            "links": True,
            "drift": True,
            "docmap": True,
            "coverage": True,
            "plan_ttl": True,
            "decisions": True,
            "require_index_table": False,
            "symbol_drift_severity": "warning",
        },
        "freshness": {
            "threshold": 70,
        },
    },
    "minimal": {
        "validate": {
            "frontmatter": True,
            "links": False,
            "drift": False,
            "docmap": True,
            "coverage": False,
            "plan_ttl": False,
            "decisions": False,
            "require_index_table": False,
            "symbol_drift_severity": "warning",
        },
        "freshness": {
            "threshold": 70,
        },
    },
}


# The 9 built-in documentation categories. Dict order matters: dir_pattern
# resolution (resolve_category step 3) walks categories in this order.
DEFAULT_CATEGORIES: dict[str, dict[str, Any]] = {
    "tutorial": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": ["docs/tutorials/**"],
        "type_aliases": ["Tutorial"],
        "template": "tutorial.md",
        "stale_after_days": 180,
        "site": {"include": True},
    },
    "how-to": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": ["docs/how-to/**"],
        "type_aliases": ["Guide"],
        "template": "how-to.md",
        "stale_after_days": 120,
        "site": {"include": True},
    },
    "reference": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": ["docs/reference/**"],
        "type_aliases": ["Reference", "API", "Spec", "Index", "Priming"],
        "template": "reference.md",
        "stale_after_days": 90,
        "site": {"include": True},
    },
    "explanation": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": ["docs/explanation/**"],
        "type_aliases": ["Overview"],
        "template": "explanation.md",
        "stale_after_days": 240,
        "site": {"include": True},
    },
    "runbook": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": ["docs/runbooks/**"],
        "type_aliases": ["Runbook"],
        "template": "runbook.md",
        "stale_after_days": 90,
        "site": {"include": True},
    },
    "prd": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": ["docs/prd/**"],
        "type_aliases": ["PRD"],
        "template": "prd.md",
        "stale_after_days": 180,
        "site": {"include": True},
    },
    "decision": {
        "enabled": True,
        "lifecycle": "immutable",
        "dir_patterns": ["docs/decisions/**"],
        "type_aliases": ["ADR"],
        "template": "decision.md",
        "site": {"include": True},
        "numbering": {"prefix": "ADR-", "digits": 3},
    },
    "plan": {
        "enabled": True,
        "lifecycle": "ephemeral",
        "dir_patterns": ["docs/plans/**"],
        "type_aliases": ["Plan"],
        "template": "plan.md",
        "site": {"include": True},
        "ttl_days": 45,
        "archive_dir": "docs/plans/archive",
    },
    "external": {
        "enabled": True,
        "lifecycle": "evergreen",
        "dir_patterns": [],
        "type_aliases": [],
        "template": "external.md",
        "stale_after_days": 365,
        "site": {"include": False},
    },
}


DEFAULT_CONFIG: dict[str, Any] = {
    "docsmith_version": 1,
    "engine_version": ENGINE_VERSION,
    "project": {
        "name": "",
        "repo_url": None,
        "default_branch": "main",
        "docs_dir": "docs",
        "subsystem_depth": 1,
    },
    "profile": "standard",
    "overrides": {},
    "categories": DEFAULT_CATEGORIES,
    "frontmatter": {
        "allowed_statuses": ["draft", "stable", "deprecated", "superseded"],
        "require_title": True,
        "require_stale_after": False,
    },
    # Intentionally empty: filled from the selected profile for every key
    # the project's config file does not set explicitly.
    "validate": {},
    "freshness": {
        "threshold": 70,
        "weights": {"age": 0.5, "ttl": 0.3, "drift": 0.2},
        "age_grace_days": 30,
    },
    "extra_gate_paths": ["CLAUDE.md", "AGENTS.md", "README.md", "docs/**/README.md"],
    "hook": {"enabled": True},
    "site": {
        "enabled": False,
        "workdir": ".docsmith/site",
        "name": "",
        "description": "",
        "site_author": "",
        "repo_name": "",
        "repo_url": "",
        "edit_branch": "main",
        "landing_title": None,
        "tarball_name": None,
        "copyright": "",
        "theme": {
            "primary": "blue",
            "accent": "blue",
            "font_text": "Roboto",
            "font_code": "Roboto Mono",
        },
        "plugins": {
            "mermaid": True,
            "swagger_ui": False,
            "git_revision_date": True,
            "minify": True,
        },
        "nav": {
            "style": "subsystem-first",
            "section_order": [],
            "subgroup_min_entries": 8,
        },
        "exclude": {
            "dir_basenames": [
                ".git",
                "node_modules",
                ".venv",
                "vendor",
                "dist",
                "build",
                "target",
                "autogen",
                "__pycache__",
            ],
            "dir_paths": [],
            "filenames": [],
        },
        "sources": [],
    },
    "okf_compat": {
        "allowed_types": [],
        "mapping_rules": [],
    },
}


def find_project_root(start: Path) -> Path:
    """Walk `start` and its parents; the first directory containing
    .docsmith/config.json is the project root.

    Raises ConfigNotFoundError if no candidate directory has the file.
    """
    start = Path(start).resolve()
    for candidate in [start, *start.parents]:
        if (candidate / CONFIG_REL_PATH).is_file():
            return candidate
    raise ConfigNotFoundError(
        f"no {CONFIG_REL_PATH} found in '{start}' or any parent directory "
        "(run 'docsmith scaffold' to create one)"
    )


def _deep_merge(base: dict, override: dict) -> dict:
    """Recursively merge `override` into `base` (override wins). Dicts merge
    key-by-key; every other value type (lists, scalars) is replaced whole."""
    merged = dict(base)
    for key, value in override.items():
        if isinstance(merged.get(key), dict) and isinstance(value, dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _apply_dotted_override(config: dict, dotted_key: str, value: Any) -> None:
    """Set config[a][b][c] = value for a dotted key "a.b.c", creating
    intermediate dicts as needed."""
    parts = dotted_key.split(".")
    node = config
    for part in parts[:-1]:
        child = node.get(part)
        if not isinstance(child, dict):
            child = {}
            node[part] = child
        node = child
    node[parts[-1]] = value


def load_config(
    start: Optional[Path] = None,
    project_root: Optional[Path] = None,
) -> tuple[Path, dict]:
    """Locate and load the effective docsmith configuration.

    Args:
        start: Directory to begin upward discovery from (defaults to cwd).
            Ignored when project_root is given.
        project_root: Explicit project root. Skips discovery, but the config
            file must still exist under it.

    Returns:
        (project_root, effective_config)

    Raises:
        ConfigNotFoundError: No config file could be located.
        ValueError: The config file is malformed, has an unknown profile, or
            declares an unsupported docsmith_version.
    """
    if project_root is not None:
        root = Path(project_root).resolve()
        config_path = root / CONFIG_REL_PATH
        if not config_path.is_file():
            raise ConfigNotFoundError(f"no {CONFIG_REL_PATH} found under '{root}'")
    else:
        root = find_project_root(start if start is not None else Path.cwd())
        config_path = root / CONFIG_REL_PATH

    try:
        with open(config_path, encoding="utf-8") as f:
            file_config = json.load(f)
    except json.JSONDecodeError as e:
        raise ValueError(f"config file '{config_path}' is not valid JSON: {e}") from e

    if not isinstance(file_config, dict):
        raise ValueError(f"config file '{config_path}' must contain a JSON object")

    # Step 2: deep-merge the file over engine defaults (file wins).
    merged = _deep_merge(copy.deepcopy(DEFAULT_CONFIG), file_config)

    if merged.get("docsmith_version") != 1:
        raise ValueError(
            f"unsupported docsmith_version {merged.get('docsmith_version')!r} "
            f"in '{config_path}' (this engine supports version 1)"
        )

    # Step 3: profile fills only the keys the file left unset.
    profile_name = merged.get("profile", "standard")
    if profile_name not in PROFILES:
        raise ValueError(
            f"unknown profile '{profile_name}' in '{config_path}' "
            f"(known profiles: {sorted(PROFILES)})"
        )
    for section, section_defaults in PROFILES[profile_name].items():
        section_config = merged.setdefault(section, {})
        for key, value in section_defaults.items():
            section_config.setdefault(key, value)

    # Step 4: dotted-path overrides apply LAST.
    overrides = merged.get("overrides") or {}
    for dotted_key, value in overrides.items():
        _apply_dotted_override(merged, dotted_key, value)

    return root, merged


def _pattern_matches(rel_path: str, pattern: str) -> bool:
    """fnmatch-based glob match on a posix-style relative path. fnmatch's `*`
    crosses `/` boundaries, which gives `**` recursive-match semantics."""
    return fnmatch.fnmatch(rel_path, pattern)


def resolve_category(rel_path: str, fm_type: Optional[str], config: dict) -> Optional[str]:
    """Resolve a document to a configured category.

    Resolution order:
        1. fm_type equals a category key -> that category.
        2. fm_type equals any category's type_aliases entry (case-sensitive)
           -> that category.
        3. First ENABLED category (dict order) with any dir_pattern matching
           rel_path -> that category.
        4. None.
    """
    categories: dict[str, dict] = config.get("categories", {})

    if fm_type is not None:
        if fm_type in categories:
            return fm_type
        for name, category in categories.items():
            if fm_type in category.get("type_aliases", []):
                return name

    posix_path = rel_path.replace("\\", "/")
    for name, category in categories.items():
        if not category.get("enabled", True):
            continue
        for pattern in category.get("dir_patterns", []):
            if _pattern_matches(posix_path, pattern):
                return name

    return None
