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


def require_whole_targets(
    targets: list[dict[str, str]],
    binaries: set[str],
    mcpbs: set[str],
    binary_name: dict[str, str],
    mcpb_name: dict[str, str],
) -> list[str]:
    """Partial coverage: every present target is whole, nothing is foreign.

    A publish job that lost one build target still ships the others; the
    recovery planner rebuilds only what is missing. What it must never ship
    is half a target (an archive without its MCPB or the reverse) or a file
    the manifest does not name.
    """
    extra = sorted(binaries - set(binary_name.values())) + sorted(
        mcpbs - set(mcpb_name.values())
    )
    if extra:
        raise SystemExit("unexpected release artifacts: " + ", ".join(extra))
    missing = []
    for target in targets:
        has_binary = binary_name[target["name"]] in binaries
        has_mcpb = mcpb_name[target["name"]] in mcpbs
        if has_binary != has_mcpb:
            raise SystemExit(
                f"release target {target['name']} is half built: "
                f"archive={'present' if has_binary else 'missing'}, "
                f"mcpb={'present' if has_mcpb else 'missing'}"
            )
        if not has_binary:
            missing.append(target["name"])
    if len(missing) == len(targets):
        raise SystemExit("no release target is complete; nothing to publish")
    return missing


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--profile", choices=("stable", "beta"), default="stable")
    parser.add_argument("--mcpbs", type=Path)
    parser.add_argument(
        "--allow-missing-targets",
        action="store_true",
        help="accept a subset of targets as long as each present target is whole",
    )
    arguments = parser.parse_args()
    targets = target_matrix(arguments.manifest)
    prefix = "tracedecay-beta" if arguments.profile == "beta" else "tracedecay"
    binary_name = {
        target["name"]: f"{prefix}-{arguments.tag}-{target['name']}.{target['archive']}"
        for target in targets
    }
    mcpb_name = {
        target["name"]: f"{prefix}-{arguments.tag}-{target['name']}.mcpb"
        for target in targets
    }
    if arguments.mcpbs is None:
        raise SystemExit("release validation requires an MCPB directory")
    binaries = files(arguments.binaries)
    mcpbs = files(arguments.mcpbs)

    if arguments.allow_missing_targets:
        missing = require_whole_targets(targets, binaries, mcpbs, binary_name, mcpb_name)
        if missing:
            print(
                "release artifact coverage is partial; missing targets: "
                + ", ".join(missing)
            )
        else:
            print("release artifact coverage matches target manifest")
        return 0

    require_exact("binary", binaries, set(binary_name.values()))
    require_exact("MCPB", mcpbs, set(mcpb_name.values()))
    print("release artifact coverage matches target manifest")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
