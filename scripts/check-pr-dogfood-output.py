#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any


TRANSIENT_GRAPH_REASONS = frozenset({"code-graph-unavailable", "code-graph-stale"})
PR_CONTEXT_MAX_SYMBOLS = 500


def require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{label} output must be a JSON object")
    return value


def require_nonnegative_integer(value: dict[str, Any], key: str, label: str) -> int:
    result = value.get(key)
    if isinstance(result, bool) or not isinstance(result, int) or result < 0:
        raise ValueError(f"{label} {key} must be a non-negative integer")
    return result


def validate_status(value: dict[str, Any], *, strict: bool = False) -> None:
    if not value:
        raise ValueError("status output must not be empty")
    if not strict:
        return

    # Report every unmet condition, not just the first. Readiness has five
    # independent gates across two subsystems (text artifact, code graph), and
    # a first-failure-only message names whichever gate happens to be checked
    # first -- so a run blocked in graph activation still reported "requires
    # current code-index freshness" and hid the phase that actually stalled.
    unmet: list[str] = []

    freshness = value.get("code_index_freshness")
    if not isinstance(freshness, dict):
        freshness = {}
        unmet.append("strict status requires typed code-index freshness")
    elif freshness.get("status") != "current":
        observed = freshness.get("status", "absent")
        unmet.append(
            "strict status requires current code-index freshness; "
            f"status={observed}"
        )
    worktree = freshness.get("worktree")
    if not isinstance(worktree, dict):
        worktree = {}
        unmet.append("strict status requires typed worktree freshness")
    else:
        progress = worktree.get("progress")
        phase = progress.get("phase") if isinstance(progress, dict) else None
        if worktree.get("coverage") != "complete":
            unmet.append(
                "strict status requires complete text-index coverage; "
                f"coverage={worktree.get('coverage', 'absent')} phase={phase}"
            )
        if worktree.get("staleness_state") != "fresh":
            unmet.append(
                "strict status requires a fresh text-index generation; "
                f"staleness_state={worktree.get('staleness_state', 'absent')} "
                f"phase={phase}"
            )
        if not worktree.get("latest_generation_id"):
            unmet.append("strict status requires a current text-index generation")

    graph = value.get("graph_statistics")
    if not isinstance(graph, dict):
        unmet.append("strict status requires typed graph statistics")
    elif graph.get("state") != "observed":
        reason = graph.get("reason", "unknown")
        unmet.append(f"strict status requires an observed graph; reason={reason}")
    graph_serving = worktree.get("code_graph_serving")
    if not isinstance(graph_serving, dict) or graph_serving.get("state") != "ready":
        state = (
            graph_serving.get("state", "absent")
            if isinstance(graph_serving, dict)
            else "absent"
        )
        unmet.append(
            "strict status requires a ready code-graph serving projection; "
            f"state={state}"
        )

    if unmet:
        raise ValueError(" | ".join(unmet))


def validate_context(value: dict[str, Any], *, strict: bool = False) -> None:
    coverage = value.get("coverage")
    if not isinstance(coverage, dict) or not coverage:
        raise ValueError("context must return typed retrieval coverage")
    if not strict:
        return

    if coverage.get("lexical") != "complete":
        raise ValueError("strict context requires complete lexical coverage")
    if coverage.get("graph") != "complete":
        raise ValueError("strict context requires complete graph symbol evidence coverage")
    if not isinstance(value.get("search_matches"), list) or not value["search_matches"]:
        raise ValueError("strict context requires lexical search evidence")
    if not isinstance(value.get("symbols"), list) or not value["symbols"]:
        raise ValueError("strict context requires graph symbol evidence")
    if value.get("verified_graph_evidence") is not None:
        raise ValueError("strict context cannot contain unavailable verified graph evidence")


def validate_pr_context(
    value: dict[str, Any],
    *,
    expected_base_oid: str,
    expected_head_oid: str,
    expected_merge_base: str,
    strict: bool = False,
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
    if strict:
        if value.get("status") == "partial" or graph is not None:
            raise ValueError("strict pr_context rejects unavailable graph evidence")

        graph_generation = value.get("graph_generation")
        if not isinstance(graph_generation, str) or not graph_generation.strip():
            raise ValueError("strict pr_context requires a valid graph generation")

        symbol_page = value.get("symbol_page")
        if not isinstance(symbol_page, dict):
            raise ValueError("strict pr_context requires typed symbol-page metadata")
        limit = require_nonnegative_integer(symbol_page, "limit", "symbol_page")
        returned = require_nonnegative_integer(symbol_page, "returned", "symbol_page")
        if limit < 1 or limit > PR_CONTEXT_MAX_SYMBOLS or returned > limit:
            raise ValueError("strict pr_context requires a valid bounded symbol page")
        has_more = symbol_page.get("has_more")
        page_complete = symbol_page.get("complete")
        continuation_available = symbol_page.get("continuation_available")
        if not all(
            isinstance(item, bool)
            for item in (has_more, page_complete, continuation_available)
        ):
            raise ValueError("strict pr_context requires typed symbol-page state")
        if symbol_page.get("selection") != "stable_prefix":
            raise ValueError("strict pr_context requires stable-prefix symbol selection")
        if page_complete != (not has_more) or continuation_available != has_more:
            raise ValueError("strict pr_context symbol-page state is inconsistent")
        next_cursor = value.get("next_cursor")
        if has_more and (not isinstance(next_cursor, str) or not next_cursor):
            raise ValueError("strict pr_context bounded symbol page requires a cursor")
        if not has_more and next_cursor is not None:
            raise ValueError("strict pr_context complete symbol page cannot have a cursor")

        integer_coverage_fields = (
            "seed_symbols_analyzed",
            "symbols_returned",
            "impact_nodes_admitted",
            "impact_nodes_returned",
            "direct_call_edges_admitted",
            "impact_bytes_admitted",
        )
        coverage_counts = {
            key: require_nonnegative_integer(coverage, key, "analysis_coverage")
            for key in integer_coverage_fields
        }
        symbols_complete = coverage.get("symbols_complete")
        impact_partial = coverage.get("impact_partial")
        if not isinstance(symbols_complete, bool) or not isinstance(impact_partial, bool):
            raise ValueError("strict pr_context requires typed bounded analysis coverage")
        if (
            coverage_counts["seed_symbols_analyzed"] != returned
            or coverage_counts["symbols_returned"] != returned
            or symbols_complete != page_complete
            or coverage["complete"] != (symbols_complete and not impact_partial)
        ):
            raise ValueError("strict pr_context bounded analysis coverage is inconsistent")
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


def run() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", choices=("status", "context", "pr_context"), required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--base-oid")
    parser.add_argument("--head-oid")
    parser.add_argument("--merge-base")
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()

    value = require_object(json.loads(args.input.read_text(encoding="utf-8")), args.kind)
    if args.kind == "status":
        validate_status(value, strict=args.strict)
    elif args.kind == "context":
        validate_context(value, strict=args.strict)
    else:
        if not all((args.base_oid, args.head_oid, args.merge_base)):
            parser.error("pr_context validation requires --base-oid, --head-oid, and --merge-base")
        validate_pr_context(
            value,
            expected_base_oid=args.base_oid,
            expected_head_oid=args.head_oid,
            expected_merge_base=args.merge_base,
            strict=args.strict,
        )
    print(f"TraceDecay PR dogfood {args.kind} output is valid")
    return 0


def main() -> int:
    try:
        return run()
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        print(f"error: TraceDecay PR dogfood validation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
