"""The `docsmith score` command (wave A2).

Scores the freshness of every ENABLED evergreen category doc from three
weighted components (weights from config freshness.weights):

    age (A)    How stale the doc is versus the code the docmap maps to it.
               Omitted (weights renormalize) when the doc has no docmap
               mapping or no mapped path has git history.
    ttl (T)    Days left before the doc's stale deadline (frontmatter
               stale_after, else last-doc-commit date + the category's
               stale_after_days), scaled to the category window.
    drift (Dr) Fraction of the doc's cited code refs that still resolve.

score = round(sum(w_i * c_i) / sum(w_i)) over the PRESENT components.
verdict = "fresh" when score >= threshold, else "stale"; any unreadable
doc scores 0 with verdict "error".

State caching (--update-state) persists per-doc results keyed by the git
last-commit timestamps of the doc (doc_commit) and its mapped code
(code_commit) plus the doc's stale_after value: when all three are
unchanged on a later run, the cached entry is reused without recompute.
"""

import datetime
import json
import sys
import time
from pathlib import Path
from typing import Any, Optional

from . import docmap as docmap_lib
from . import frontmatter_spec
from . import gitinfo
from . import refs as refs_lib
from .validate import category_docs

STATE_REL_PATH = Path(".docsmith") / "state.json"

_SECONDS_PER_DAY = 86400


def _clamp(low: float, high: float, value: float) -> float:
    return max(low, min(high, value))


def _load_docmap_mapping(root: Path) -> dict:
    """The docmap's "map" mapping, or {} when the file is missing/invalid
    (score treats an absent docmap as 'no doc is mapped', never an error)."""
    try:
        return docmap_lib.load_docmap(root)["map"]
    except (FileNotFoundError, ValueError, json.JSONDecodeError):
        return {}


def _mapped_keys(rel_path: str, mapping: dict) -> "list[str]":
    """C(D): the docmap keys whose entries contain this doc's rel path."""
    return [
        key
        for key in sorted(mapping)
        if any(
            entry["path"] == rel_path
            for entry in docmap_lib.normalize_entries(mapping[key])
        )
    ]


def _load_state(root: Path) -> dict:
    """Best-effort read of the existing state file ({} when absent/bad)."""
    try:
        with open(root / STATE_REL_PATH, encoding="utf-8") as f:
            state = json.load(f)
        return state if isinstance(state, dict) else {}
    except (OSError, json.JSONDecodeError):
        return {}


def _score_doc(
    root: Path,
    rel_path: str,
    category_config: dict,
    mapping: dict,
    freshness: dict,
    threshold: int,
    now: "datetime.datetime",
    prev_scores: dict,
) -> dict:
    """Compute (or reuse from cache) the full state entry for one doc."""
    weights: dict = freshness.get("weights", {"age": 0.5, "ttl": 0.3, "drift": 0.2})
    grace_days = freshness.get("age_grace_days", 30)

    text = (root / rel_path).read_text(encoding="utf-8")
    fm, _ = frontmatter_spec.parse_frontmatter(text)
    stale_date = (
        frontmatter_spec._parse_stale_after(fm.get("stale_after"))
        if fm and "stale_after" in fm
        else None
    )
    stale_after_key = stale_date.isoformat() if stale_date else None

    # doc_commit / code_commit are the git last-commit timestamps: they
    # change exactly when new commits touch the doc / its mapped code,
    # which is what the cache must invalidate on.
    doc_commit = gitinfo.last_commit_ts(root, rel_path)
    doc_ts = doc_commit if doc_commit is not None else now.timestamp()

    keys = _mapped_keys(rel_path, mapping)
    code_commit: Optional[int] = None
    if keys:
        code_stamps = [
            ts for ts in (gitinfo.last_commit_ts(root, key) for key in keys)
            if ts is not None
        ]
        if code_stamps:
            code_commit = max(code_stamps)

    cached = prev_scores.get(rel_path)
    if (
        isinstance(cached, dict)
        and doc_commit is not None  # never trust a cache entry for untracked docs
        and cached.get("doc_commit") == doc_commit
        and cached.get("code_commit") == code_commit
        and cached.get("stale_after") == stale_after_key
    ):
        return dict(cached)

    components: dict[str, float] = {}

    # Age: doc freshness relative to the mapped code's last change.
    if code_commit is not None:
        delta_days = max(0.0, (code_commit - doc_ts) / _SECONDS_PER_DAY)
        components["age"] = _clamp(0.0, 100.0, 100.0 * (1.0 - delta_days / grace_days))

    # TTL: days left before the stale deadline, scaled to the category window.
    stale_after_days = category_config.get("stale_after_days")
    if stale_after_days:
        doc_date = datetime.datetime.fromtimestamp(
            doc_ts, tz=datetime.timezone.utc
        ).date()
        deadline = (
            stale_date
            if stale_date is not None
            else doc_date + datetime.timedelta(days=stale_after_days)
        )
        days_left = (deadline - now.date()).days
        components["ttl"] = _clamp(0.0, 100.0, 100.0 * days_left / stale_after_days)

    # Drift: fraction of cited refs that still resolve.
    refs = refs_lib.extract(text)
    if not refs:
        components["drift"] = 100.0
    else:
        broken = sum(1 for ref in refs if refs_lib.resolve(ref, root) != "ok")
        components["drift"] = 100.0 * (1.0 - broken / len(refs))

    weight_total = sum(weights.get(name, 0.0) for name in components)
    weighted_sum = sum(weights.get(name, 0.0) * value for name, value in components.items())
    score = round(weighted_sum / weight_total) if weight_total else 0

    return {
        "score": score,
        "computed_at": now.isoformat(),
        "doc_commit": doc_commit,
        "code_commit": code_commit,
        "stale_after": stale_after_key,
        "components": {name: round(value, 2) for name, value in components.items()},
        "verdict": "fresh" if score >= threshold else "stale",
    }


def _error_entry(now: "datetime.datetime") -> dict:
    return {
        "score": 0,
        "computed_at": now.isoformat(),
        "doc_commit": None,
        "code_commit": None,
        "stale_after": None,
        "components": {},
        "verdict": "error",
    }


def _human_table(docs_out: "list[dict]") -> str:
    """Fixed-width table sorted by score ascending (already sorted)."""
    lines = ["SCORE  AGE  TTL  DRIFT  VERDICT  DOC"]
    for doc in docs_out:
        components = doc["components"]

        def cell(name: str) -> str:
            value = components.get(name)
            return "-" if value is None else str(round(value))

        lines.append(
            f"{doc['score']:>5}  {cell('age'):>3}  {cell('ttl'):>3}  "
            f"{cell('drift'):>5}  {doc['verdict']:<7}  {doc['path']}"
        )
    return "\n".join(lines)


def run(args, project_root: Path, config: dict) -> int:
    """Execute `docsmith score`. Returns the process exit code."""
    root = Path(project_root)
    categories: dict[str, dict] = config.get("categories", {})
    freshness: dict = config.get("freshness", {})
    threshold: int = (
        args.threshold if args.threshold is not None else freshness.get("threshold", 70)
    )
    now = datetime.datetime.now(datetime.timezone.utc)

    scored_set = [
        (rel_path, category)
        for rel_path, category in category_docs(root, config)
        if category is not None
        and categories.get(category, {}).get("enabled", True)
        and categories.get(category, {}).get("lifecycle") == "evergreen"
    ]

    if args.doc:
        requested = [Path(doc).as_posix() for doc in args.doc]
        scored_paths = {rel_path for rel_path, _ in scored_set}
        for doc in requested:
            if doc not in scored_paths:
                print(
                    f"docsmith: '{doc}' is not an evergreen category doc in "
                    "this project (nothing to score)",
                    file=sys.stderr,
                )
                return 2
        scored_set = [
            (rel_path, category)
            for rel_path, category in scored_set
            if rel_path in set(requested)
        ]

    mapping = _load_docmap_mapping(root)
    existing_state = _load_state(root)
    prev_scores = existing_state.get("scores", {})
    if not isinstance(prev_scores, dict):
        prev_scores = {}

    entries: dict[str, dict] = {}
    for rel_path, category in scored_set:
        try:
            entries[rel_path] = _score_doc(
                root,
                rel_path,
                categories.get(category, {}),
                mapping,
                freshness,
                threshold,
                now,
                prev_scores,
            )
        except Exception:  # unreadable doc / git failure -> verdict "error"
            entries[rel_path] = _error_entry(now)

    docs_out = sorted(
        (
            {
                "path": rel_path,
                "score": entry["score"],
                "components": entry["components"],
                "verdict": entry["verdict"],
            }
            for rel_path, entry in entries.items()
        ),
        key=lambda doc: (doc["score"], doc["path"]),
    )
    summary = {
        "scored": len(docs_out),
        "fresh": sum(1 for doc in docs_out if doc["verdict"] == "fresh"),
        "stale": sum(1 for doc in docs_out if doc["verdict"] == "stale"),
        "error": sum(1 for doc in docs_out if doc["verdict"] == "error"),
    }

    if args.update_state:
        merged_scores = dict(prev_scores)
        merged_scores.update(entries)
        state: dict[str, Any] = {
            "version": 1,
            "last_scan": {
                "git_head": gitinfo.head_sha(root),
                "timestamp": now.isoformat(),
            },
            "scores": merged_scores,
        }
        if "validate" in existing_state:
            state["validate"] = existing_state["validate"]
        state_path = root / STATE_REL_PATH
        state_path.parent.mkdir(parents=True, exist_ok=True)
        state_path.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")

    if args.json:
        payload = {
            "command": "score",
            "project_root": str(root),
            "generated_at": now.isoformat(),
            "threshold": threshold,
            "docs": docs_out,
            "summary": summary,
        }
        print(json.dumps(payload, indent=2))
    elif not args.quiet or args.report:
        print(_human_table(docs_out))

    if args.fail_under is not None:
        if any(
            doc["score"] < args.fail_under or doc["verdict"] == "error"
            for doc in docs_out
        ):
            return 1
    return 0
