#!/usr/bin/env python3
"""Fail closed when a clone-envelope receipt does not name the measured tip.

Usage:
  scripts/check-clone-envelope-receipt.py \\
    --receipt benchmark_data/index-bench/clone-envelope-20260915.json \\
    --head HEAD

Prints the receipt harness_commit and target_results. Exit 0 only when
harness_commit equals the resolved HEAD and every target_results value is
true. Does not invent measurements.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def resolve_head(head: str) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", head],
        check=True,
        capture_output=True,
        text=True,
    )
    return completed.stdout.strip()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--head", default="HEAD")
    args = parser.parse_args()

    receipt = json.loads(args.receipt.read_text(encoding="utf-8"))
    harness = receipt.get("harness_commit")
    targets = receipt.get("target_results") or {}
    head = resolve_head(args.head)

    print(f"head={head}")
    print(f"harness_commit={harness}")
    print(f"target_results={json.dumps(targets, sort_keys=True)}")

    if not isinstance(harness, str) or not harness:
        print("receipt missing harness_commit", file=sys.stderr)
        return 2
    if harness != head:
        print(
            "harness_commit does not match measured HEAD; tip is not qualified",
            file=sys.stderr,
        )
        return 1
    if not isinstance(targets, dict) or not targets:
        print("receipt missing target_results", file=sys.stderr)
        return 2
    failed = [name for name, ok in targets.items() if ok is not True]
    if failed:
        print(f"failing targets: {', '.join(failed)}", file=sys.stderr)
        return 1
    print("receipt qualifies measured HEAD")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
