#!/usr/bin/env python3
"""Behavioral tests for the Linux test partition coverage check."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT_PATH = ROOT / "scripts/linux-test-partitions.py"

def load_script():
    spec = importlib.util.spec_from_file_location("linux_test_partitions", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    # Dataclasses resolve their annotations through sys.modules.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module

def target(kind: str, name: str, *, test: bool = True, required: list[str] | None = None) -> dict:
    entry = {"kind": [kind], "name": name, "test": test}
    if required:
        entry["required-features"] = required
    return entry

def metadata() -> dict:
    return {
        "packages": [
            {
                "name": "root",
                "features": {"test-helpers": [], "test-transport": ["test-helpers", "store/test-transport"]},
                "targets": [
                    target("lib", "root"),
                    target("test", "session_suite", required=["test-helpers"]),
                    target("test", "graph_suite", required=["test-helpers"]),
                    target("test", "transport_suite", required=["test-transport"]),
                    target("bench", "queries", test=False),
                    target("example", "fixture", test=False),
                ],
            },
            {
                "name": "cli",
                "targets": [target("bin", "cli"), target("test", "cli_suite")],
            },
            {
                "name": "store",
                "targets": [
                    target("lib", "store"),
                    target("test", "durability", required=["test-helpers"]),
                ],
            },
        ]
    }

LINUX_GROUPS = [
    {"name": "root", "timeout_minutes": 105},
    {"name": "store", "timeout_minutes": 30},
]

MACOS_GROUPS = [
    {"name": "root", "timeout_minutes": 90, "budget_basis": "root-lib 60 + root-suites 45, x4/3"},
    {"name": "store", "timeout_minutes": 40},
]

def manifest(
    partitions: list[dict], not_run: dict | None = None, macos_groups: list[dict] | None = MACOS_GROUPS
) -> dict:
    document: dict = {"partitions": partitions, "linux_groups": LINUX_GROUPS}
    if not_run is not None:
        document["not_run"] = not_run
    if macos_groups is not None:
        document["macos_groups"] = macos_groups
    return document

COMPLETE = [
    {
        "name": "root-lib",
        "timeout_minutes": 60,
        "windows_timeout_minutes": 150,
        "macos_group": "root",
        "linux_group": "root",
        "packages": ["root"],
        "targets": ["lib"],
        "features": ["test-helpers"],
    },
    {
        "name": "root-suites",
        "timeout_minutes": 45,
        "windows_timeout_minutes": 110,
        "macos_group": "root",
        "linux_group": "root",
        "packages": ["root", "cli"],
        "targets": ["test:session_suite", "test:graph_suite", "bins", "test:cli_suite"],
        "features": ["root/test-helpers"],
    },
    {
        "name": "store",
        "timeout_minutes": 30,
        "windows_timeout_minutes": 75,
        "macos_group": "store",
        "linux_group": "store",
        "packages": ["store"],
        "features": ["store/test-helpers"],
    },
]
NOT_RUN = {"root::transport_suite": "test-transport suites are compile-checked only"}

class CoverageTest(unittest.TestCase):
    def setUp(self) -> None:
        self.script = load_script()

    def check(self, partitions: list[dict], not_run: dict | None = NOT_RUN) -> list[str]:
        return self.script.check(manifest(partitions, not_run), metadata())

    def test_complete_disjoint_partition_passes(self) -> None:
        lines = self.check(COMPLETE)
        self.assertEqual(lines[0], "root-lib: 1 test targets")
        self.assertEqual(lines[1], "root-suites: 4 test targets")
        self.assertEqual(lines[2], "store: 2 test targets")
        self.assertEqual(lines[3], "8 test targets: 7 in exactly one partition, 1 listed under not_run")
        self.assertEqual(lines[4], "macOS root: root-lib, root-suites")
        self.assertEqual(lines[5], "macOS store: store")
        self.assertEqual(lines[6], "3 partitions: each in exactly one of 2 macOS groups")
        self.assertEqual(lines[7:], [
            "Linux root: root-lib, root-suites",
            "Linux store: store",
            "3 partitions: each in exactly one of 2 Linux groups",
        ])

    def test_new_test_target_without_a_partition_fails(self) -> None:
        # A new crate (or a new suite in an existing crate) appears in cargo
        # metadata; nothing selects it, so the check must fail.
        partitions = [dict(COMPLETE[0]), dict(COMPLETE[1]), dict(COMPLETE[2])]
        partitions[2] = {**partitions[2], "targets": ["lib"]}
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(partitions)
        self.assertIn("store test `durability` is in no partition", str(caught.exception))

    def test_target_in_two_partitions_fails(self) -> None:
        partitions = [dict(p) for p in COMPLETE]
        partitions[0] = {**partitions[0], "targets": ["lib", "test:graph_suite"]}
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(partitions)
        self.assertIn("root test `graph_suite` is in 2 partitions: root-lib, root-suites", str(caught.exception))

    def test_required_features_the_partition_does_not_enable_do_not_count(self) -> None:
        # cargo skips a `required-features` target silently when the feature is
        # off, so selecting it by name without the feature leaves it unrun.
        partitions = [dict(p) for p in COMPLETE]
        partitions[2] = {"name": "store", "timeout_minutes": 30, "windows_timeout_minutes": 75, "packages": ["store"]}
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(partitions)
        self.assertIn("store test `durability` is in no partition", str(caught.exception))

    def test_not_run_entry_that_a_partition_runs_fails(self) -> None:
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(COMPLETE, {**NOT_RUN, "cli::cli_suite": "stale exemption"})
        self.assertIn("cli test `cli_suite` is listed under not_run but partition 'root-suites' runs it", str(caught.exception))

    def test_not_run_entry_for_unknown_target_fails(self) -> None:
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(COMPLETE, {**NOT_RUN, "root::removed_suite": "gone"})
        self.assertIn("not_run lists 'root::removed_suite'", str(caught.exception))

    def test_unknown_package_or_target_name_fails(self) -> None:
        with self.assertRaises(self.script.PartitionError):
            self.check([{**COMPLETE[0], "packages": ["missing"]}])
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check([{**COMPLETE[0], "targets": ["test:no_such_suite"]}])
        self.assertIn("no test target named 'no_such_suite'", str(caught.exception))

    def test_implied_features_satisfy_required_features(self) -> None:
        # `test-transport` implies `test-helpers` in the root's feature table,
        # so a partition enabling only the former still runs the latter's suites.
        partitions = [dict(p) for p in COMPLETE]
        partitions[1] = {**partitions[1], "features": ["root/test-transport"]}
        lines = self.check(partitions)
        self.assertEqual(lines[1], "root-suites: 4 test targets")

    def test_feature_of_an_unselected_package_fails(self) -> None:
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check([{**COMPLETE[2], "features": ["root/test-helpers"]}])
        self.assertIn("enables 'root/test-helpers' but does not select 'root'", str(caught.exception))

    def test_partition_that_runs_nothing_fails(self) -> None:
        partitions = [dict(p) for p in COMPLETE]
        partitions.append({"name": "empty", "timeout_minutes": 10, "windows_timeout_minutes": 25, "packages": ["root"], "targets": ["examples"]})
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(partitions)
        self.assertIn("partition 'empty' runs no test target", str(caught.exception))

    def test_cargo_args_follow_the_selection(self) -> None:
        args = self.script.cargo_args(manifest(COMPLETE, NOT_RUN), metadata(), "root-suites")
        self.assertEqual(
            args,
            [
                "-p", "root", "-p", "cli",
                "--test", "session_suite", "--test", "graph_suite", "--bins", "--test", "cli_suite",
                "--features", "root/test-helpers",
            ],
        )
        self.assertEqual(
            self.script.cargo_args(manifest(COMPLETE, NOT_RUN), metadata(), "store"),
            ["-p", "store", "--features", "store/test-helpers"],
        )
        with self.assertRaises(self.script.PartitionError):
            self.script.cargo_args(manifest(COMPLETE, NOT_RUN), metadata(), "nope")

    def test_build_args_add_the_spawned_executables_to_the_same_selection(self) -> None:
        partitions = [dict(p) for p in COMPLETE]
        partitions[1] = {**partitions[1], "executables": ["bins", "example:fixture"]}
        document = manifest(partitions, NOT_RUN)
        self.assertEqual(
            self.script.build_args(document, metadata(), "root-suites"),
            [
                "-p", "root", "-p", "cli",
                "--test", "session_suite", "--test", "graph_suite", "--bins", "--test", "cli_suite",
                "--bins", "--example", "fixture",
                "--features", "root/test-helpers",
            ],
        )
        # A partition whose tests spawn nothing has no build step.
        self.assertEqual(self.script.build_args(document, metadata(), "root-lib"), [])

    def test_executables_accept_only_bins_and_named_examples(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "partitions.json"
            path.write_text(
                json.dumps(manifest([{**COMPLETE[0], "executables": ["test:session_suite"]}])), encoding="utf-8"
            )
            with self.assertRaises(self.script.PartitionError) as caught:
                self.script.load_manifest(path)
            self.assertIn("executables take `bins`, `bin:<name>` or `example:<name>`", str(caught.exception))

    def test_macos_matrix_carries_groups_budgets_and_run_order(self) -> None:
        # The partitions of a group run in manifest order; the matrix entry
        # carries them as the space-separated list the job loops over.
        self.assertEqual(
            self.script.group_matrix(manifest(COMPLETE), "macos"),
            {
                "include": [
                    {"group": "root", "timeout": 90, "partitions": "root-lib root-suites"},
                    {"group": "store", "timeout": 40, "partitions": "store"},
                ]
            },
        )

    def test_linux_groups_and_windows_partitions_carry_each_host_budget(self) -> None:
        self.assertEqual(
            self.script.group_matrix(manifest(COMPLETE), "linux"),
            {"include": [
                {"group": "root", "timeout": 105, "partitions": "root-lib root-suites"},
                {"group": "store", "timeout": 30, "partitions": "store"},
            ]},
        )
        self.assertEqual(
            self.script.matrix(manifest(COMPLETE), "windows_timeout_minutes"),
            {"include": [
                {"partition": "root-lib", "timeout": 150},
                {"partition": "root-suites", "timeout": 110},
                {"partition": "store", "timeout": 75},
            ]},
        )

    def test_linux_group_validation(self) -> None:
        for declarations, error in (
            (None, "has no linux_groups"),
            ([*LINUX_GROUPS, LINUX_GROUPS[0]], "Linux group names repeat"),
            ([*LINUX_GROUPS, {"name": "idle", "timeout_minutes": 1}], "runs no partition"),
            ([{**LINUX_GROUPS[0], "timeout_minutes": 0}, LINUX_GROUPS[1]], "positive timeout"),
            ([{**LINUX_GROUPS[0], "timeout_minutes": True}, LINUX_GROUPS[1]], "positive timeout"),
            ([{**LINUX_GROUPS[0], "name": "../outside"}, LINUX_GROUPS[1]], "needs a name"),
        ):
            with self.subTest(declarations=declarations):
                document = {**manifest(COMPLETE), "linux_groups": declarations}
                with self.assertRaisesRegex(self.script.PartitionError, error):
                    self.script.groups(document, "linux")
        for replacement, error in ((None, "has no 'linux_group'"), ("unknown", "which is not listed")):
            partitions = [dict(p) for p in COMPLETE]
            if replacement is None:
                del partitions[0]["linux_group"]
            else:
                partitions[0]["linux_group"] = replacement
            with self.assertRaisesRegex(self.script.PartitionError, error):
                self.script.groups(manifest(partitions), "linux")

    def test_partition_without_a_macos_group_fails(self) -> None:
        # A new partition that no macOS job runs is the macOS analogue of a
        # target in no partition: the lane would pass without it.
        partitions = [dict(p) for p in COMPLETE]
        del partitions[2]["macos_group"]
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(partitions)
        self.assertIn("partition 'store' has no 'macos_group'", str(caught.exception))

    def test_partition_naming_an_unlisted_macos_group_fails(self) -> None:
        partitions = [dict(p) for p in COMPLETE]
        partitions[2] = {**partitions[2], "macos_group": "storage"}
        with self.assertRaises(self.script.PartitionError) as caught:
            self.check(partitions)
        self.assertIn("partition 'store' names macOS group 'storage', which is not listed", str(caught.exception))

    def test_macos_group_without_partitions_fails(self) -> None:
        # A listed group with no partitions would be a matrix job that runs
        # nothing and reports green.
        document = manifest(COMPLETE, NOT_RUN, [*MACOS_GROUPS, {"name": "idle", "timeout_minutes": 10}])
        with self.assertRaises(self.script.PartitionError) as caught:
            self.script.check(document, metadata())
        self.assertIn("macOS group 'idle' runs no partition", str(caught.exception))

    def test_more_macos_groups_than_the_concurrency_cap_fails(self) -> None:
        cap = self.script.MACOS_GROUP_CAP
        groups = [{"name": f"group-{index}", "timeout_minutes": 10} for index in range(cap + 1)]
        partitions = [
            {**COMPLETE[0], "name": f"part-{index}", "macos_group": f"group-{index}"} for index in range(cap + 1)
        ]
        with self.assertRaises(self.script.PartitionError) as caught:
            self.script.groups(manifest(partitions, NOT_RUN, groups), "macos")
        self.assertIn(f"{cap + 1} macOS groups exceed the {cap} concurrent macOS jobs", str(caught.exception))
        self.assertEqual(
            list(self.script.groups(manifest(partitions[:cap], NOT_RUN, groups[:cap]), "macos")),
            [f"group-{index}" for index in range(cap)],
        )

    def test_macos_group_validation(self) -> None:
        with self.assertRaises(self.script.PartitionError) as caught:
            self.script.groups(manifest(COMPLETE, NOT_RUN, None), "macos")
        self.assertIn("has no macos_groups", str(caught.exception))
        with self.assertRaises(self.script.PartitionError) as caught:
            self.script.groups(manifest(COMPLETE, NOT_RUN, [MACOS_GROUPS[0], {**MACOS_GROUPS[1], "name": "root"}]), "macos")
        self.assertIn("macOS group names repeat: 'root'", str(caught.exception))
        with self.assertRaises(self.script.PartitionError) as caught:
            self.script.groups(manifest(COMPLETE, NOT_RUN, [MACOS_GROUPS[0], {"name": "store"}]), "macos")
        self.assertIn("macOS group 'store' needs a positive timeout", str(caught.exception))
        with self.assertRaises(self.script.PartitionError) as caught:
            self.script.groups(manifest(COMPLETE, NOT_RUN, [MACOS_GROUPS[0], {"timeout_minutes": 40}]), "macos")
        self.assertIn("every macOS group needs a name", str(caught.exception))

    def test_manifest_validation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "partitions.json"
            path.write_text(json.dumps(manifest([{"name": "a", "packages": ["root"]}])), encoding="utf-8")
            with self.assertRaises(self.script.PartitionError) as caught:
                self.script.load_manifest(path)
            self.assertIn("has no 'timeout_minutes'", str(caught.exception))
            path.write_text(
                json.dumps(manifest([{"name": "a", "packages": ["root"], "timeout_minutes": 5}])), encoding="utf-8"
            )
            with self.assertRaises(self.script.PartitionError) as caught:
                self.script.load_manifest(path)
            self.assertIn("has no 'windows_timeout_minutes'", str(caught.exception))
            path.write_text(
                json.dumps(
                    manifest(
                        [
                            {
                                "name": "a",
                                "packages": ["root"],
                                "timeout_minutes": 5,
                                "windows_timeout_minutes": 0,
                                "macos_group": "root",
                            }
                        ]
                    )
                ),
                encoding="utf-8",
            )
            with self.assertRaises(self.script.PartitionError) as caught:
                self.script.load_manifest(path)
            self.assertIn("needs a positive windows_timeout_minutes", str(caught.exception))
            path.write_text(
                json.dumps(
                    manifest([{"name": "a", "packages": ["root"], "timeout_minutes": 5, "windows_timeout_minutes": 12}])
                ),
                encoding="utf-8",
            )
            with self.assertRaises(self.script.PartitionError) as caught:
                self.script.load_manifest(path)
            self.assertIn("has no 'macos_group'", str(caught.exception))
            path.write_text(
                json.dumps(manifest([COMPLETE[0], {**COMPLETE[2], "name": "root-lib"}])), encoding="utf-8"
            )
            with self.assertRaises(self.script.PartitionError):
                self.script.load_manifest(path)
            path.write_text(
                json.dumps(manifest(COMPLETE[:2], NOT_RUN)), encoding="utf-8"
            )
            with self.assertRaises(self.script.PartitionError) as caught:
                self.script.load_manifest(path)
            self.assertIn("macOS group 'store' runs no partition", str(caught.exception))

class CommandLineTest(unittest.TestCase):
    def test_linux_matrix_on_the_command_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "partitions.json"
            path.write_text(json.dumps(manifest(COMPLETE)), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT_PATH), "--manifest", str(path), "linux-matrix"],
                capture_output=True, text=True, check=True,
            )
            self.assertEqual(json.loads(result.stdout), {"include": [
                {"group": "root", "timeout": 105, "partitions": "root-lib root-suites"},
                {"group": "store", "timeout": 30, "partitions": "store"},
            ]})

    def test_group_runs_exact_commands_and_preserves_failures_and_reports(self) -> None:
        expected = [
            ["exec", "--", "cargo", "build", "--locked", "--profile", "perf",
             "-p", "root", "--lib", "--example", "fixture", "--features", "test-helpers"],
            ["exec", "--", "cargo", "nextest", "run", "--profile", "ci", "--cargo-profile", "perf", "--locked",
             "-p", "root", "--lib", "--features", "test-helpers", "--no-tests=fail"],
            ["exec", "--", "cargo", "build", "--locked", "--profile", "perf",
             "-p", "root", "-p", "cli", "--test", "session_suite", "--test", "graph_suite", "--bins",
             "--test", "cli_suite", "--bins", "--example", "fixture", "--features", "root/test-helpers"],
            ["exec", "--", "cargo", "nextest", "run", "--profile", "ci", "--cargo-profile", "perf", "--locked",
             "-p", "root", "-p", "cli", "--test", "session_suite", "--test", "graph_suite", "--bins",
             "--test", "cli_suite", "--features", "root/test-helpers", "--no-tests=fail"],
        ]
        for mode in ("success", "no-build", "build-failure", "test-failure", "missing-report", "report-move-failure"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "scripts").mkdir()
                script = root / "scripts/linux-test-partitions.py"
                shutil.copyfile(SCRIPT_PATH, script)
                partitions = [dict(p) for p in COMPLETE]
                if mode != "no-build":
                    partitions[0]["executables"] = ["example:fixture"]
                partitions[1]["executables"] = ["bins", "example:fixture"]
                manifest_path = root / "partitions.json"
                manifest_path.write_text(json.dumps(manifest(partitions, NOT_RUN)), encoding="utf-8")
                metadata_path = root / "metadata.json"
                metadata_path.write_text(json.dumps(metadata()), encoding="utf-8")
                commands = root / "commands.jsonl"
                bin_dir = root / "bin"
                bin_dir.mkdir()
                hauler = bin_dir / "hauler"
                hauler.write_text(f"#!{sys.executable}\n" + '''import json, os, sys
from pathlib import Path
args = sys.argv[1:]
assert args[:3] == ["exec", "--", "cargo"], args
with Path("commands.jsonl").open("a") as log:
    log.write(json.dumps(args) + "\\n")
os.execvp("cargo", args[2:])
''', encoding="utf-8")
                cargo = bin_dir / "cargo"
                cargo.write_text(f"#!{sys.executable}\n" + '''import os, sys
from pathlib import Path
args = sys.argv[1:]
first = "--lib" in args
mode = os.environ["PARTITION_TEST_MODE"]
if args[0] == "build":
    sys.exit(42 if first and mode == "build-failure" else 0)
assert args[:2] == ["nextest", "run"], args
if not (first and mode == "missing-report"):
    report = Path("target/nextest/ci/junit.xml")
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text('<testsuite name="' + ("lib" if first else "suites") + '"/>')
if first and mode == "report-move-failure":
    Path("target/nextest/linux/root-lib.xml").mkdir()
sys.exit(101 if first and mode == "test-failure" else 0)
''', encoding="utf-8")
                for executable in (hauler, cargo):
                    executable.chmod(0o755)
                source = root / "target/nextest/ci/junit.xml"
                source.parent.mkdir(parents=True)
                source.write_text("stale source", encoding="utf-8")
                output = root / "target/nextest/linux"
                output.mkdir()
                (output / "root-lib.xml").write_text("stale result", encoding="utf-8")
                result = subprocess.run(
                    [sys.executable, str(script), "--manifest", str(manifest_path), "--metadata", str(metadata_path),
                     "run-linux-group", "root"],
                    cwd=root, capture_output=True, text=True,
                    env={**os.environ, "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"], "PARTITION_TEST_MODE": mode},
                )
                self.assertEqual(result.returncode, 0 if mode in ("success", "no-build") else 1, result.stdout + result.stderr)
                actual = [json.loads(line) for line in commands.read_text().splitlines()]
                expected_commands = expected
                if mode == "build-failure":
                    expected_commands = expected[:1] + expected[2:]
                elif mode == "no-build":
                    expected_commands = expected[1:]
                self.assertEqual(actual, expected_commands)
                self.assertEqual((output / "root-suites.xml").read_text(), '<testsuite name="suites"/>')
                self.assertFalse(source.exists())
                timings = json.loads((output / "timings.json").read_text())
                self.assertEqual(timings["group"], "root")
                first, second = timings["partitions"]
                self.assertEqual([first["partition"], second["partition"]], ["root-lib", "root-suites"])
                self.assertEqual(second["test"]["exit_code"], 0)
                self.assertGreater(second["test"]["seconds"], 0)
                self.assertIsNone(second["error"])
                if mode == "no-build":
                    self.assertIsNone(first["build"])
                if mode == "build-failure":
                    self.assertEqual(first["build"]["exit_code"], 42)
                    self.assertIsNone(first["test"])
                    self.assertFalse((output / "root-lib.xml").exists())
                elif mode == "missing-report":
                    self.assertEqual(first["test"]["exit_code"], 0)
                    self.assertIn("without its JUnit report", first["error"])
                    self.assertFalse((output / "root-lib.xml").exists())
                elif mode == "report-move-failure":
                    self.assertEqual(first["test"]["exit_code"], 0)
                    self.assertIn("junit.xml' -> '", first["error"])
                    self.assertTrue(first["error"].endswith("root-lib.xml'"), first["error"])
                    self.assertIsNone(first["report"])
                else:
                    self.assertEqual(first["test"]["exit_code"], 101 if mode == "test-failure" else 0)
                    self.assertEqual((output / "root-lib.xml").read_text(), '<testsuite name="lib"/>')

    def test_check_fails_closed_on_the_command_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            metadata_path = Path(directory) / "metadata.json"
            metadata_path.write_text(json.dumps(metadata()), encoding="utf-8")
            manifest_path = Path(directory) / "partitions.json"
            manifest_path.write_text(json.dumps({**manifest(COMPLETE[:2], NOT_RUN, MACOS_GROUPS[:1]), "linux_groups": LINUX_GROUPS[:1]}), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT_PATH), "--manifest", str(manifest_path), "--metadata", str(metadata_path), "check"],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("store lib `store` is in no partition", result.stderr)

            manifest_path.write_text(json.dumps(manifest(COMPLETE, NOT_RUN)), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT_PATH), "--manifest", str(manifest_path), "--metadata", str(metadata_path), "cargo-args", "root-lib"],
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(result.stdout.strip(), "-p root --lib --features test-helpers")

            result = subprocess.run(
                [sys.executable, str(SCRIPT_PATH), "--manifest", str(manifest_path), "macos-matrix"],
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(
                json.loads(result.stdout),
                {
                    "include": [
                        {"group": "root", "timeout": 90, "partitions": "root-lib root-suites"},
                        {"group": "store", "timeout": 40, "partitions": "store"},
                    ]
                },
            )

            result = subprocess.run(
                [sys.executable, str(SCRIPT_PATH), "--manifest", str(manifest_path), "windows-matrix"],
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(
                json.loads(result.stdout),
                {
                    "include": [
                        {"partition": "root-lib", "timeout": 150},
                        {"partition": "root-suites", "timeout": 110},
                        {"partition": "store", "timeout": 75},
                    ]
                },
            )

    def test_repository_manifest_covers_the_workspace(self) -> None:
        # The real manifest against the real workspace: this is the assertion
        # that keeps a new crate from silently leaving the Linux lane.
        result = subprocess.run(
            [sys.executable, str(SCRIPT_PATH), "check"], capture_output=True, text=True, cwd=ROOT
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("in exactly one partition", result.stdout)

if __name__ == "__main__":
    unittest.main()
