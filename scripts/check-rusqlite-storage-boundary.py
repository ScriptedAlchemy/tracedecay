#!/usr/bin/env python3
"""Keep production rusqlite dependencies inside storage-owning crates."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess


STORAGE_OWNERS = {
    "tracedecay-query",
    "tracedecay-runtime-core",
    "tracedecay-rusqlite-runtime",
    "tracedecay-sessions",
}

SHIPPED_ROOTS = {
    "tracedecay": {"production"},
    "tracedecay-cli": {"production"},
}


class StorageBoundaryViolation(ValueError):
    pass


def run(repo: Path, arguments: list[str]) -> str:
    return subprocess.run(
        arguments,
        cwd=repo,
        check=True,
        capture_output=True,
        encoding="utf-8",
    ).stdout


def production_feature_closure(metadata: dict) -> dict[str, set[str]]:
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["source"] is None
    }
    missing_roots = SHIPPED_ROOTS.keys() - packages.keys()
    if missing_roots:
        raise StorageBoundaryViolation(
            "production root is absent from cargo metadata: "
            + ", ".join(sorted(missing_roots))
        )

    active_packages = set(SHIPPED_ROOTS)
    active_features = {
        name: set(features) for name, features in SHIPPED_ROOTS.items()
    }
    changed = True
    while changed:
        changed = False
        for package_name in tuple(active_packages):
            package = packages[package_name]
            dependencies: dict[str, list[dict]] = {}
            for dependency in package["dependencies"]:
                if dependency["kind"] == "dev":
                    continue
                alias = dependency["rename"] or dependency["name"]
                dependencies.setdefault(alias, []).append(dependency)

            enabled = active_features.setdefault(package_name, set())
            active_aliases = {
                alias
                for alias, variants in dependencies.items()
                if any(not dependency["optional"] for dependency in variants)
            }
            forwarded: list[tuple[str, str, bool]] = []
            for feature in tuple(enabled):
                for member in package["features"].get(feature, []):
                    if member.startswith("dep:"):
                        active_aliases.add(member.removeprefix("dep:"))
                    elif "/" in member:
                        alias, dependency_feature = member.split("/", maxsplit=1)
                        weak = alias.endswith("?")
                        alias = alias.removesuffix("?")
                        if not weak:
                            active_aliases.add(alias)
                        forwarded.append((alias, dependency_feature, weak))
                    elif member in package["features"]:
                        if member not in enabled:
                            enabled.add(member)
                            changed = True
                    elif member in dependencies:
                        active_aliases.add(member)

            for alias in active_aliases:
                for dependency in dependencies.get(alias, []):
                    dependency_name = dependency["name"]
                    if dependency_name not in packages:
                        continue
                    if dependency_name not in active_packages:
                        active_packages.add(dependency_name)
                        changed = True
                    dependency_features = active_features.setdefault(
                        dependency_name, set()
                    )
                    requested = set(dependency["features"])
                    if dependency["uses_default_features"]:
                        requested.add("default")
                    if not requested.issubset(dependency_features):
                        dependency_features.update(requested)
                        changed = True

            for alias, dependency_feature, weak in forwarded:
                if weak and alias not in active_aliases:
                    continue
                for dependency in dependencies.get(alias, []):
                    dependency_name = dependency["name"]
                    if dependency_name not in packages:
                        continue
                    dependency_features = active_features.setdefault(
                        dependency_name, set()
                    )
                    if dependency_feature not in dependency_features:
                        dependency_features.add(dependency_feature)
                        changed = True

    return active_features


def evaluate_storage_boundary(metadata: dict) -> set[str]:
    production_features = production_feature_closure(metadata)
    test_helper_packages = {
        package
        for package, features in production_features.items()
        if "test-helpers" in features
    }
    if test_helper_packages:
        raise StorageBoundaryViolation(
            "production features reach test-only helper surfaces: "
            + ", ".join(sorted(test_helper_packages))
        )

    declared_owners: set[str] = set()
    manifest_violations: set[str] = set()
    for package in metadata["packages"]:
        if package["source"] is not None:
            continue
        for dependency in package["dependencies"]:
            if dependency["name"] != "rusqlite" or dependency["kind"] == "dev":
                continue
            if dependency["kind"] is None and dependency["optional"]:
                enabling_features = {
                    name
                    for name, members in package["features"].items()
                    if "dep:rusqlite" in members
                }
                if enabling_features == {"test-helpers"}:
                    continue
            declared_owners.add(package["name"])
            if package["name"] not in STORAGE_OWNERS:
                manifest_violations.add(package["name"])
    if manifest_violations:
        raise StorageBoundaryViolation(
            "workspace package declares production rusqlite outside storage: "
            + ", ".join(sorted(manifest_violations))
        )

    missing_owners = STORAGE_OWNERS - declared_owners
    if missing_owners:
        raise StorageBoundaryViolation(
            "storage owner allowlist contains packages without production rusqlite: "
            + ", ".join(sorted(missing_owners))
        )
    return declared_owners


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repo",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
    )
    arguments = parser.parse_args()
    repo = arguments.repo.resolve()
    metadata = json.loads(
        run(
            repo,
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        )
    )
    try:
        declared_owners = evaluate_storage_boundary(metadata)
    except StorageBoundaryViolation as error:
        raise SystemExit(str(error)) from error
    print(
        "production rusqlite dependency owners: "
        + ", ".join(sorted(declared_owners))
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
