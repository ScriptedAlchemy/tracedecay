#!/usr/bin/env python3
"""Resolve safe Cargo feature arguments for current or historical source tags."""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import sys
import tomllib


HOTPATH_RELEASE_FEATURES = (
    "hotpath",
    "hotpath-alloc",
    "hotpath-cpu",
    "hotpath-mcp",
)


def production_release_features(
    features: dict[str, object], target: str | None
) -> tuple[str, ...]:
    """Return the artifact feature set for a production-capable source tag."""

    present = set(HOTPATH_RELEASE_FEATURES).intersection(features)
    if not present:
        # Tags predating the shipped profiler remain recoverable with the
        # production profile they declared at the time.
        return ("production",)
    missing = set(HOTPATH_RELEASE_FEATURES).difference(features)
    if missing:
        raise SystemExit(
            "source Cargo.toml has an incomplete Hotpath release feature set: "
            + ", ".join(sorted(missing))
        )
    if target is None:
        raise SystemExit("--target is required for a Hotpath-enabled release source")

    selected = ["production", "hotpath", "hotpath-alloc"]
    if "-linux-" in target or target.endswith("-apple-darwin"):
        selected.append("hotpath-cpu")
    elif not target.endswith("-pc-windows-msvc"):
        raise SystemExit(f"unsupported Hotpath release target: {target}")
    selected.append("hotpath-mcp")
    return tuple(selected)


def expand_local_features(
    features: dict[str, list[str]], selected: list[str]
) -> set[str]:
    resolved: set[str] = set()
    pending = list(selected)
    while pending:
        feature = pending.pop()
        if feature in resolved:
            continue
        resolved.add(feature)
        for member in features.get(feature, []):
            if member in features:
                pending.append(member)
    return resolved


def write_output(path: Path | None, name: str, value: str) -> None:
    line = f"{name}={value}\n"
    if path is None:
        sys.stdout.write(line)
        return
    with path.open("a", encoding="utf-8") as handle:
        handle.write(line)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target")
    parser.add_argument("--github-output", type=Path)
    arguments = parser.parse_args()

    source = arguments.source.resolve()
    with source.joinpath("Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    features = manifest.get("features", {})
    if not isinstance(features, dict):
        raise SystemExit("source Cargo.toml has an invalid feature table")
    defaults = features.get("default", [])
    if not isinstance(defaults, list):
        raise SystemExit("source Cargo.toml has an invalid feature table")

    if "production" in features:
        checker = Path(__file__).with_name("check-production-feature-profile.py")
        subprocess.run(
            [sys.executable, str(checker), "--repo", str(source)],
            check=True,
        )
        profile = "production"
        cargo_features = ",".join(
            production_release_features(features, arguments.target)
        )
        cargo_args = f"--no-default-features --features {cargo_features}"
    else:
        resolved_defaults = expand_local_features(features, defaults)
        if "test-transport" in resolved_defaults:
            raise SystemExit(
                "historical default features enable test-transport; refusing release"
            )
        profile = "legacy-default"
        cargo_args = ""
        cargo_features = ""

    write_output(arguments.github_output, "profile", profile)
    write_output(arguments.github_output, "cargo_args", cargo_args)
    write_output(arguments.github_output, "cargo_features", cargo_features)
    print(f"release source profile: {profile}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
