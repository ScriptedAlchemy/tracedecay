"""Stable CI artifacts for the catalog-driven MCP tool sweep."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any
import xml.etree.ElementTree as ET


PASSING_VERDICTS = frozenset({"PASS"})


def write_reports(out: Path, report: dict[str, Any]) -> None:
    """Write compact machine-readable and JUnit reports even after failures."""
    out.mkdir(parents=True, exist_ok=True)
    (out / "results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    rows = report.get("tools", [])
    if not isinstance(rows, list):
        rows = []
    cancellations = [
        {"tool": _text(row, "name", "unnamed-tool"), **event}
        for row in rows
        if isinstance(row, dict)
        for event in row.get("cancellation", [])
        if isinstance(event, dict)
    ]
    (out / "cancellations.json").write_text(json.dumps(cancellations, indent=2, sort_keys=True) + "\n")
    failures = [row for row in rows if not _passes(row)]
    fatal = report.get("fatal")
    has_fatal = isinstance(fatal, str) and bool(fatal)
    suite = ET.Element(
        "testsuite",
        {
            "name": "tracedecay.mcp_tool_sweep",
            "tests": str(len(rows) + int(has_fatal)),
            "failures": str(len(failures) + int(has_fatal)),
            "errors": "0",
        },
    )
    for row in rows:
        name = _text(row, "name", "unnamed-tool")
        elapsed = _number(row.get("max_ms")) / 1000
        case = ET.SubElement(suite, "testcase", {"name": name, "time": f"{elapsed:.3f}"})
        if not _passes(row):
            verdict = _text(row, "verdict", "FAIL")
            failure = ET.SubElement(case, "failure", {"type": verdict, "message": _note(row)})
            failure.text = _note(row)
    if has_fatal:
        case = ET.SubElement(suite, "testcase", {"name": "catalog-completion", "time": "0.000"})
        failure = ET.SubElement(case, "failure", {"type": "FATAL", "message": fatal})
        failure.text = fatal
    ET.ElementTree(suite).write(out / "junit.xml", encoding="utf-8", xml_declaration=True)


def _passes(row: Any) -> bool:
    return isinstance(row, dict) and row.get("verdict") in PASSING_VERDICTS


def _text(row: Any, key: str, fallback: str) -> str:
    value = row.get(key) if isinstance(row, dict) else None
    return value if isinstance(value, str) and value else fallback


def _number(value: Any) -> float:
    return float(value) if isinstance(value, (int, float)) and value >= 0 else 0.0


def _note(row: Any) -> str:
    note = row.get("note") if isinstance(row, dict) else None
    return note if isinstance(note, str) and note else "tool sweep failure"
