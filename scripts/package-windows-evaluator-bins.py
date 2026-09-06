#!/usr/bin/env python3
"""Package, restore, and preflight Windows nextest evaluator executables."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any


KIND = "windows-nextest-evaluator-bins"
SCHEMA = 1
FEATURES = "tracedecay/test-helpers"
CARGO_ARGS = [
    "--workspace",
    "--bins",
    "--locked",
    "--features",
    FEATURES,
]
REQUIRED_BINARIES = (
    {
        "name": "tracedecay-search-eval",
        "override_env": "TRACEDECAY_SEARCH_EVAL_TEST_BIN",
    },
    {
        "name": "tracedecay-search-eval-direct",
        "override_env": "TRACEDECAY_SEARCH_EVAL_DIRECT_TEST_BIN",
    },
)
IDENTITY_NAME = "identity.json"


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def rustc_version() -> str:
    try:
        output = subprocess.run(
            ["rustc", "--version"],
            check=True,
            text=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        return f"unavailable ({error})"
    return output.stdout.strip()


def locate_binary(profile_dir: Path, name: str) -> Path:
    suffixed = profile_dir / f"{name}.exe"
    unsuffixed = profile_dir / name
    candidates = (suffixed, unsuffixed) if os.name == "nt" else (unsuffixed, suffixed)
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    searched = " or ".join(str(path) for path in candidates)
    fail(
        f"search-eval binary `{name}` is missing at {searched}; "
        "build it with `cargo build --workspace --bins --locked "
        f"--features {FEATURES}`"
    )


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def load_identity(directory: Path) -> dict[str, Any]:
    identity_path = directory / IDENTITY_NAME
    if not identity_path.is_file():
        fail(f"missing {identity_path}")
    try:
        payload = json.loads(identity_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        fail(f"invalid {identity_path}: {error}")
    if not isinstance(payload, dict):
        fail(f"{identity_path} must be an object")
    if payload.get("kind") != KIND or payload.get("schema") != SCHEMA:
        fail(f"{identity_path} is not a {KIND} schema {SCHEMA} manifest")
    binaries = payload.get("binaries")
    if not isinstance(binaries, list) or len(binaries) != len(REQUIRED_BINARIES):
        fail(f"{identity_path} must list the required evaluator binaries")
    return payload


def append_github_env(github_env: Path, bindings: list[tuple[str, Path]]) -> None:
    lines = "".join(f"{name}={path}\n" for name, path in bindings)
    with github_env.open("a", encoding="utf-8") as handle:
        handle.write(lines)


def package(profile_dir: Path, output_dir: Path, git_sha: str) -> None:
    if not git_sha.strip():
        fail("git sha is required so shards can report the packaged build identity")
    output_dir.mkdir(parents=True, exist_ok=True)
    binaries: list[dict[str, str]] = []
    for spec in REQUIRED_BINARIES:
        source = locate_binary(profile_dir, spec["name"])
        destination = output_dir / source.name
        shutil.copy2(source, destination)
        binaries.append(
            {
                "filename": source.name,
                "name": spec["name"],
                "override_env": spec["override_env"],
                "sha256": sha256_file(destination),
            }
        )
    write_json(
        output_dir / IDENTITY_NAME,
        {
            "binaries": binaries,
            "cargo_args": list(CARGO_ARGS),
            "features": FEATURES,
            "git_sha": git_sha.strip(),
            "kind": KIND,
            "rustc": rustc_version(),
            "schema": SCHEMA,
        },
    )


def restore(artifact_dir: Path, output_dir: Path, github_env: Path | None) -> None:
    identity = load_identity(artifact_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(artifact_dir / IDENTITY_NAME, output_dir / IDENTITY_NAME)
    bindings: list[tuple[str, Path]] = []
    for entry in identity["binaries"]:
        filename = entry.get("filename")
        override_env = entry.get("override_env")
        if not isinstance(filename, str) or not isinstance(override_env, str):
            fail("identity binaries must declare filename and override_env")
        source = artifact_dir / filename
        if not source.is_file():
            fail(f"artifact is missing {filename}")
        destination = output_dir / filename
        shutil.copy2(source, destination)
        bindings.append((override_env, destination.resolve()))
    if github_env is not None:
        append_github_env(github_env, bindings)


def preflight(directory: Path) -> None:
    identity = load_identity(directory)
    for entry in identity["binaries"]:
        filename = entry.get("filename")
        expected = entry.get("sha256")
        name = entry.get("name", filename)
        if not isinstance(filename, str) or not isinstance(expected, str):
            fail("identity binaries must declare filename and sha256")
        binary = directory / filename
        if not binary.is_file():
            fail(f"restored evaluator `{name}` is missing at {binary}")
        actual = sha256_file(binary)
        if actual != expected:
            fail(
                f"restored evaluator `{name}` sha256 {actual} does not match "
                f"packaged sha256 {expected}"
            )
        try:
            executed = subprocess.run(
                [str(binary), "--help"],
                check=False,
                text=True,
                capture_output=True,
            )
        except OSError as error:
            fail(f"failed to execute restored evaluator `{name}`: {error}")
        if executed.returncode != 0:
            fail(
                f"restored evaluator `{name}` failed to execute --help "
                f"(exit {executed.returncode}): {executed.stderr.strip()}"
            )
    print(json.dumps(identity, indent=2, sort_keys=True))
    print(
        "windows evaluator build identity "
        f"git_sha={identity.get('git_sha')} features={identity.get('features')} "
        f"rustc={identity.get('rustc')}"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    package_parser = subparsers.add_parser("package")
    package_parser.add_argument("--profile-dir", type=Path, required=True)
    package_parser.add_argument("--output-dir", type=Path, required=True)
    package_parser.add_argument("--git-sha", required=True)

    restore_parser = subparsers.add_parser("restore")
    restore_parser.add_argument("--artifact-dir", type=Path, required=True)
    restore_parser.add_argument("--output-dir", type=Path, required=True)
    restore_parser.add_argument("--github-env", type=Path)

    preflight_parser = subparsers.add_parser("preflight")
    preflight_parser.add_argument("--dir", type=Path, required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.command == "package":
        package(args.profile_dir, args.output_dir, args.git_sha)
        return
    if args.command == "restore":
        restore(args.artifact_dir, args.output_dir, args.github_env)
        return
    if args.command == "preflight":
        preflight(args.dir)
        return
    fail(f"unknown command {args.command}")


if __name__ == "__main__":
    main()
