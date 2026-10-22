#!/usr/bin/env python3
"""Plan release work without rebuilding already-published immutable artifacts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def load_targets(path: Path) -> list[dict[str, str]]:
    value = json.loads(path.read_text(encoding="utf-8"))
    targets = value.get("include")
    if not isinstance(targets, list) or not targets:
        raise SystemExit("release target manifest has no targets")
    return targets


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
    parser.add_argument("--tag", required=True)
    parser.add_argument("--profile", choices=("stable", "beta"), default="stable")
    parser.add_argument("--asset-names", type=Path, required=True)
    parser.add_argument("--retained-output", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    arguments = parser.parse_args()

    targets = load_targets(arguments.manifest)
    existing = {
        line
        for line in arguments.asset_names.read_text(encoding="utf-8").splitlines()
        if line
    }
    missing, retained = plan(targets, arguments.tag, arguments.profile, existing)
    arguments.retained_output.write_text(
        "".join(f"{asset}\n" for asset in retained),
        encoding="utf-8",
    )
    matrix = json.dumps({"include": missing}, separators=(",", ":"))
    package_build_required = any(
        not target.get("inventory_only", False) for target in missing
    )
    with arguments.github_output.open("a", encoding="utf-8") as output:
        output.write(f"matrix={matrix}\n")
        output.write(f"build_required={'true' if missing else 'false'}\n")
        output.write(
            f"package_build_required={'true' if package_build_required else 'false'}\n"
        )


if __name__ == "__main__":
    main()
