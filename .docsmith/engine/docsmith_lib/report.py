"""Result aggregation and rendering for docsmith commands.

A Report collects per-check findings and renders them either as JSON (for
--json / machine consumers) or as grouped human-readable text. exit_code()
implements the CLI contract: 1 on any error (or, with strict=True, on any
warning), 0 otherwise.
"""

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Union


class Report:
    """Accumulates findings for one docsmith command invocation."""

    def __init__(self, command: str, project_root: Union[Path, str]) -> None:
        self.command = command
        self.project_root = str(project_root)
        self.checked = 0
        self._results: list[dict] = []

    def add(
        self,
        check: str,
        severity: str,
        path: str,
        message: str,
        line: Optional[int] = None,
    ) -> None:
        """Record one finding. severity is "error" or "warning"."""
        self._results.append({
            "check": check,
            "severity": severity,
            "path": path,
            "line": line,
            "message": message,
        })

    @property
    def results(self) -> list[dict]:
        return list(self._results)

    @property
    def errors(self) -> int:
        return sum(1 for result in self._results if result["severity"] == "error")

    @property
    def warnings(self) -> int:
        return sum(1 for result in self._results if result["severity"] == "warning")

    def to_json(self) -> str:
        """Machine-readable JSON rendering of the full report."""
        payload = {
            "command": self.command,
            "project_root": self.project_root,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "summary": {
                "errors": self.errors,
                "warnings": self.warnings,
                "checked": self.checked,
            },
            "results": [dict(result) for result in self._results],
        }
        return json.dumps(payload, indent=2)

    def to_human(self) -> str:
        """Human-readable rendering: findings grouped per check, followed by
        a trailing 'FAILURES: n / WARNINGS: n' summary line."""
        lines: list[str] = []
        for check in dict.fromkeys(result["check"] for result in self._results):
            lines.append(f"{check}:")
            for result in self._results:
                if result["check"] != check:
                    continue
                tag = "[FAIL]" if result["severity"] == "error" else "[WARN]"
                location = result["path"]
                if result["line"] is not None:
                    location = f"{location}:{result['line']}"
                lines.append(f"  {tag} {location}: {result['message']}")
        lines.append(f"FAILURES: {self.errors} / WARNINGS: {self.warnings}")
        return "\n".join(lines)

    def exit_code(self, strict: bool = False) -> int:
        """1 when there are errors (or warnings under strict), else 0."""
        if self.errors:
            return 1
        if strict and self.warnings:
            return 1
        return 0
