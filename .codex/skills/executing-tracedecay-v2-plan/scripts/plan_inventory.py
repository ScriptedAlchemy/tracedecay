#!/usr/bin/env python3
"""Read-only inventory of TraceDecay V2 plan PR/task slices."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path


PR_VALUE = r"[0-9]+(?:\.[0-9]+)?(?:[A-Z][A-Z0-9-]*)?"
HEADING = re.compile(r"^(?P<marks>#{3,4})\s+(?P<heading>.*)$")
PR_DECLARATION = re.compile(
    r"^(?:(?:\d+(?:\.\d+)*\s+)|(?:Task\s+\d+[A-Z]?:\s+)|"
    r"(?:Companion requirements for\s+))?PR\s+"
)
PR_EXPRESSION = re.compile(
    rf"\bPR\s+(?P<start>{PR_VALUE})"
    rf"(?:(?:\s*[–-]\s*)(?P<end>{PR_VALUE})|"
    rf"(?P<slashes>(?:\s*/\s*{PR_VALUE})+))?"
)
PR_ID = re.compile(rf"\bPR\s+({PR_VALUE})\b")
CHECKBOX = re.compile(r"^\s*- \[([ xX])\]")
ORDERING = re.compile(r"(?:\*\*Ordering:\*\*|\b(?:after|depends on|blocked by|requires)\b)", re.IGNORECASE)
COMMIT = re.compile(r"(?:Commit:|Commit separately:).*?`([^`]+)`")
FM_ID = re.compile(r"\bFM-(\d{3})\b")
BASELINE_FM_BINDINGS = {
    "FM-161": ("PR 33R",),
    "FM-162": ("PR 21", "PR 19A", "PR 6A"),
    "FM-163": ("PR 24E0", "PR 33R", "PR 24E", "PR 37"),
    "FM-164": ("PR 6D", "PR 33S"),
    "FM-165": ("PR 33R", "PR 37"),
    "FM-166": ("PR 33R", "PR 33E", "PR 35I"),
    "FM-167": ("PR 24FR", "PR 22H", "PR 37"),
    "FM-169": ("PR 24Q", "PR 36R"),
    "FM-170": ("PR 24Q", "PR 36R"),
    "FM-171": ("PR 36R", "PR 37"),
}


def plan_files(root: Path) -> list[Path]:
    files = [root / "docs/plans/2026-07-09-tracedecay-brain-rewrite.md"]
    files.extend(sorted((root / "docs/plans/tracedecay-v2").glob("*.md")))
    return [path for path in files if path.is_file()]


def validate_failure_matrix(root: Path) -> list[str]:
    """Reject invalid FM IDs or missing PR references in active FM rows.

    This validates matrix row text only. The activated execution-graph export is
    the authority for slice ownership, dependency edges, and next-ready state.
    """
    path = root / "docs/plans/tracedecay-v2/14-historical-failure-regression-matrix.md"
    row_pairs = [
        (f"FM-{match.group(1)}", line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.startswith("| FM-") and (match := FM_ID.search(line))
    ]
    row_ids = [fm_id for fm_id, _ in row_pairs]
    duplicates = sorted({value for value in row_ids if row_ids.count(value) > 1})
    if duplicates:
        raise ValueError(f"duplicate failure-matrix IDs: {', '.join(duplicates)}")
    rows = dict(row_pairs)
    expected = [f"FM-{value:03d}" for value in range(1, len(row_ids) + 1)]
    if set(row_ids) != set(expected):
        missing = sorted(set(expected) - set(row_ids))
        detail = ", ".join(missing)
        raise ValueError(f"non-contiguous failure-matrix IDs; missing: {detail}")
    for fm_id, bindings in BASELINE_FM_BINDINGS.items():
        missing = [binding for binding in bindings if binding not in rows[fm_id]]
        if missing:
            raise ValueError(
                f"{fm_id} missing required PR reference(s): {', '.join(missing)}"
            )
    return expected


def _expand_range(start: str, end: str) -> list[str]:
    """Expand the numeric or final-component ranges used by the V2 plans."""
    if start.isdigit() and end.isdigit():
        first, last = int(start), int(end)
        return [str(value) for value in range(first, last + 1)] if first <= last else []

    left = re.fullmatch(r"(.+?)([A-Z])", start)
    right = re.fullmatch(r"(.+?)([A-Z])", end)
    if left and right and left.group(1) == right.group(1):
        first, last = ord(left.group(2)), ord(right.group(2))
        if first <= last:
            return [f"{left.group(1)}{chr(value)}" for value in range(first, last + 1)]

    left = re.fullmatch(r"(\d+[A-Z])(\d+)", start)
    right = re.fullmatch(r"(\d+[A-Z])(\d+)", end)
    if left and right and left.group(1) == right.group(1):
        first, last = int(left.group(2)), int(right.group(2))
        if first <= last:
            return [f"{left.group(1)}{value}" for value in range(first, last + 1)]
    return [start, end]


def heading_ids(heading: str) -> list[str]:
    """Return canonical IDs declared by a PR-bearing H3/H4 heading."""
    if not PR_DECLARATION.match(heading):
        return []

    ids: list[str] = []
    for match in PR_EXPRESSION.finditer(heading):
        start = match.group("start")
        if match.group("end"):
            values = _expand_range(start, match.group("end"))
        elif match.group("slashes"):
            values = [start, *re.findall(PR_VALUE, match.group("slashes"))]
        else:
            values = [start]
        ids.extend(f"PR {value}" for value in values)
    return list(dict.fromkeys(ids))


def scan(path: Path, root: Path) -> list[dict[str, object]]:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    starts: list[tuple[int, str, list[str]]] = []
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if match and (ids := heading_ids(match.group("heading"))):
            starts.append((index, match.group("heading"), ids))
    records: list[dict[str, object]] = []
    for ordinal, (start, heading, ids) in enumerate(starts):
        end = starts[ordinal + 1][0] if ordinal + 1 < len(starts) else len(lines)
        block = lines[start:end]
        references = sorted({f"PR {value}" for line in block for value in PR_ID.findall(line)} - set(ids))
        ordering = [line.strip() for line in block if ORDERING.search(line)][:20]
        commits = [match.group(1) for line in block if (match := COMMIT.search(line))]
        boxes = [match.group(1).lower() for line in block if (match := CHECKBOX.match(line))]
        records.append(
            {
                "ids": ids,
                "heading": heading,
                "path": str(path.relative_to(root)),
                "line": start + 1,
                "end_line": end,
                "referenced_prs": references,
                "ordering_evidence": ordering,
                "checkboxes": {"done": boxes.count("x"), "total": len(boxes)},
                "commit_subjects": commits,
                "block_sha256": hashlib.sha256("\n".join(block).encode()).hexdigest(),
            }
        )
    return records


def render_markdown(records: list[dict[str, object]]) -> str:
    output = ["| ID | Source | Checks | References |", "|---|---|---:|---|"]
    for record in records:
        ids = ", ".join(record["ids"]) or "—"
        checks = record["checkboxes"]
        refs = ", ".join(record["referenced_prs"]) or "—"
        output.append(
            f"| {ids} | `{record['path']}:{record['line']}` {record['heading']} | "
            f"{checks['done']}/{checks['total']} | {refs} |"
        )
    return "\n".join(output)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--id", help="Exact PR ID such as 'PR 4E'")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    validate_failure_matrix(root)
    records = [record for path in plan_files(root) for record in scan(path, root)]
    if args.id:
        records = [record for record in records if args.id in record["ids"]]
    records.sort(key=lambda record: (record["path"], record["line"]))
    if args.json:
        print(json.dumps({"records": records, "count": len(records)}, indent=2, sort_keys=True))
    else:
        print(render_markdown(records))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
