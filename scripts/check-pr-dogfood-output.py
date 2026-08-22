#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


TRANSIENT_GRAPH_REASONS = frozenset({"code-graph-unavailable", "code-graph-stale"})


def require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{label} output must be a JSON object")
    return value


def validate_status(value: dict[str, Any]) -> None:
    if not value:
        raise ValueError("status output must not be empty")


def validate_context(value: dict[str, Any]) -> None:
    coverage = value.get("coverage")
    if not isinstance(coverage, dict) or not coverage:
        raise ValueError("context must return typed retrieval coverage")


def validate_pr_context(
    value: dict[str, Any],
    *,
    expected_base_oid: str,
    expected_head_oid: str,
    expected_merge_base: str,
) -> None:
    expected_oids = {
        "base_oid": expected_base_oid,
        "head_oid": expected_head_oid,
        "merge_base": expected_merge_base,
    }
    for key, expected in expected_oids.items():
        if value.get(key) != expected:
            raise ValueError(f"pr_context {key} does not match the requested Git commit")

    changes = value.get("changes")
    if not isinstance(changes, list) or not changes:
        raise ValueError("pr_context must return changed-file evidence")
    if value.get("files_changed") != len(changes):
        raise ValueError("pr_context changed-file count must match its evidence")

    coverage = value.get("analysis_coverage")
    if not isinstance(coverage, dict) or not isinstance(coverage.get("complete"), bool):
        raise ValueError("pr_context must return typed analysis coverage")

    graph = value.get("verified_graph_evidence")
    if graph is None:
        if value.get("status") == "partial":
            raise ValueError("partial pr_context requires typed graph evidence")
        return
    if value.get("status") != "partial":
        raise ValueError("unavailable graph evidence requires partial pr_context status")
    if coverage["complete"] is not False:
        raise ValueError("partial pr_context analysis coverage must remain incomplete")
    if not isinstance(graph, dict) or graph.get("status") != "unavailable":
        raise ValueError("partial pr_context graph evidence must be typed unavailable")
    if graph.get("reason_code") not in TRANSIENT_GRAPH_REASONS:
        raise ValueError("partial pr_context requires an explicitly transient graph reason")
    if graph.get("retryable") is not True:
        raise ValueError("partial pr_context graph evidence must remain retryable")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", choices=("status", "context", "pr_context"), required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--base-oid")
    parser.add_argument("--head-oid")
    parser.add_argument("--merge-base")
    args = parser.parse_args()

    value = require_object(json.loads(args.input.read_text(encoding="utf-8")), args.kind)
    if args.kind == "status":
        validate_status(value)
    elif args.kind == "context":
        validate_context(value)
    else:
        if not all((args.base_oid, args.head_oid, args.merge_base)):
            parser.error("pr_context validation requires --base-oid, --head-oid, and --merge-base")
        validate_pr_context(
            value,
            expected_base_oid=args.base_oid,
            expected_head_oid=args.head_oid,
            expected_merge_base=args.merge_base,
        )
    print(f"TraceDecay PR dogfood {args.kind} output is valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
