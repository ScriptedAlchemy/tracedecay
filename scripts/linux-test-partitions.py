#!/usr/bin/env python3
"""Resolve the Linux test partitions and prove they cover every test target.

`.github/linux-test-partitions.json` names the partitions the Linux test lane
compiles and runs as parallel jobs. Each partition is a cargo selection
(`-p <package>` plus target flags such as `--lib` or `--test <name>`), so the
job builds only the test targets it runs and the lane's wall time is the
slowest partition rather than the whole workspace. That is also the failure
mode this script exists for: a target that no partition selects is a target
CI never runs. `check` therefore resolves every partition against `cargo
metadata` with cargo's own selection rules and fails unless every test target
in the workspace is selected by exactly one partition, or is listed under
`not_run` with a reason.

macOS runs the same partitions grouped: every partition names one of the
`macos_groups` under `macos_group`, and each group is one hosted 3-vCPU job
that runs its partitions' selections in turn against one target directory.
`check` also proves that every partition names a listed group and every group
runs at least one partition, and that the manifest stays within the
MACOS_GROUP_CAP concurrent macOS jobs a run may take. A group's
`budget_basis` is documentation: the measurement its `timeout_minutes` rests
on.

    linux-test-partitions.py [--metadata FILE] check
    linux-test-partitions.py [--metadata FILE] cargo-args <partition>
    linux-test-partitions.py [--metadata FILE] build-args <partition>
    linux-test-partitions.py matrix
    linux-test-partitions.py macos-matrix

`cargo-args` prints the selection for one partition as a shell-quoted
argument list. `build-args` prints that selection plus the partition's
`executables` — the binaries and examples its tests spawn rather than link,
which a test build does not produce on its own — for a `cargo build` that
shares the test build's resolution; it prints nothing for a partition with no
executables. `matrix` and `macos-matrix` print the `strategy.matrix`
documents the Linux and macOS jobs feed through `fromJSON`.
"""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / ".github/linux-test-partitions.json"

# cargo metadata reports every kind a target can have; a test run only ever
# builds the ones with a test harness, and the lane's `cargo test-ci` alias
# selects `--workspace` with no target flags, which is exactly the set of
# targets whose manifest `test` flag is true.
LIBRARY_KINDS = frozenset({"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"})
TARGET_FLAGS = {
    "lib": ("lib", "--lib"),
    "bins": ("bin", "--bins"),
    "tests": ("test", "--tests"),
    "examples": ("example", "--examples"),
    "benches": ("bench", "--benches"),
}
NAMED_TARGET_FLAGS = {
    "bin": ("bin", "--bin"),
    "test": ("test", "--test"),
    "example": ("example", "--example"),
    "bench": ("bench", "--bench"),
}
# The account's hosted concurrency admits five macOS jobs at once. One run
# takes at most four, so another run's macOS jobs can start beside it.
MACOS_GROUP_CAP = 4


class PartitionError(ValueError):
    """The manifest does not describe a complete, disjoint partition."""


@dataclass(frozen=True)
class TestTarget:
    package: str
    kind: str
    name: str
    required_features: frozenset[str]

    @property
    def label(self) -> str:
        return f"{self.package} {self.kind} `{self.name}`"


def target_kind(target: dict[str, Any]) -> str:
    kinds = set(target["kind"])
    if kinds & LIBRARY_KINDS:
        return "lib"
    if len(kinds) != 1:
        raise PartitionError(f"target {target['name']!r} has unexpected kinds {sorted(kinds)}")
    return next(iter(kinds))


def load_metadata(path: Path | None) -> dict[str, Any]:
    if path is not None:
        return json.loads(path.read_text(encoding="utf-8"))
    output = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        cwd=ROOT,
        text=True,
    )
    return json.loads(output)


@dataclass(frozen=True)
class Workspace:
    """The test targets and feature tables `cargo metadata --no-deps` reports."""

    targets: dict[str, list[TestTarget]]
    features: dict[str, dict[str, list[str]]]

    @property
    def universe(self) -> list[TestTarget]:
        return [target for targets in self.targets.values() for target in targets]


def load_workspace(metadata: dict[str, Any]) -> Workspace:
    """Every target a `cargo test` with no target flags would build, per package."""
    by_package: dict[str, list[TestTarget]] = {}
    features: dict[str, dict[str, list[str]]] = {}
    for package in metadata["packages"]:
        targets = by_package.setdefault(package["name"], [])
        features[package["name"]] = package.get("features", {})
        for raw in package["targets"]:
            if not raw.get("test", False):
                continue
            target = TestTarget(
                package=package["name"],
                kind=target_kind(raw),
                name=raw["name"],
                required_features=frozenset(raw.get("required-features", [])),
            )
            targets.append(target)
    return Workspace(by_package, features)


def load_manifest(path: Path = MANIFEST_PATH) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    partitions = document.get("partitions")
    if not isinstance(partitions, list) or not partitions:
        raise PartitionError(f"{path} has no partitions")
    names = [partition.get("name") for partition in partitions]
    if any(not isinstance(name, str) or not name for name in names):
        raise PartitionError(f"{path}: every partition needs a name")
    if len(set(names)) != len(names):
        raise PartitionError(f"{path}: partition names repeat")
    for partition in partitions:
        for key in ("packages", "timeout_minutes", "macos_group"):
            if key not in partition:
                raise PartitionError(f"partition {partition['name']!r} has no {key!r}")
        if not isinstance(partition["packages"], list) or not partition["packages"]:
            raise PartitionError(f"partition {partition['name']!r} selects no packages")
        if not isinstance(partition["timeout_minutes"], int) or partition["timeout_minutes"] <= 0:
            raise PartitionError(f"partition {partition['name']!r} needs a positive timeout")
        for selector in partition.get("executables", []):
            kind_name, _, target_name = selector.partition(":")
            if kind_name not in ("bins", "bin", "example") or bool(target_name) != (kind_name != "bins"):
                raise PartitionError(
                    f"partition {partition['name']!r}: executables take `bins`, `bin:<name>` "
                    f"or `example:<name>`, not {selector!r}"
                )
    not_run = document.get("not_run", {})
    if not isinstance(not_run, dict) or any(
        not isinstance(reason, str) or not reason for reason in not_run.values()
    ):
        raise PartitionError(f"{path}: not_run must map `package::target` to a reason")
    macos_groups(document)
    return document


def macos_groups(document: dict[str, Any]) -> dict[str, list[str]]:
    """The macOS groups and the partitions each runs, in manifest order.

    Every partition names exactly one listed group and every group runs at
    least one partition, so a partition cannot fall out of the macOS lane and
    a group job cannot run nothing; the group count is bounded by the macOS
    concurrency one run may take.
    """
    groups = document.get("macos_groups")
    if not isinstance(groups, list) or not groups:
        raise PartitionError("manifest has no macos_groups")
    members: dict[str, list[str]] = {}
    for group in groups:
        name = group.get("name") if isinstance(group, dict) else None
        if not isinstance(name, str) or not name:
            raise PartitionError("every macOS group needs a name")
        if name in members:
            raise PartitionError(f"macOS group names repeat: {name!r}")
        timeout = group.get("timeout_minutes")
        if not isinstance(timeout, int) or timeout <= 0:
            raise PartitionError(f"macOS group {name!r} needs a positive timeout")
        members[name] = []
    if len(members) > MACOS_GROUP_CAP:
        raise PartitionError(
            f"{len(members)} macOS groups exceed the {MACOS_GROUP_CAP} concurrent macOS jobs a run may take"
        )
    for partition in document["partitions"]:
        if "macos_group" not in partition:
            raise PartitionError(f"partition {partition['name']!r} has no 'macos_group'")
        group = partition["macos_group"]
        if group not in members:
            raise PartitionError(
                f"partition {partition['name']!r} names macOS group {group!r}, "
                f"which is not listed under macos_groups"
            )
        members[group].append(partition["name"])
    for name, partitions in members.items():
        if not partitions:
            raise PartitionError(f"macOS group {name!r} runs no partition")
    return members


def enabled_features(
    partition: dict[str, Any], package: str, feature_table: dict[str, list[str]]
) -> frozenset[str]:
    """Features the partition turns on for `package`, including the ones they imply.

    `feature_table` is the package's `[features]` map from cargo metadata; a
    feature that lists another feature of the same package enables it too
    (`test-transport = ["test-helpers", ...]`), so `required-features` are
    judged against the closure, as cargo does.
    """
    pending: list[str] = []
    for feature in partition.get("features", []):
        owner, _, name = feature.rpartition("/")
        if owner == package or (not owner and len(partition["packages"]) == 1):
            pending.append(name)
    enabled: set[str] = set()
    while pending:
        name = pending.pop()
        if name in enabled:
            continue
        enabled.add(name)
        for implied in feature_table.get(name, []):
            if "/" not in implied and not implied.startswith("dep:"):
                pending.append(implied)
    return frozenset(enabled)


def select(partition: dict[str, Any], workspace: Workspace) -> tuple[list[TestTarget], list[str]]:
    """Apply cargo's package and target selection rules to one partition.

    Returns the targets the partition builds and runs, plus the cargo arguments
    that produce that selection. A target with `required-features` counts as
    selected only when the partition enables them, because cargo otherwise
    skips it silently.
    """
    name = partition["name"]
    by_package = workspace.targets
    args: list[str] = []
    packages = partition["packages"]
    for package in packages:
        if package not in by_package:
            raise PartitionError(f"partition {name!r} selects unknown package {package!r}")
        args.extend(["-p", package])

    selectors = partition.get("targets", [])
    selected: list[TestTarget] = []
    if not selectors:
        # No target flag: cargo builds every target with `test = true`.
        for package in packages:
            selected.extend(by_package[package])
    for selector in selectors:
        kind_name, _, target_name = selector.partition(":")
        if target_name:
            if kind_name not in NAMED_TARGET_FLAGS:
                raise PartitionError(f"partition {name!r}: unknown selector {selector!r}")
            kind, flag = NAMED_TARGET_FLAGS[kind_name]
            args.extend([flag, target_name])
            matches = [
                target
                for package in packages
                for target in by_package[package]
                if target.kind == kind and target.name == target_name
            ]
            if not matches:
                raise PartitionError(
                    f"partition {name!r}: no {kind} target named {target_name!r} "
                    f"in {', '.join(packages)}"
                )
            selected.extend(matches)
        else:
            if kind_name not in TARGET_FLAGS:
                raise PartitionError(f"partition {name!r}: unknown selector {selector!r}")
            kind, flag = TARGET_FLAGS[kind_name]
            args.append(flag)
            selected.extend(
                target
                for package in packages
                for target in by_package[package]
                if target.kind == kind
            )

    features = partition.get("features", [])
    for feature in features:
        owner, _, _ = feature.rpartition("/")
        # cargo rejects `pkg/feature` for a package outside the selection.
        if owner and owner not in packages:
            raise PartitionError(
                f"partition {name!r} enables {feature!r} but does not select {owner!r}"
            )
    if features:
        args.extend(["--features", ",".join(features)])

    runnable = [
        target
        for target in selected
        if target.required_features
        <= enabled_features(partition, target.package, workspace.features[target.package])
    ]
    return runnable, args


def check(document: dict[str, Any], metadata: dict[str, Any]) -> list[str]:
    """Return a human-readable coverage summary; raise on any gap or overlap."""
    workspace = load_workspace(metadata)
    universe = workspace.universe
    owners: dict[TestTarget, list[str]] = {target: [] for target in universe}
    lines: list[str] = []
    for partition in document["partitions"]:
        runnable, _ = select(partition, workspace)
        if not runnable:
            raise PartitionError(f"partition {partition['name']!r} runs no test target")
        for target in runnable:
            owners[target].append(partition["name"])
        lines.append(f"{partition['name']}: {len(runnable)} test targets")

    not_run = document.get("not_run", {})
    problems: list[str] = []
    for target, names in owners.items():
        key = f"{target.package}::{target.name}"
        if len(names) > 1:
            problems.append(f"{target.label} is in {len(names)} partitions: {', '.join(names)}")
        elif not names and key not in not_run:
            problems.append(f"{target.label} is in no partition (add it or list it under not_run)")
        elif names and key in not_run:
            problems.append(f"{target.label} is listed under not_run but partition {names[0]!r} runs it")
    known = {f"{target.package}::{target.name}" for target in universe}
    for key in not_run:
        if key not in known:
            problems.append(f"not_run lists {key!r}, which is not a test target in the workspace")
    if problems:
        raise PartitionError("\n".join(problems))

    lines.append(
        f"{len(universe)} test targets: {sum(1 for names in owners.values() if names)} in exactly "
        f"one partition, {len(not_run)} listed under not_run"
    )
    groups = macos_groups(document)
    for group, partitions in groups.items():
        lines.append(f"macOS {group}: {', '.join(partitions)}")
    lines.append(
        f"{len(document['partitions'])} partitions: each in exactly one of {len(groups)} macOS groups"
    )
    return lines


def cargo_args(document: dict[str, Any], metadata: dict[str, Any], name: str) -> list[str]:
    workspace = load_workspace(metadata)
    for partition in document["partitions"]:
        if partition["name"] == name:
            _, args = select(partition, workspace)
            return args
    raise PartitionError(f"no partition named {name!r}")


def build_args(document: dict[str, Any], metadata: dict[str, Any], name: str) -> list[str]:
    """The test selection plus the executables the partition's tests spawn.

    Selecting the test targets keeps dev-dependencies in the resolution, so the
    build shares every unit with the nextest run that follows; the extra
    `--bins` / `--example` flags add the executables cargo would otherwise only
    link for a package whose own integration tests are selected.
    """
    workspace = load_workspace(metadata)
    for partition in document["partitions"]:
        if partition["name"] != name:
            continue
        executables = partition.get("executables", [])
        if not executables:
            return []
        _, args = select(partition, workspace)
        extra: list[str] = []
        for selector in executables:
            kind_name, _, target_name = selector.partition(":")
            if kind_name == "bins":
                extra.append("--bins")
            else:
                extra.extend([f"--{kind_name}", target_name])
        # Keep `--features` last, as cargo prints it and as `select` emits it.
        if "--features" in args:
            index = args.index("--features")
            return args[:index] + extra + args[index:]
        return args + extra
    raise PartitionError(f"no partition named {name!r}")


def matrix(document: dict[str, Any]) -> dict[str, Any]:
    return {
        "include": [
            {"partition": partition["name"], "timeout": partition["timeout_minutes"]}
            for partition in document["partitions"]
        ]
    }


def macos_matrix(document: dict[str, Any]) -> dict[str, Any]:
    """One matrix entry per macOS group; `partitions` is the space-separated run order."""
    members = macos_groups(document)
    return {
        "include": [
            {
                "group": group["name"],
                "timeout": group["timeout_minutes"],
                "partitions": " ".join(members[group["name"]]),
            }
            for group in document["macos_groups"]
        ]
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--manifest", type=Path, default=MANIFEST_PATH)
    parser.add_argument("--metadata", type=Path, help="pre-recorded `cargo metadata` JSON")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check")
    for command in ("cargo-args", "build-args"):
        commands.add_parser(command).add_argument("partition")
    commands.add_parser("matrix")
    commands.add_parser("macos-matrix")
    args = parser.parse_args()

    try:
        document = load_manifest(args.manifest)
        if args.command == "matrix":
            print(json.dumps(matrix(document)))
            return
        if args.command == "macos-matrix":
            print(json.dumps(macos_matrix(document)))
            return
        metadata = load_metadata(args.metadata)
        if args.command == "check":
            print("\n".join(check(document, metadata)))
        elif args.command == "cargo-args":
            print(shlex.join(cargo_args(document, metadata, args.partition)))
        else:
            print(shlex.join(build_args(document, metadata, args.partition)))
    except (OSError, json.JSONDecodeError, KeyError, subprocess.CalledProcessError, PartitionError) as error:
        print(f"linux test partitions: {error}", file=sys.stderr)
        raise SystemExit(1) from error


if __name__ == "__main__":
    main()
