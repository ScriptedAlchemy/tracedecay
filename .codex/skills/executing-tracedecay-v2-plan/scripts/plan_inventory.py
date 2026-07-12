#!/usr/bin/env python3
"""Read-only inventory of TraceDecay V2 plan PR/task slices."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path


PR_VALUE = r"[0-9]+(?:[A-Z][A-Z0-9-]*)?"
HEADING = re.compile(
    rf"^(?P<marks>#{{3,4}})\s+"
    rf"(?P<heading>(?:(?:Task\s+\d+[A-Z]?:\s+)|(?:Companion requirements for\s+))?"
    rf"PR\s+(?P<ids>{PR_VALUE}(?:\s*/\s*{PR_VALUE})*)"
    rf"(?:\s*[—:-].*)?)$"
)
PR_ID = re.compile(r"\bPR\s+([0-9]+[A-Z][A-Z0-9-]*|[0-9]+)\b")
CHECKBOX = re.compile(r"^\s*- \[([ xX])\]")
ORDERING = re.compile(r"(?:\*\*Ordering:\*\*|\b(?:after|depends on|blocked by|requires)\b)", re.IGNORECASE)
COMMIT = re.compile(r"(?:Commit:|Commit separately:).*?`([^`]+)`")


def plan_files(root: Path) -> list[Path]:
    files = [root / "docs/plans/2026-07-09-tracedecay-brain-rewrite.md"]
    files.extend(sorted((root / "docs/plans/tracedecay-v2").glob("*.md")))
    return [path for path in files if path.is_file()]


def scan(path: Path, root: Path) -> list[dict[str, object]]:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    starts: list[tuple[int, str, str]] = []
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if match:
            starts.append((index, match.group("heading"), match.group("ids")))
    records: list[dict[str, object]] = []
    for ordinal, (start, heading, heading_ids) in enumerate(starts):
        end = starts[ordinal + 1][0] if ordinal + 1 < len(starts) else len(lines)
        block = lines[start:end]
        ids = [f"PR {value.strip()}" for value in heading_ids.split("/")]
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
