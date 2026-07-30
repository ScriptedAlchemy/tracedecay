#!/usr/bin/env python3
"""Prove default and explicit production Cargo profiles resolve identically."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import tomllib


def metadata(repo: Path, *arguments: str) -> dict[str, object]:
    completed = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--locked",
            *arguments,
        ],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def resolved_features(value: dict[str, object]) -> dict[str, set[str]]:
    resolve = value.get("resolve")
    if not isinstance(resolve, dict):
        raise SystemExit("cargo metadata omitted resolve graph")
    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        raise SystemExit("cargo metadata omitted resolve nodes")
    result: dict[str, set[str]] = {}
    for node in nodes:
        if not isinstance(node, dict):
            continue
        package_id = node.get("id")
        features = node.get("features")
        if isinstance(package_id, str) and isinstance(features, list):
            result[package_id] = {item for item in features if isinstance(item, str)}
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
    )
    arguments = parser.parse_args()
    repo = arguments.repo.resolve()

    with repo.joinpath("Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    features = manifest.get("features", {})
    if features.get("default") != ["production"]:
        raise SystemExit("default feature must delegate only to production")
    if features.get("production") != [
        "token-counting",
        "lite",
        "full",
        "semantic-fastembed",
    ]:
        raise SystemExit("production feature set changed without updating its contract")
    if "test-transport" in features["production"]:
        raise SystemExit("production feature directly enables test-transport")

    default = metadata(repo)
    production = metadata(
        repo, "--no-default-features", "--features", "production"
    )
    default_graph = resolved_features(default)
    production_graph = resolved_features(production)
    if default_graph.keys() != production_graph.keys():
        raise SystemExit("default and production resolve different package graphs")

    root_id = default["resolve"]["root"]
    default_graph[root_id].discard("default")
    mismatches = [
        package_id
        for package_id in sorted(default_graph)
        if default_graph[package_id] != production_graph[package_id]
    ]
    if mismatches:
        raise SystemExit(
            "default and production resolve different features for: "
            + ", ".join(mismatches)
        )
    contaminated = [
        package_id
        for package_id, package_features in production_graph.items()
        if "test-transport" in package_features
    ]
    if contaminated:
        raise SystemExit(
            "production graph enables test-transport for: " + ", ".join(contaminated)
        )

    print("default and production Cargo feature graphs are identical")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
