#!/usr/bin/env python3
"""Validate exact release asset coverage from the release target manifest."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def target_matrix(path: Path) -> list[dict[str, str]]:
    value = json.loads(path.read_text(encoding="utf-8"))
    targets = value.get("include")
    if not isinstance(targets, list) or not targets:
        raise SystemExit("release target manifest has no targets")
    names: set[str] = set()
    for target in targets:
        if not isinstance(target, dict):
            raise SystemExit("release target must be an object")
        required = ("name", "runner", "target", "archive")
        if any(not isinstance(target.get(field), str) or not target[field] for field in required):
            raise SystemExit("release target is missing required string fields")
        if target["archive"] not in {"tar.gz", "zip"}:
            raise SystemExit(f"unsupported release archive: {target['archive']}")
        if target["name"] in names:
            raise SystemExit(f"duplicate release target: {target['name']}")
        names.add(target["name"])
    return targets


def files(path: Path) -> set[str]:
    if not path.is_dir():
        raise SystemExit(f"release artifact directory is missing: {path}")
    result = {item.name for item in path.iterdir() if item.is_file() and item.stat().st_size}
    empty = sorted(item.name for item in path.iterdir() if item.is_file() and not item.stat().st_size)
    if empty:
        raise SystemExit("empty release artifacts: " + ", ".join(empty))
    return result


def require_exact(kind: str, actual: set[str], expected: set[str]) -> None:
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing or extra:
        details = []
        if missing:
            details.append("missing " + ", ".join(missing))
        if extra:
            details.append("unexpected " + ", ".join(extra))
        raise SystemExit(f"{kind} coverage mismatch: {'; '.join(details)}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--profile", choices=("stable", "beta"), default="stable")
    parser.add_argument("--mcpbs", type=Path)
    arguments = parser.parse_args()
    targets = target_matrix(arguments.manifest)
    binary_prefix = "tracedecay-beta" if arguments.profile == "beta" else "tracedecay"

    require_exact(
        "binary",
        files(arguments.binaries),
        {
            f"{binary_prefix}-{arguments.tag}-{target['name']}.{target['archive']}"
            for target in targets
        },
    )
    if arguments.mcpbs is None:
        raise SystemExit("release validation requires an MCPB directory")
    mcpb_prefix = (
        "tracedecay-beta" if arguments.profile == "beta" else "tracedecay"
    )
    require_exact(
        "MCPB",
        files(arguments.mcpbs),
        {
            f"{mcpb_prefix}-{arguments.tag}-{target['name']}.mcpb"
            for target in targets
        },
    )
    print("release artifact coverage matches target manifest")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
