#!/usr/bin/env python3
"""Validate source and packaged Cargo feature ownership for distribution builds."""

from __future__ import annotations

import argparse
from pathlib import Path
import tomllib


REQUIRED_ROOT_FEATURES = {
    "full",
    "token-counting",
    "semantic-fastembed",
    "test-transport",
}
REQUIRED_ROOT_SEMANTIC_MEMBERS = {
    "tracedecay-semantic/semantic-fastembed",
    "tracedecay-usecases/semantic-fastembed",
}
REQUIRED_SEMANTIC_MEMBERS = {
    "dep:fastembed",
    "fastembed/ort-download-binaries-rustls-tls",
}


def load(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def optional_dependencies(manifest: dict) -> set[str]:
    names: set[str] = set()

    def collect(table: object) -> None:
        if not isinstance(table, dict):
            return
        dependencies = table.get("dependencies")
        if isinstance(dependencies, dict):
            for name, spec in dependencies.items():
                if isinstance(spec, dict) and spec.get("optional") is True:
                    names.add(name)

    collect(manifest)
    for target in manifest.get("target", {}).values():
        collect(target)
    return names


def dependency_package_names(manifest: dict) -> set[str]:
    names: set[str] = set()

    def collect(table: object) -> None:
        if not isinstance(table, dict):
            return
        dependencies = table.get("dependencies")
        if isinstance(dependencies, dict):
            for name, spec in dependencies.items():
                package = spec.get("package") if isinstance(spec, dict) else None
                names.add(package if isinstance(package, str) else name)

    collect(manifest)
    for target in manifest.get("target", {}).values():
        collect(target)
    return names


def require_matching_features(name: str, source: dict, packaged: dict) -> dict:
    source_features = source.get("features", {})
    packaged_features = packaged.get("features", {})
    if source_features != packaged_features:
        raise SystemExit(
            f"distribution acceptance: packaged {name} feature wiring differs from Cargo.toml"
        )
    return packaged_features


def require_optional_dependencies_wired(name: str, manifest: dict, features: dict) -> None:
    references = {
        item
        for members in features.values()
        for item in members
        if isinstance(item, str)
    }
    unwired = sorted(
        dependency
        for dependency in optional_dependencies(manifest)
        if f"dep:{dependency}" not in references and dependency not in features
    )
    if unwired:
        raise SystemExit(
            f"distribution acceptance: {name} optional dependencies are not feature-wired: "
            + ", ".join(unwired)
        )


def validate(
    root_source: dict,
    root_packaged: dict,
    semantic_source: dict,
    semantic_packaged: dict,
) -> None:
    root_features = require_matching_features("root", root_source, root_packaged)
    missing = sorted(REQUIRED_ROOT_FEATURES - root_features.keys())
    if missing:
        raise SystemExit(
            "distribution acceptance: source manifest is missing required features: "
            + ", ".join(missing)
        )
    root_semantic_members = root_features.get("semantic-fastembed")
    if "fastembed" in dependency_package_names(root_packaged):
        raise SystemExit(
            "distribution acceptance: root package must not own fastembed"
        )
    if (
        not isinstance(root_semantic_members, list)
        or set(root_semantic_members) != REQUIRED_ROOT_SEMANTIC_MEMBERS
    ):
        raise SystemExit(
            "distribution acceptance: root semantic-fastembed must forward to the "
            "semantic and usecases owners"
        )
    require_optional_dependencies_wired("root", root_packaged, root_features)

    semantic_features = require_matching_features(
        "tracedecay-semantic", semantic_source, semantic_packaged
    )
    semantic_members = semantic_features.get("semantic-fastembed")
    if not isinstance(semantic_members, list) or not REQUIRED_SEMANTIC_MEMBERS.issubset(
        semantic_members
    ):
        raise SystemExit(
            "distribution acceptance: tracedecay-semantic semantic-fastembed must enable "
            "dep:fastembed and fastembed/ort-download-binaries-rustls-tls"
        )
    fastembed_dependency = semantic_packaged.get("dependencies", {}).get("fastembed")
    if (
        not isinstance(fastembed_dependency, dict)
        or fastembed_dependency.get("optional") is not True
        or fastembed_dependency.get("default-features") is not False
    ):
        raise SystemExit(
            "distribution acceptance: tracedecay-semantic fastembed must remain optional "
            "with default features disabled"
        )
    require_optional_dependencies_wired(
        "tracedecay-semantic", semantic_packaged, semantic_features
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root-source", type=Path, required=True)
    parser.add_argument("--root-packaged", type=Path, required=True)
    parser.add_argument("--semantic-source", type=Path, required=True)
    parser.add_argument("--semantic-packaged", type=Path, required=True)
    arguments = parser.parse_args()
    validate(
        load(arguments.root_source),
        load(arguments.root_packaged),
        load(arguments.semantic_source),
        load(arguments.semantic_packaged),
    )
    print("distribution feature wiring is valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
