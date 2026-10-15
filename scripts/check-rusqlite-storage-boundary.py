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
        raise SystemExit(
            "workspace package declares production rusqlite outside storage: "
            + ", ".join(sorted(manifest_violations))
        )

    missing_owners = STORAGE_OWNERS - declared_owners
    if missing_owners:
        raise SystemExit(
            "storage owner allowlist contains packages without production rusqlite: "
            + ", ".join(sorted(missing_owners))
        )
    print(
        "production rusqlite dependency owners: "
        + ", ".join(sorted(declared_owners))
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
