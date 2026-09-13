#!/usr/bin/env python3
"""Build the feature-off/on executables used by Hotpath parity tests."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


PACKAGE = "tracedecay-search-eval"
EXAMPLE = "emit_controlled_workload_reports"
FEATURE = "controlled-workload-hotpath"


def cargo_target_directory(source: Path) -> Path:
    output = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        cwd=source,
        text=True,
    )
    return Path(json.loads(output)["target_directory"])


def build_example(
    source: Path,
    profile: str,
    target: str | None,
    workspace: bool,
    packages: list[str],
    base_features: str | None,
    hotpath: bool,
) -> None:
    # Package selection drives feature unification. `--workspace`, or the
    # exact `-p` set a CI partition compiled its tests with, lets that lane
    # reuse its graph for the feature-off build instead of resolving a second
    # one for this package.
    if workspace:
        selection = ["--workspace"]
    elif packages:
        selection = [flag for package in packages for flag in ("-p", package)]
    else:
        selection = ["-p", PACKAGE]
    features = [] if base_features is None else [base_features]
    if hotpath:
        features.append(FEATURE if selection == ["-p", PACKAGE] else f"{PACKAGE}/{FEATURE}")
    command = [
        "cargo",
        "build",
        *selection,
        "--profile",
        profile,
        "--example",
        EXAMPLE,
        "--locked",
    ]
    if target is not None:
        command.extend(["--target", target])
    if features:
        command.extend(["--features", ",".join(features)])
    subprocess.run(command, cwd=source, check=True)


def executable_suffix(target: str | None) -> str:
    if target is not None:
        return ".exe" if "windows" in target else ""
    return ".exe" if os.name == "nt" else ""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.cwd())
    parser.add_argument("--profile", choices=("test", "perf", "release"), required=True)
    parser.add_argument("--target")
    parser.add_argument(
        "--workspace",
        action="store_true",
        help=f"select the whole workspace instead of -p {PACKAGE}, sharing "
        "the caller's dependency graph",
    )
    parser.add_argument(
        "-p",
        "--package",
        dest="packages",
        action="append",
        default=[],
        help=f"select these packages instead of -p {PACKAGE} (repeatable); the "
        "set must include it and should be the one the calling lane compiled "
        "its tests with, so the feature-off build is a cache hit",
    )
    parser.add_argument(
        "--features",
        help="cargo features to enable on both builds (for example the "
        "root fixture feature the CI test lane compiles with)",
    )
    args = parser.parse_args()
    if args.workspace and args.packages:
        parser.error("--workspace and --package are exclusive")
    if args.packages and PACKAGE not in args.packages:
        parser.error(f"--package selection must include {PACKAGE}")

    source = args.source.resolve()
    target_root = cargo_target_directory(source)
    if args.target is not None:
        target_root /= args.target
    # Cargo places the built-in `test` profile under `debug`; custom profiles
    # such as `perf` use their own name.
    profile_directory = "debug" if args.profile == "test" else args.profile
    suffix = executable_suffix(args.target)
    example = target_root / profile_directory / "examples" / f"{EXAMPLE}{suffix}"
    helper_directory = target_root / "controlled-workload-hotpath"
    helper_directory.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(dir=helper_directory, prefix=".build-") as staging_name:
        staging = Path(staging_name)
        staged_off = staging / f"hotpath-off{suffix}"
        staged_on = staging / f"hotpath-on{suffix}"

        build_example(
            source, args.profile, args.target, args.workspace, args.packages, args.features, False
        )
        shutil.copy2(example, staged_off)
        build_example(
            source, args.profile, args.target, args.workspace, args.packages, args.features, True
        )
        shutil.copy2(example, staged_on)

        off = helper_directory / staged_off.name
        on = helper_directory / staged_on.name
        os.replace(staged_off, off)
        os.replace(staged_on, on)

    print(f"built controlled-workload Hotpath helpers: {off} {on}")


if __name__ == "__main__":
    main()
