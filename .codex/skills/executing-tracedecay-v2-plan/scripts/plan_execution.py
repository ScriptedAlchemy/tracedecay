#!/usr/bin/env python3
"""Validate a V2 execution-state export and render fail-closed next-ready packets."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

import execution_state
import live_evidence
import slice_authority


STATE_ENV = "TRACEDECAY_V2_EXECUTION_STATE"
DEFAULT_STATE = Path(".tracedecay/v2-execution-state.json")


def strict_json(path: Path) -> dict[str, Any]:
    """Load JSON while rejecting ambiguous duplicate object keys at every depth."""

    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON object key {key!r}")
            result[key] = value
        return result

    def invalid_constant(value: str) -> None:
        raise ValueError(f"non-finite JSON constant {value!r}")

    document = json.loads(
        path.read_bytes(), object_pairs_hook=unique_object, parse_constant=invalid_constant
    )
    if not isinstance(document, dict):
        raise ValueError("execution-state root must be a JSON object")
    return document


def candidate_commits(document: dict[str, Any]) -> list[str]:
    ledger = document.get("completion_ledger", {})
    entries = ledger.get("entries", []) if isinstance(ledger, dict) else []
    return sorted({
        candidate["commit"]
        for entry in entries if isinstance(entry, dict)
        for candidate in [entry.get("candidate")]
        if isinstance(candidate, dict)
        and isinstance(candidate.get("commit"), str)
        and execution_state.COMMIT.fullmatch(candidate["commit"])
    })


def analyze(document: dict[str, Any], live: live_evidence.LiveEvidence | None = None) -> dict[str, Any]:
    return execution_state.next_ready(execution_state.validate(document, live))


def resolve_state(root: Path, explicit: Path | None) -> Path:
    if explicit is not None:
        return explicit
    configured = os.environ.get(STATE_ENV)
    if configured:
        return Path(configured)
    default = root / DEFAULT_STATE
    active = root / ".tracedecay/v2-execution-active.json"
    if default.exists() and active.exists():
        raise ValueError(
            "ambiguous execution state: legacy direct state and active generation pointer both exist"
        )
    if default.exists():
        return default
    if active.exists():
        selected, failure = slice_authority.resolve_active_generation(active, root, "state")
        if failure is not None or selected is None:
            raise ValueError(f"active execution state: {failure.reason}: {failure.detail}")
        return selected
    return default


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate canonical V2 DAG/ledger evidence and select next-ready work."
    )
    parser.add_argument(
        "--graph", "--state", dest="state", type=Path,
        help=("tracedecay.v2.execution-state/v1 JSON export; defaults to "
              f"${STATE_ENV} then <repo-root>/{DEFAULT_STATE}"),
    )
    parser.add_argument(
        "--next-ready", action="store_true",
        help="accepted compatibility flag; validation always computes the sealed view",
    )
    parser.add_argument("--format", choices=("markdown", "json"), default="markdown")
    parser.add_argument(
        "--root", type=Path, required=True,
        help="authoritative current repository checkout root",
    )
    parser.add_argument(
        "--canonical-ref", default="HEAD",
        help="authoritative integration ref resolved in --root (default: HEAD)",
    )
    args = parser.parse_args()

    try:
        root = args.root.resolve()
        state = resolve_state(root, args.state)
        document = strict_json(state)
        live = live_evidence.inspect(root, args.canonical_ref, candidate_commits(document))
        view = analyze(document, live)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        view = {
            "schema": execution_state.VIEW_SCHEMA,
            "valid": False,
            "activation_mode": "invalid",
            "repository": None,
            "source_commit": None,
            "source_set_digest": None,
            "graph_revision": None,
            "graph_digest": None,
            "errors": [f"input: {error}"],
            "next_ready": [],
            "blocked": [],
            "execution_order": [],
        }

    if args.format == "json":
        print(json.dumps(view, indent=2, sort_keys=True))
    else:
        print(execution_state.markdown(view), end="")
    return 0 if view["valid"] else 2


if __name__ == "__main__":
    sys.exit(main())
