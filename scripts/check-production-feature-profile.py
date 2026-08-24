#!/usr/bin/env python3
"""Prove default and explicit production Cargo profiles resolve identically."""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import tomllib


def resolved_features(repo: Path, *arguments: str) -> dict[str, set[str]]:
    completed = subprocess.run(
        [
            "cargo",
            "tree",
            "--locked",
            "--package",
            "tracedecay",
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}|{f}",
            *arguments,
        ],
        cwd=repo,
        check=True,
        capture_output=True,
        encoding="utf-8",
    )
    result: dict[str, set[str]] = {}
    for line in completed.stdout.splitlines():
        package_id, separator, feature_list = line.partition("|")
        if not separator:
            raise SystemExit(f"cargo tree emitted an invalid package row: {line}")
        package_id = package_id.removesuffix(" (*)")
        features = {feature for feature in feature_list.split(",") if feature}
        result.setdefault(package_id, set()).update(features)
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
    required_production = {"token-counting", "lite", "full", "semantic-fastembed"}
    if not required_production.issubset(set(features.get("production", []))):
        raise SystemExit("production feature set lost a required member")
    if "test-transport" in features["production"]:
        raise SystemExit("production feature directly enables test-transport")

    # `cargo metadata` unifies dev-dependency features across the workspace and
    # therefore makes test-only transports look production-reachable. Inspect
    # the root package's normal/build tree so this check matches the artifact
    # that `cargo build` actually produces.
    default_graph = resolved_features(repo)
    production_graph = resolved_features(
        repo, "--no-default-features", "--features", "production"
    )
    if default_graph.keys() != production_graph.keys():
        raise SystemExit("default and production resolve different package graphs")

    root_id = next(
        (package_id for package_id in default_graph if package_id.startswith("tracedecay ")),
        None,
    )
    if root_id is None:
        raise SystemExit("cargo tree omitted the tracedecay root package")
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
