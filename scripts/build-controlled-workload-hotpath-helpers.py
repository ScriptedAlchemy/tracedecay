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


EXAMPLE = "emit_controlled_workload_reports"
FEATURE = "controlled-workload-hotpath"


def cargo_target_directory(source: Path) -> Path:
    output = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        cwd=source,
        text=True,
    )
    return Path(json.loads(output)["target_directory"])


def build_example(source: Path, profile: str, target: str | None, feature: str | None) -> None:
    command = [
        "cargo",
        "build",
        "-p",
        "tracedecay-search-eval",
        "--profile",
        profile,
        "--example",
        EXAMPLE,
        "--locked",
    ]
    if target is not None:
        command.extend(["--target", target])
    if feature is not None:
        command.extend(["--features", feature])
    subprocess.run(command, cwd=source, check=True)


def executable_suffix(target: str | None) -> str:
    if target is not None:
        return ".exe" if "windows" in target else ""
    return ".exe" if os.name == "nt" else ""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.cwd())
    parser.add_argument("--profile", choices=("test", "release"), required=True)
    parser.add_argument("--target")
    args = parser.parse_args()

    source = args.source.resolve()
    target_root = cargo_target_directory(source)
    if args.target is not None:
        target_root /= args.target
    profile_directory = "debug" if args.profile == "test" else "release"
    suffix = executable_suffix(args.target)
    example = target_root / profile_directory / "examples" / f"{EXAMPLE}{suffix}"
    helper_directory = target_root / "controlled-workload-hotpath"
    helper_directory.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(dir=helper_directory, prefix=".build-") as staging_name:
        staging = Path(staging_name)
        staged_off = staging / f"hotpath-off{suffix}"
        staged_on = staging / f"hotpath-on{suffix}"

        build_example(source, args.profile, args.target, None)
        shutil.copy2(example, staged_off)
        build_example(source, args.profile, args.target, FEATURE)
        shutil.copy2(example, staged_on)

        off = helper_directory / staged_off.name
        on = helper_directory / staged_on.name
        os.replace(staged_off, off)
        os.replace(staged_on, on)

    print(f"built controlled-workload Hotpath helpers: {off} {on}")


if __name__ == "__main__":
    main()
