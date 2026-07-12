#!/usr/bin/env python3
"""Validate a V2 execution-graph export and select fail-closed next-ready slices."""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

import plan_inventory

TERMINAL = {"integrated", "superseded"}
KNOWN = TERMINAL | {
    "not_started", "active", "changes_requested", "implemented_unreviewed",
    "approved_unintegrated", "blocked_unknown",
}


def inventory(root: Path) -> dict[str, list[dict[str, Any]]]:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for path in plan_inventory.plan_files(root):
        for record in plan_inventory.scan(path, root):
            for slice_id in record["ids"]:
                grouped[slice_id].append(record)
    return dict(sorted(grouped.items()))


def validate(root: Path, graph: dict[str, Any]) -> tuple[list[str], dict[str, dict[str, Any]]]:
    errors: list[str] = []
    sources = inventory(root)
    entries = graph.get("slices")
    if not isinstance(entries, list):
        return ["graph.slices must be an array"], {}

    by_id: dict[str, dict[str, Any]] = {}
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("id"), str):
            errors.append("every graph slice requires a string id")
            continue
        slice_id = entry["id"]
        if slice_id in by_id:
            errors.append(f"duplicate graph slice: {slice_id}")
        by_id[slice_id] = entry

    for missing in sorted(set(sources) - set(by_id)):
        errors.append(f"missing graph slice: {missing}")
    for unknown in sorted(set(by_id) - set(sources)):
        errors.append(f"unknown graph slice: {unknown}")

    for slice_id, entry in sorted(by_id.items()):
        status = entry.get("status")
        if status not in KNOWN:
            errors.append(f"{slice_id}: invalid status {status!r}")
        prerequisites = entry.get("prerequisites")
        if not isinstance(prerequisites, list) or not all(isinstance(value, str) for value in prerequisites):
            errors.append(f"{slice_id}: prerequisites must be a string array")
            prerequisites = []
        for prerequisite in prerequisites:
            if prerequisite == slice_id:
                errors.append(f"{slice_id}: self dependency")
            elif prerequisite not in by_id:
                errors.append(f"{slice_id}: unknown prerequisite {prerequisite}")

        authority = entry.get("authority")
        source_hashes = entry.get("source_hashes")
        known_sources = sources.get(slice_id, [])
        matching = [record for record in known_sources if record["path"] == authority]
        if len(matching) != 1:
            errors.append(f"{slice_id}: authority must name exactly one declaring plan source")
        expected = {record["path"]: record["block_sha256"] for record in known_sources}
        if not isinstance(source_hashes, dict) or source_hashes != expected:
            errors.append(f"{slice_id}: stale or incomplete source_hashes")

        receipts = entry.get("receipts")
        if status in TERMINAL and not isinstance(receipts, dict):
            errors.append(f"{slice_id}: terminal status requires receipts")
        if status == "integrated":
            required = {"implementation_commit", "review_verdict", "test_receipts", "integration_commit"}
            if not isinstance(receipts, dict) or not required.issubset(receipts):
                errors.append(f"{slice_id}: integrated status lacks required receipts")

    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(slice_id: str, trail: list[str]) -> None:
        if slice_id in visiting:
            errors.append("dependency cycle: " + " -> ".join([*trail, slice_id]))
            return
        if slice_id in visited or slice_id not in by_id:
            return
        visiting.add(slice_id)
        for parent in by_id[slice_id].get("prerequisites", []):
            visit(parent, [*trail, slice_id])
        visiting.remove(slice_id)
        visited.add(slice_id)

    for slice_id in sorted(by_id):
        visit(slice_id, [])
    return errors, by_id


def next_ready(by_id: dict[str, dict[str, Any]]) -> list[str]:
    ready = []
    for slice_id, entry in sorted(by_id.items()):
        if entry["status"] != "not_started":
            continue
        if all(by_id[parent]["status"] in TERMINAL for parent in entry["prerequisites"]):
            ready.append(slice_id)
    return ready


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--graph", type=Path, required=True)
    parser.add_argument("--next-ready", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    graph = json.loads(args.graph.read_text(encoding="utf-8"))
    errors, by_id = validate(root, graph)
    if errors:
        print(json.dumps({"valid": False, "errors": errors}, indent=2, sort_keys=True))
        return 2
    result: dict[str, Any] = {"valid": True, "slice_count": len(by_id)}
    if args.next_ready:
        result["next_ready"] = next_ready(by_id)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
