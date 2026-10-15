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

SHIPPED_ROOTS = ("tracedecay", "tracedecay-cli")


def run(repo: Path, arguments: list[str]) -> str:
    return subprocess.run(
        arguments,
        cwd=repo,
        check=True,
        capture_output=True,
        encoding="utf-8",
    ).stdout


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
    manifest_violations: list[str] = []
    for package in metadata["packages"]:
        if package["source"] is not None or package["name"] in STORAGE_OWNERS:
            continue
        for dependency in package["dependencies"]:
            if dependency["name"] != "rusqlite" or dependency["kind"] is not None:
                continue
            if dependency["optional"]:
                enabling_features = {
                    name
                    for name, members in package["features"].items()
                    if "dep:rusqlite" in members
                }
                if enabling_features == {"test-helpers"}:
                    continue
            manifest_violations.append(package["name"])
    if manifest_violations:
        raise SystemExit(
            "workspace package declares production rusqlite outside storage: "
            + ", ".join(sorted(set(manifest_violations)))
        )

    owners: set[str] = set()
    for root in SHIPPED_ROOTS:
        output = run(
            repo,
            [
                "cargo",
                "tree",
                "--locked",
                "--package",
                root,
                "--edges",
                "normal,build",
                "--no-default-features",
                "--features",
                "production",
                "--invert",
                "rusqlite",
                "--depth",
                "1",
                "--prefix",
                "none",
                "--format",
                "{p}",
            ],
        )
        rows = [line for line in output.splitlines() if line]
        if not rows or not rows[0].startswith("rusqlite "):
            raise SystemExit(f"{root} production graph omitted rusqlite")
        owners.update(row.split(maxsplit=1)[0] for row in rows[1:])
    violations = owners - STORAGE_OWNERS
    if violations:
        raise SystemExit(
            "production rusqlite dependency escaped the storage boundary: "
            + ", ".join(sorted(violations))
        )
    print("production rusqlite dependency owners: " + ", ".join(sorted(owners)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
