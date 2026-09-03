"""YAML frontmatter parsing and field validation for docsmith.

Field rules enforced by check_fields (severity in {"error", "warning"}):
    - category unresolvable            -> error
    - title missing/empty              -> error when frontmatter.require_title
                                          is true, warning otherwise
    - status not in allowed_statuses   -> error (only when present)
    - tags not a list                  -> error (only when present)
    - stale_after not YYYY-MM-DD       -> error (only when present)
    - stale_after in the past          -> warning
    - generated malformed              -> error (must be a dict with a str
                                          'by' and an ISO-8601 'at')
"""

import datetime
from typing import Optional

import yaml


def parse_frontmatter(text: str) -> tuple[Optional[dict], Optional[str]]:
    """Parse the YAML frontmatter block at the very top of a markdown file.

    The block must start at line 1 with '---' and end at the next '---' line.

    Returns:
        (frontmatter_dict, None) on success, or (None, error_message) when
        the block is missing, unclosed, unparseable, or not a mapping.
    """
    lines = text.split("\n")
    if not lines or lines[0].strip() != "---":
        return None, "no frontmatter block"

    raw_frontmatter: Optional[str] = None
    for i in range(1, len(lines)):
        if lines[i].strip() == "---":
            raw_frontmatter = "\n".join(lines[1:i])
            break
    if raw_frontmatter is None:
        return None, "no closing '---' frontmatter delimiter"

    try:
        frontmatter = yaml.safe_load(raw_frontmatter)
    except yaml.YAMLError as e:
        return None, f"frontmatter YAML failed to parse: {e}"

    if not isinstance(frontmatter, dict):
        return None, "frontmatter did not parse to a YAML mapping (dict)"

    return frontmatter, None


def _parse_stale_after(value: object) -> Optional[datetime.date]:
    """Parse a stale_after value into a date, or None if malformed.

    PyYAML auto-parses unquoted YYYY-MM-DD scalars into datetime.date, so
    both date objects and strings are accepted.
    """
    if isinstance(value, datetime.datetime):
        return value.date()
    if isinstance(value, datetime.date):
        return value
    if isinstance(value, str):
        try:
            return datetime.date.fromisoformat(value)
        except ValueError:
            return None
    return None


def _generated_is_valid(generated: object) -> bool:
    """A 'generated' block must be a dict with a non-empty str 'by' and an
    ISO-8601-parseable 'at'."""
    if not isinstance(generated, dict):
        return False
    by = generated.get("by")
    if not isinstance(by, str) or not by.strip():
        return False
    at = generated.get("at")
    if isinstance(at, (datetime.date, datetime.datetime)):
        # PyYAML auto-parses unquoted timestamps.
        return True
    if isinstance(at, str):
        try:
            datetime.datetime.fromisoformat(at)
            return True
        except ValueError:
            return False
    return False


def check_fields(fm: dict, config: dict, category: Optional[str]) -> list[tuple[str, str]]:
    """Validate frontmatter fields against the effective config.

    Args:
        fm: Parsed frontmatter mapping.
        config: Effective docsmith config (uses the "frontmatter" section).
        category: The already-resolved category for this doc, or None when
            the caller could not resolve one.

    Returns:
        List of (severity, message) findings; severity is "error" or "warning".
    """
    findings: list[tuple[str, str]] = []
    frontmatter_config = config.get("frontmatter", {})

    if category is None:
        findings.append((
            "error",
            f"type '{fm.get('type')}' does not resolve to any category",
        ))

    title = fm.get("title")
    if not (isinstance(title, str) and title.strip()):
        severity = "error" if frontmatter_config.get("require_title", True) else "warning"
        findings.append((severity, "frontmatter is missing a non-empty 'title' key"))

    if "status" in fm:
        allowed_statuses = frontmatter_config.get("allowed_statuses", [])
        status = fm.get("status")
        if not (isinstance(status, str) and status in allowed_statuses):
            findings.append((
                "error",
                f"'status: {status}' is not an allowed value "
                f"(allowed: {allowed_statuses})",
            ))

    if "tags" in fm and not isinstance(fm.get("tags"), list):
        findings.append((
            "error",
            f"'tags' must be a list, got {type(fm.get('tags')).__name__}",
        ))

    if "stale_after" in fm:
        stale_after = fm.get("stale_after")
        parsed_date = _parse_stale_after(stale_after)
        if parsed_date is None:
            findings.append((
                "error",
                f"'stale_after: {stale_after}' is not a valid YYYY-MM-DD date",
            ))
        elif parsed_date < datetime.date.today():
            findings.append((
                "warning",
                f"document is stale: 'stale_after: {parsed_date.isoformat()}' has passed",
            ))

    if "generated" in fm and not _generated_is_valid(fm.get("generated")):
        findings.append((
            "error",
            "'generated' must be a dict with a str 'by' and an ISO-8601 'at'",
        ))

    return findings


def has_index_table(text: str) -> bool:
    """True when a markdown table appears within 40 lines after the first
    ATX H1 ('# ') heading. A table line starts with '|' and contains at
    least one more '|'."""
    lines = text.split("\n")
    for i, line in enumerate(lines):
        if line.startswith("# "):
            for candidate in lines[i + 1 : i + 1 + 40]:
                stripped = candidate.lstrip()
                if stripped.startswith("|") and stripped.count("|") >= 2:
                    return True
            return False
    return False
