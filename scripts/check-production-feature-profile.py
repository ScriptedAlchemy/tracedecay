#!/usr/bin/env python3
"""Prove the production Cargo graph does not enable test-transport."""

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
    parser.add_argument(
        "--manifest-only",
        action="store_true",
        help="check the feature table and skip cargo tree",
    )
    arguments = parser.parse_args()
    repo = arguments.repo.resolve()

    # The product package moved to `crates/tracedecay`; the repository root is
    # now a virtual workspace manifest that declares no features at all.
    package_manifest = repo.joinpath("crates", "tracedecay", "Cargo.toml")
    if not package_manifest.is_file():
        raise SystemExit(f"missing product package manifest at {package_manifest}")
    with package_manifest.open("rb") as handle:
        manifest = tomllib.load(handle)
    features = manifest.get("features", {})
    if features.get("default") != ["production"]:
        raise SystemExit("default feature must delegate only to production")
    required_production = {"token-counting", "lite", "full"}
    if not required_production.issubset(set(features.get("production", []))):
        raise SystemExit("production feature set lost a required member")
    if "test-transport" in features["production"]:
        raise SystemExit("production feature directly enables test-transport")
    if arguments.manifest_only:
        print("production feature manifest is eligible for release")
        return 0

    # `default` is exactly `["production"]`, so a second default-feature walk
    # repeats this graph. `cargo metadata` unifies dev-dependency features
    # across the workspace and makes test-only transports look reachable;
    # this normal/build tree is the artifact `cargo build` produces.
    production_graph = resolved_features(
        repo, "--no-default-features", "--features", "production"
    )
    root_id = next(
        (
            package_id
            for package_id in production_graph
            if package_id.startswith("tracedecay ")
        ),
        None,
    )
    if root_id is None:
        raise SystemExit("cargo tree omitted the tracedecay root package")
    contaminated = [
        package_id
        for package_id, package_features in production_graph.items()
        if "test-transport" in package_features
    ]
    if contaminated:
        raise SystemExit(
            "production graph enables test-transport for: " + ", ".join(contaminated)
        )

    print("production Cargo feature graph excludes test-transport")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
