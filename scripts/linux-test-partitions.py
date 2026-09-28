#!/usr/bin/env python3
"""Resolve Cargo test selections and prove complete, disjoint coverage.

    linux-test-partitions.py [--metadata FILE] check
    linux-test-partitions.py [--metadata FILE] cargo-args <partition>
    linux-test-partitions.py [--metadata FILE] build-args <partition>
    linux-test-partitions.py linux-matrix
    linux-test-partitions.py windows-matrix
    linux-test-partitions.py macos-matrix
    linux-test-partitions.py [--metadata FILE] run-linux-group <group>

`cargo-args` and `build-args` emit shell-quoted selections. The Linux runner
passes those same argument arrays through Hauler, preserves every partition's
JUnit report, and writes build/test durations and exit codes to timings.json.
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
import time
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
    if any(not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name) for name in names):
        raise PartitionError(f"{path}: every partition needs a name using letters, digits, underscores or hyphens")
    if len(set(names)) != len(names):
        raise PartitionError(f"{path}: partition names repeat")
    for partition in partitions:
        for key in ("packages", "timeout_minutes", "windows_timeout_minutes", "macos_group"):
            if key not in partition:
                raise PartitionError(f"partition {partition['name']!r} has no {key!r}")
        if not isinstance(partition["packages"], list) or not partition["packages"]:
            raise PartitionError(f"partition {partition['name']!r} selects no packages")
        for key in ("timeout_minutes", "windows_timeout_minutes"):
            if not isinstance(partition[key], int) or partition[key] <= 0:
                raise PartitionError(f"partition {partition['name']!r} needs a positive {key}")
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
    groups(document, "macos")
    groups(document, "linux")
    return document


def groups(document: dict[str, Any], platform: str) -> dict[str, list[str]]:
    label = {"linux": "Linux", "macos": "macOS"}[platform]
    declarations = document.get(f"{platform}_groups")
    if not isinstance(declarations, list) or not declarations:
        raise PartitionError(f"manifest has no {platform}_groups")
    members: dict[str, list[str]] = {}
    for group in declarations:
        name = group.get("name") if isinstance(group, dict) else None
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name):
            raise PartitionError(f"every {label} group needs a name using letters, digits, underscores or hyphens")
        if name in members:
            raise PartitionError(f"{label} group names repeat: {name!r}")
        timeout = group.get("timeout_minutes")
        if not isinstance(timeout, int) or isinstance(timeout, bool) or timeout <= 0:
            raise PartitionError(f"{label} group {name!r} needs a positive timeout")
        members[name] = []
    if platform == "macos" and len(members) > MACOS_GROUP_CAP:
        raise PartitionError(
            f"{len(members)} macOS groups exceed the {MACOS_GROUP_CAP} concurrent macOS jobs a run may take"
        )
    for partition in document["partitions"]:
        key = f"{platform}_group"
        if key not in partition:
            raise PartitionError(f"partition {partition['name']!r} has no {key!r}")
        group = partition[key]
        if not isinstance(group, str) or group not in members:
            raise PartitionError(
                f"partition {partition['name']!r} names {label} group {group!r}, "
                f"which is not listed under {platform}_groups"
            )
        members[group].append(partition["name"])
    for name, partitions in members.items():
        if not partitions:
            raise PartitionError(f"{label} group {name!r} runs no partition")
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
    for platform, label in (("macos", "macOS"), ("linux", "Linux")):
        members = groups(document, platform)
        for group, partitions in members.items():
            lines.append(f"{label} {group}: {', '.join(partitions)}")
        lines.append(
            f"{len(document['partitions'])} partitions: each in exactly one of {len(members)} {label} groups"
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


def matrix(document: dict[str, Any], timeout_key: str) -> dict[str, Any]:
    """One matrix entry per partition; `timeout_key` selects the host's budget."""
    return {
        "include": [
            {"partition": partition["name"], "timeout": partition[timeout_key]}
            for partition in document["partitions"]
        ]
    }


def group_matrix(document: dict[str, Any], platform: str) -> dict[str, Any]:
    members = groups(document, platform)
    return {
        "include": [
            {
                "group": group["name"],
                "timeout": group["timeout_minutes"],
                "partitions": " ".join(members[group["name"]]),
            }
            for group in document[f"{platform}_groups"]
        ]
    }


def run_linux_group(document: dict[str, Any], metadata: dict[str, Any], name: str) -> int:
    members = groups(document, "linux")
    if name not in members:
        raise PartitionError(f"no Linux group named {name!r}")
    check(document, metadata)
    output = ROOT / "target/nextest/linux"
    output.mkdir(parents=True, exist_ok=True)
    source = ROOT / "target/nextest/ci/junit.xml"
    results: dict[str, Any] = {"group": name, "partitions": []}
    failed = False
    for partition in members[name]:
        result: dict[str, Any] = {"partition": partition, "build": None, "test": None, "report": None, "error": None}
        print(f"::group::Test partition {partition}", flush=True)
        try:
            source.unlink(missing_ok=True)
            destination = output / f"{partition}.xml"
            destination.unlink(missing_ok=True)
            build = build_args(document, metadata, partition)
            commands = []
            if build:
                commands.append(("build", ["build", "--locked", "--profile", "perf", *build]))
            commands.append(("test", [
                "nextest", "run", "--profile", "ci", "--cargo-profile", "perf", "--locked",
                *cargo_args(document, metadata, partition), "--no-tests=fail",
            ]))
            for stage, args in commands:
                started = time.monotonic()
                completed = subprocess.run(["hauler", "exec", "--", "cargo", *args], cwd=ROOT)
                result[stage] = {"seconds": round(time.monotonic() - started, 3), "exit_code": completed.returncode}
                if completed.returncode:
                    failed = True
                    break
            if source.exists():
                source.replace(destination)
                result["report"] = destination.relative_to(ROOT).as_posix()
            elif result["test"] is not None and result["test"]["exit_code"] == 0:
                raise PartitionError(f"{partition}: nextest succeeded without its JUnit report")
        except (OSError, PartitionError) as error:
            result["error"] = str(error)
            failed = True
            print(f"::error::{error}", flush=True)
        results["partitions"].append(result)
        (output / "timings.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result), flush=True)
        print("::endgroup::", flush=True)
    return int(failed)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--manifest", type=Path, default=MANIFEST_PATH)
    parser.add_argument("--metadata", type=Path, help="pre-recorded `cargo metadata` JSON")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check")
    for command in ("cargo-args", "build-args"):
        commands.add_parser(command).add_argument("partition")
    commands.add_parser("linux-matrix")
    commands.add_parser("run-linux-group").add_argument("group")
    commands.add_parser("windows-matrix")
    commands.add_parser("macos-matrix")
    args = parser.parse_args()

    try:
        document = load_manifest(args.manifest)
        if args.command == "windows-matrix":
            print(json.dumps(matrix(document, "windows_timeout_minutes")))
            return
        if args.command in ("linux-matrix", "macos-matrix"):
            print(json.dumps(group_matrix(document, args.command.removesuffix("-matrix"))))
            return
        metadata = load_metadata(args.metadata)
        if args.command == "run-linux-group":
            raise SystemExit(run_linux_group(document, metadata, args.group))
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
