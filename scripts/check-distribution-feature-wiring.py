#!/usr/bin/env python3
"""Validate source and packaged Cargo feature ownership for distribution builds.

The checks are public-contract and layering rules only: the packaged manifest
must carry the source feature set, optional native dependencies must stay
optional and feature-wired, the root package must not own FastEmbed, the
supported `lang-*` surface must match the extraction owner, and each language
feature must compile in isolation. How a feature is forwarded through
intermediate crates is Cargo's job; resolved behavior is proven by the
packaged-artifact builds and launch checks in check-distribution-acceptance.sh.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import tomllib


REQUIRED_ROOT_FEATURES = {
    "full",
    "hotpath",
    "hotpath-alloc",
    "hotpath-cpu",
    "hotpath-mcp",
    "token-counting",
    "semantic-fastembed",
    "test-transport",
}
REQUIRED_CLI_FEATURE_MEMBERS = {
    "production": {"tracedecay/production"},
    "hotpath": {
        "dep:regex",
        "tracedecay/hotpath",
        "hotpath/hotpath",
        "hotpath/tokio",
        "hotpath/ureq-3",
    },
    "hotpath-alloc": {
        "hotpath",
        "tracedecay/hotpath-alloc",
        "hotpath/hotpath-alloc",
    },
    "hotpath-cpu": {
        "hotpath",
        "tracedecay/hotpath-cpu",
        "hotpath/hotpath-cpu",
    },
    "hotpath-mcp": {"hotpath", "hotpath/hotpath-mcp"},
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


def language_feature_names(features: dict) -> set[str]:
    return {name for name in features if name.startswith("lang-")}


def require_language_surface(
    name: str,
    features: dict,
    authority_features: set[str],
) -> None:
    """The public `lang-*` surface must advertise exactly the owner's languages."""
    actual_features = language_feature_names(features)
    if actual_features != authority_features:
        missing = sorted(authority_features - actual_features)
        extra = sorted(actual_features - authority_features)
        details = []
        if missing:
            details.append("missing " + ", ".join(missing))
        if extra:
            details.append("extra " + ", ".join(extra))
        raise SystemExit(
            f"distribution acceptance: {name} language features differ from "
            "tracedecay-code-extraction: " + "; ".join(details)
        )


def require_isolated_language_features_compile(
    manifest_path: Path,
    authority_features: set[str],
    cargo_config: Path | None,
    offline: bool,
) -> None:
    manifest = load(manifest_path)
    package_name = manifest.get("package", {}).get("name")
    if not isinstance(package_name, str):
        raise SystemExit(
            "distribution acceptance: extraction build manifest has no package name"
        )

    build_features = language_feature_names(manifest.get("features", {}))
    if build_features != authority_features:
        raise SystemExit(
            "distribution acceptance: extraction build manifest language features "
            "differ from the packaged authority"
        )

    for feature in sorted(authority_features):
        command = [
            "cargo",
            "check",
            "--manifest-path",
            str(manifest_path),
            "--package",
            package_name,
            "--lib",
            "--no-default-features",
            "--features",
            feature,
        ]
        if cargo_config is not None:
            command.extend(["--config", str(cargo_config)])
        if offline:
            command.append("--offline")
        completed = subprocess.run(
            command, check=False, capture_output=True, text=True
        )
        if completed.returncode != 0:
            details = completed.stderr.strip() or completed.stdout.strip()
            raise SystemExit(
                f"distribution acceptance: {feature} does not compile in isolation"
                + (f"\n{details}" if details else "")
            )


def validate(
    root_source: dict,
    root_packaged: dict,
    code_index_source: dict,
    code_index_packaged: dict,
    extraction_source: dict,
    extraction_packaged: dict,
    semantic_source: dict,
    semantic_packaged: dict,
    cli_source: dict,
    cli_packaged: dict,
) -> None:
    root_features = require_matching_features("root", root_source, root_packaged)
    missing = sorted(REQUIRED_ROOT_FEATURES - root_features.keys())
    if missing:
        raise SystemExit(
            "distribution acceptance: source manifest is missing required features: "
            + ", ".join(missing)
        )
    # Layering rule, not topology: the native semantic runtime is owned by
    # tracedecay-semantic (checked below) and never by the composition root.
    if "fastembed" in dependency_package_names(root_packaged):
        raise SystemExit(
            "distribution acceptance: root package must not own fastembed"
        )
    require_optional_dependencies_wired("root", root_packaged, root_features)

    require_matching_features(
        "tracedecay-code-index", code_index_source, code_index_packaged
    )
    extraction_features = require_matching_features(
        "tracedecay-code-extraction", extraction_source, extraction_packaged
    )
    require_language_surface(
        "root", root_features, language_feature_names(extraction_features)
    )
    require_optional_dependencies_wired(
        "tracedecay-code-extraction", extraction_packaged, extraction_features
    )

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
    fastembed_dependencies = [
        dependencies["fastembed"]
        for table in [semantic_packaged, *semantic_packaged.get("target", {}).values()]
        if isinstance(table, dict)
        and isinstance(dependencies := table.get("dependencies"), dict)
        and "fastembed" in dependencies
    ]
    if (
        len(fastembed_dependencies) != 1
        or not isinstance(fastembed_dependencies[0], dict)
        or fastembed_dependencies[0].get("optional") is not True
        or fastembed_dependencies[0].get("default-features") is not False
    ):
        raise SystemExit(
            "distribution acceptance: tracedecay-semantic fastembed must remain optional "
            "with default features disabled"
        )
    require_optional_dependencies_wired(
        "tracedecay-semantic", semantic_packaged, semantic_features
    )

    cli_features = require_matching_features(
        "tracedecay-cli", cli_source, cli_packaged
    )
    missing_cli = sorted(
        REQUIRED_CLI_FEATURE_MEMBERS.keys() - cli_features.keys()
    )
    if missing_cli:
        raise SystemExit(
            "distribution acceptance: tracedecay-cli is missing required features: "
            + ", ".join(missing_cli)
        )
    for feature, expected in REQUIRED_CLI_FEATURE_MEMBERS.items():
        members = cli_features.get(feature)
        # Extra crate passthroughs are allowed; the contract is the required
        # Hotpath/release members, not an exhaustive crate inventory.
        if not isinstance(members, list) or not expected.issubset(members):
            raise SystemExit(
                f"distribution acceptance: tracedecay-cli {feature} must enable "
                + ", ".join(sorted(expected))
            )
    require_optional_dependencies_wired(
        "tracedecay-cli", cli_packaged, cli_features
    )


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    root_manifest = repo / "crates/tracedecay/Cargo.toml"
    code_index_manifest = repo / "crates/tracedecay-code-index/Cargo.toml"
    extraction_manifest = repo / "crates/tracedecay-code-extraction/Cargo.toml"
    semantic_manifest = repo / "crates/tracedecay-semantic/Cargo.toml"
    cli_manifest = repo / "crates/tracedecay-cli/Cargo.toml"
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    # Source manifests default to this checkout; packaged manifests must be
    # the extracted `.crate` trees, so they have no default — comparing a
    # manifest with itself is not package verification.
    parser.add_argument("--root-source", type=Path, default=root_manifest)
    parser.add_argument("--root-packaged", type=Path, required=True)
    parser.add_argument("--code-index-source", type=Path, default=code_index_manifest)
    parser.add_argument("--code-index-packaged", type=Path, required=True)
    parser.add_argument("--extraction-source", type=Path, default=extraction_manifest)
    parser.add_argument("--extraction-packaged", type=Path, required=True)
    parser.add_argument("--semantic-source", type=Path, default=semantic_manifest)
    parser.add_argument("--semantic-packaged", type=Path, required=True)
    parser.add_argument("--cli-source", type=Path, default=cli_manifest)
    parser.add_argument("--cli-packaged", type=Path, required=True)
    parser.add_argument("--check-extraction-manifest", type=Path)
    parser.add_argument("--cargo-config", type=Path)
    parser.add_argument("--offline", action="store_true")
    arguments = parser.parse_args()
    extraction_packaged = load(arguments.extraction_packaged)
    validate(
        load(arguments.root_source),
        load(arguments.root_packaged),
        load(arguments.code_index_source),
        load(arguments.code_index_packaged),
        load(arguments.extraction_source),
        extraction_packaged,
        load(arguments.semantic_source),
        load(arguments.semantic_packaged),
        load(arguments.cli_source),
        load(arguments.cli_packaged),
    )
    if arguments.check_extraction_manifest is not None:
        require_isolated_language_features_compile(
            arguments.check_extraction_manifest,
            language_feature_names(extraction_packaged.get("features", {})),
            arguments.cargo_config,
            arguments.offline,
        )
    print("distribution feature wiring is valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
