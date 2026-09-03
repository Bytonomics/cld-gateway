"""Docmap loading and file-to-doc matching for docsmith.

The docmap file (`.docsmith/docmap.json`) maps source paths to the
documentation entries that must be reviewed when those paths change:

    {
      "version": 1,
      "updated_at": "<ISO-8601>",
      "map": {
        "src/":    [{"path": "docs/a.md", "reason": "...", "message": "..."}],
        "main.go": ["docs/b.md"]            <- legacy plain-string entry
      }
    }

Matching semantics (ported exactly from the smritea posttooluse hook's
find_matching_docs):
    - Keys ending in '/' are directory prefixes: they match any rel_path
      that starts with the key.
    - All other keys are exact file-path matches.
    - Every matching key contributes its entries; results are deduplicated
      by doc path, preserving first-seen order (the first entry's
      reason/message is kept).
"""

import json
from pathlib import Path
from typing import Any

DOCMAP_REL_PATH = Path(".docsmith") / "docmap.json"


def load_docmap(project_root: Path) -> dict:
    """Load and structurally validate the docmap envelope.

    Raises:
        FileNotFoundError: The docmap file does not exist.
        ValueError: The envelope's version is not 1, or "map" is not a dict.
    """
    docmap_path = Path(project_root) / DOCMAP_REL_PATH
    if not docmap_path.is_file():
        raise FileNotFoundError(f"no docmap file at '{docmap_path}'")

    with open(docmap_path, encoding="utf-8") as f:
        data = json.load(f)

    if not isinstance(data, dict) or data.get("version") != 1:
        raise ValueError(
            f"docmap '{docmap_path}' has unsupported version "
            f"{data.get('version') if isinstance(data, dict) else data!r} "
            "(this engine supports version 1)"
        )
    if not isinstance(data.get("map"), dict):
        raise ValueError(f"docmap '{docmap_path}' is missing a 'map' object")

    return data


def normalize_entries(value: Any) -> list[dict]:
    """Normalize a docmap value into a list of {path, reason, message} dicts.

    Accepts a list containing dicts ({path, reason?, message?}) and/or legacy
    plain strings (a bare doc path). Entries without a path are skipped.
    Anything that is not a list normalizes to [].
    """
    if not isinstance(value, list):
        return []

    normalized: list[dict] = []
    for entry in value:
        if isinstance(entry, str):
            if entry:
                normalized.append({"path": entry, "reason": "", "message": ""})
        elif isinstance(entry, dict):
            path = entry.get("path")
            if path:
                normalized.append({
                    "path": path,
                    "reason": entry.get("reason", ""),
                    "message": entry.get("message", ""),
                })
    return normalized


def find_matching_docs(rel_path: str, doc_map: dict) -> list[dict]:
    """Find all doc entries that should be reviewed when rel_path changes.

    Args:
        rel_path: File path relative to the project root.
        doc_map: The docmap's "map" mapping (source path -> entries). A full
            {"version", "map"} envelope is also accepted for convenience.

    Returns:
        Normalized entries from every matching key, deduplicated by doc path
        preserving first-seen order (keys are iterated in sorted order).
    """
    mapping = doc_map
    if isinstance(doc_map.get("map"), dict):
        mapping = doc_map["map"]

    matched_entries: list[dict] = []
    for key in sorted(mapping.keys()):
        if key.endswith("/"):
            if rel_path.startswith(key):
                matched_entries.extend(normalize_entries(mapping[key]))
        elif rel_path == key:
            matched_entries.extend(normalize_entries(mapping[key]))

    deduped: list[dict] = []
    seen_paths: set[str] = set()
    for entry in matched_entries:
        if entry["path"] not in seen_paths:
            deduped.append(entry)
            seen_paths.add(entry["path"])
    return deduped


def migrate_legacy(legacy: dict, now_iso: str) -> dict:
    """Wrap a legacy bare mapping into the version-1 docmap envelope.

    Drops metadata keys starting with "_" (e.g. "_comment") and normalizes
    every value via normalize_entries.
    """
    return {
        "version": 1,
        "updated_at": now_iso,
        "map": {
            key: normalize_entries(value)
            for key, value in legacy.items()
            if not key.startswith("_")
        },
    }
