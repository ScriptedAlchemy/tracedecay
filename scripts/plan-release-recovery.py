#!/usr/bin/env python3
"""Plan release work without rebuilding already-published immutable artifacts."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path


def load_targets(path: Path) -> list[dict[str, str]]:
    # Planning and final artifact coverage use the same manifest validator.
    spec = importlib.util.spec_from_file_location(
        "release_artifacts", Path(__file__).with_name("check-release-artifacts.py")
    )
    if spec is None or spec.loader is None:
        raise SystemExit("release target validator is unavailable")
    validator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(validator)
    return validator.target_matrix(path)


def execution_matrix(
    missing: list[dict[str, str]], current: list[dict[str, str]]
) -> list[dict[str, str]]:
    """Select current runners without changing an immutable release's identity."""
    by_name = {target["name"]: target for target in current}
    result = []
    for target in missing:
        name = target["name"]
        execution = by_name.get(name)
        if execution is None:
            raise SystemExit(f"no execution runner for release target: {name}")
        for field in ("target", "archive"):
            if execution[field] != target[field]:
                raise SystemExit(
                    f"execution release target {name} changes {field}: "
                    f"{target[field]!r} != {execution[field]!r}"
                )
        result.append({**target, "runner": execution["runner"]})
    return result


def target_assets(
    target: dict[str, str], tag: str, profile: str
) -> tuple[str, ...]:
    name = target["name"]
    archive = target["archive"]
    prefix = "tracedecay-beta" if profile == "beta" else "tracedecay"
    return (
        f"{prefix}-{tag}-{name}.{archive}",
        f"{prefix}-{tag}-{name}.mcpb",
        f"{prefix}-{tag}-{name}.spdx.json",
    )


def plan(
    targets: list[dict[str, str]],
    tag: str,
    profile: str,
    existing: set[str],
) -> tuple[list[dict[str, str]], list[str]]:
    expected_mutable = {
        asset
        for target in targets
        for asset in target_assets(target, tag, profile)
    }
    # Both profiles publish the installer beside the archives, so `install.sh`
    # is immutable release metadata on a prerelease exactly as it is on a
    # stable release. While it counted as unexpected under `beta`, uploading
    # it to a prerelease would have made every later recovery run for that tag
    # abort.
    fixed = {"SHA256SUMS", "install.sh"}
    unexpected = sorted(existing - expected_mutable - fixed)
    if unexpected:
        raise SystemExit(
            "unexpected existing release assets: " + ", ".join(unexpected)
        )

    missing_packages = {
        target["name"]: any(
            asset not in existing
            for asset in target_assets(target, tag, profile)[:2]
        )
        for target in targets
    }
    missing_targets = [
        {
            **target,
            **(
                {"inventory_only": True}
                if not missing_packages[target["name"]]
                else {}
            ),
        }
        for target in targets
        if missing_packages[target["name"]]
        or target_assets(target, tag, profile)[2] not in existing
    ]
    finalized = sorted(existing & fixed)
    if finalized and any(missing_packages[target["name"]] for target in missing_targets):
        raise SystemExit(
            "final release metadata exists before all immutable artifacts "
            f"({', '.join(finalized)}); refusing destructive recovery"
        )
    retained = sorted(existing & expected_mutable)
    return missing_targets, retained


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument(
        "--execution-manifest",
        type=Path,
        required=True,
        help="release target manifest from the running workflow revision",
    )
    parser.add_argument("--tag", required=True)
    parser.add_argument("--profile", choices=("stable", "beta"), default="stable")
    parser.add_argument("--asset-names", type=Path, required=True)
    parser.add_argument("--retained-output", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    arguments = parser.parse_args()

    targets = load_targets(arguments.manifest)
    execution_targets = load_targets(arguments.execution_manifest)
    existing = {
        line
        for line in arguments.asset_names.read_text(encoding="utf-8").splitlines()
        if line
    }
    missing, retained = plan(targets, arguments.tag, arguments.profile, existing)
    missing = execution_matrix(missing, execution_targets)
    arguments.retained_output.write_text(
        "".join(f"{asset}\n" for asset in retained),
        encoding="utf-8",
    )
    matrix = json.dumps({"include": missing}, separators=(",", ":"))
    package_build_required = any(
        not target.get("inventory_only", False) for target in missing
    )
    current_by_name = {target["name"]: target for target in execution_targets}
    # SDK packages need the full source roster, even when daemon assets have
    # already shipped. Historical releases without a native SDK may have
    # retired targets; their completed assets still need no execution runner.
    supported = [
        target for target in targets
        if all(
            current_by_name.get(target["name"], {}).get(field) == target[field]
            for field in ("target", "archive")
        )
    ]
    release_matrix = json.dumps(
        {"include": execution_matrix(supported, execution_targets)},
        separators=(",", ":"),
    )
    with arguments.github_output.open("a", encoding="utf-8") as output:
        output.write(f"matrix={matrix}\n")
        output.write(f"release_matrix={release_matrix}\n")
        output.write(f"build_required={'true' if missing else 'false'}\n")
        output.write(
            f"package_build_required={'true' if package_build_required else 'false'}\n"
        )


if __name__ == "__main__":
    main()
