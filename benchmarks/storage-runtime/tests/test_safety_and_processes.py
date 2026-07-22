"""Unit tests for the S0 storage-runtime baseline runner.

Stdlib unittest only; no live daemon, no live profile, no product fixtures.
Run from this directory:

    python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import run_storage_baseline as rsb  # noqa: E402
import profile_safety  # noqa: E402



class GuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="s0-guard-")
        self.root = Path(self.tmp.name)
        self.home = self.root / "home"
        self.live = self.home / ".tracedecay"
        self.live.mkdir(parents=True)

    def tearDown(self):
        self.tmp.cleanup()

    def forbidden(self, env=None):
        return rsb.forbidden_profile_roots(env or {}, self.home)

    def test_default_profile_location_detected(self):
        roots = self.forbidden()
        labels = [label for label, _ in roots]
        self.assertIn("default profile ~/.tracedecay", labels)

    def test_env_override_locations_detected(self):
        env = {
            "TRACEDECAY_DATA_DIR": str(self.root / "custom-profile"),
            "TRACEDECAY_GLOBAL_DB": str(self.root / "elsewhere" / "global.db"),
        }
        roots = self.forbidden(env)
        labels = [label for label, _ in roots]
        self.assertIn("TRACEDECAY_DATA_DIR", labels)
        self.assertIn("TRACEDECAY_GLOBAL_DB parent", labels)

    def test_refuses_live_profile_path(self):
        with self.assertRaises(rsb.SafetyError):
            rsb.guard_path(self.live, "input", self.forbidden())

    def test_refuses_path_inside_live_profile(self):
        with self.assertRaises(rsb.SafetyError):
            rsb.guard_path(self.live / "nested" / "deeper", "input", self.forbidden())

    def test_refuses_path_containing_live_profile(self):
        with self.assertRaises(rsb.SafetyError):
            rsb.guard_path(self.home, "output", self.forbidden())

    def test_refuses_symlink_alias(self):
        alias = self.root / "alias"
        try:
            alias.symlink_to(self.live)
        except OSError:
            self.skipTest("symlinks unsupported on this platform")
        with self.assertRaises(rsb.SafetyError):
            rsb.guard_path(alias, "input", self.forbidden())

    def test_allows_isolated_paths(self):
        candidate = self.root / "isolated" / "input"
        resolved = rsb.guard_path(candidate, "input", self.forbidden())
        self.assertTrue(str(resolved).endswith(os.path.join("isolated", "input")))

    def test_output_must_be_empty(self):
        out = self.root / "out"
        out.mkdir()
        (out / "stale.txt").write_text("stale")
        with self.assertRaises(rsb.SafetyError):
            rsb.prepare_output_dir(out, self.forbidden())

    def test_output_must_not_already_exist_even_when_empty(self):
        out = self.root / "empty-out"
        out.mkdir()
        with self.assertRaises(rsb.SafetyError):
            rsb.prepare_output_dir(out, self.forbidden())

    def test_output_created_when_missing(self):
        out = self.root / "fresh" / "out"
        resolved = rsb.prepare_output_dir(out, self.forbidden())
        self.assertTrue(resolved.is_dir())


class ChildEnvTests(unittest.TestCase):
    def test_tracedecay_vars_scrubbed(self):
        base = {
            "PATH": "/usr/bin",
            "TRACEDECAY_DATA_DIR": "/live/profile",
            "TRACEDECAY_GLOBAL_DB": "/live/profile/global.db",
            "NEXTEST_TEST_NAME": "some-test",
        }
        env = rsb.build_child_env(base, {}, [], [])
        self.assertEqual(env, {"PATH": "/usr/bin"})

    def test_declared_env_cannot_override_runner_roots(self):
        with self.assertRaises(rsb.ConfigError):
            rsb.build_child_env({}, {"TRACEDECAY_DATA_DIR": "/isolated/copy"}, [], [])

    def test_windows_case_variants_are_scrubbed_and_isolated(self):
        with tempfile.TemporaryDirectory(prefix="s0-env-") as tmp:
            run_dir = Path(tmp) / "run"
            run_dir.mkdir()
            sandbox = rsb.create_child_sandbox(run_dir)
            env = rsb.build_child_env(
                {
                    "Path": "C:\\Windows",
                    "tracedecay_data_dir": "C:\\live",
                    "nExTeSt_TeSt_NaMe": "leak",
                    "HOME": "C:\\live-home",
                },
                {},
                [],
                [],
                sandbox,
                windows=True,
            )
            self.assertNotIn("tracedecay_data_dir", env)
            self.assertNotIn("nExTeSt_TeSt_NaMe", env)
            self.assertEqual(env["HOME"], str(sandbox["home"]))
            self.assertEqual(env["TRACEDECAY_DATA_DIR"], str(sandbox["data"]))
            self.assertEqual(env["TMPDIR"], str(sandbox["temp"]))
            self.assertEqual(env["SQLITE_TMPDIR"], str(sandbox["temp"]))

    def test_declared_path_must_stay_in_runner_sandbox(self):
        with tempfile.TemporaryDirectory(prefix="s0-env-") as tmp:
            root = Path(tmp)
            run_dir = root / "run"
            run_dir.mkdir()
            sandbox = rsb.create_child_sandbox(run_dir)
            with self.assertRaises(rsb.SafetyError):
                rsb.build_child_env(
                    {},
                    {"CUSTOM_PATH": str(root / "outside")},
                    ["CUSTOM_PATH"],
                    [],
                    sandbox,
                )

    def test_declared_path_values_guarded(self):
        with tempfile.TemporaryDirectory(prefix="s0-env-") as tmp:
            home = Path(tmp) / "home"
            live = home / ".tracedecay"
            live.mkdir(parents=True)
            forbidden = rsb.forbidden_profile_roots({}, home)
            with self.assertRaises(rsb.SafetyError):
                rsb.build_child_env(
                    {},
                    {"TRACEDECAY_DATA_DIR": str(live)},
                    ["TRACEDECAY_DATA_DIR"],
                    forbidden,
                )


class RecursiveSafetyTests(unittest.TestCase):
    def test_recursive_tree_rejects_symlink(self):
        with tempfile.TemporaryDirectory(prefix="s0-tree-") as tmp:
            root = Path(tmp)
            tree = root / "tree"
            tree.mkdir()
            target = root / "outside.txt"
            target.write_text("outside")
            try:
                (tree / "link").symlink_to(target)
            except OSError:
                self.skipTest("symlinks unsupported on this platform")
            with self.assertRaises(rsb.SafetyError):
                rsb.validate_safe_tree(tree, "test tree")

    def test_recursive_tree_rejects_hardlinked_regular_file(self):
        with tempfile.TemporaryDirectory(prefix="s0-tree-") as tmp:
            root = Path(tmp)
            tree = root / "tree"
            tree.mkdir()
            original = root / "original.txt"
            original.write_text("bytes")
            try:
                os.link(original, tree / "alias.txt")
            except OSError:
                self.skipTest("hardlinks unsupported on this filesystem")
            with self.assertRaises(rsb.SafetyError):
                rsb.validate_safe_tree(tree, "test tree")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO creation unavailable")
    def test_recursive_tree_rejects_special_file(self):
        with tempfile.TemporaryDirectory(prefix="s0-tree-") as tmp:
            tree = Path(tmp) / "tree"
            tree.mkdir()
            fifo = tree / "pipe"
            try:
                os.mkfifo(fifo)
            except OSError as exc:
                self.skipTest(f"FIFO creation unavailable: {exc}")
            with self.assertRaises(rsb.SafetyError):
                rsb.validate_safe_tree(tree, "test tree")

    def test_safe_copy_is_independent_runner_owned_tree(self):
        with tempfile.TemporaryDirectory(prefix="s0-tree-") as tmp:
            root = Path(tmp)
            source = root / "source"
            source.mkdir()
            (source / "state.txt").write_text("before")
            copied = rsb.copy_safe_tree(source, root / "copy", "test")
            (copied / "state.txt").write_text("after")
            self.assertEqual((source / "state.txt").read_text(), "before")
            self.assertEqual((copied / "state.txt").read_text(), "after")

    def test_expanded_path_cannot_escape_runner_root(self):
        with tempfile.TemporaryDirectory(prefix="s0-tree-") as tmp:
            root = Path(tmp) / "run"
            root.mkdir()
            with self.assertRaises(rsb.SafetyError):
                rsb.substitute_argv(
                    ["__RUN_DIR__/../escape"],
                    {"RUN_DIR": str(root)},
                    {"RUN_DIR": root},
                )

    def test_binary_placeholder_cannot_be_modified(self):
        with tempfile.TemporaryDirectory(prefix="s0-tree-") as tmp:
            root = Path(tmp)
            binary = root / "binary"
            binary.write_text("binary")
            with self.assertRaises(rsb.SafetyError):
                rsb.substitute_argv(
                    ["__PRODUCT_BINARY__/escape"],
                    {"PRODUCT_BINARY": str(binary)},
                    {"PRODUCT_BINARY": root},
                )

    def test_atomic_output_cannot_be_replaced(self):
        with tempfile.TemporaryDirectory(prefix="s0-output-") as tmp:
            path = Path(tmp) / "artifact.json"
            rsb.atomic_write_new(path, "first\n", "test")
            self.assertEqual(path.read_text(), "first\n")
            with self.assertRaises(rsb.SafetyError):
                rsb.atomic_write_new(path, "second\n", "test")

    def test_canonical_json_hash_and_bounded_read_helpers(self):
        document = {"z": "snowman ☃", "a": [1, 2]}
        encoded = '{"a":[1,2],"z":"snowman ☃"}'.encode("utf-8")
        self.assertEqual(rsb.canonical_compact_json(document).encode("utf-8"), encoded)
        self.assertEqual(rsb.sha256_bytes(encoded), hashlib.sha256(encoded).hexdigest())
        with tempfile.TemporaryDirectory(prefix="s0-json-") as tmp:
            root = Path(tmp)
            source = root / "source.json"
            source.write_bytes(encoded)
            self.assertEqual(
                rsb.read_file_no_follow(source, "test", max_bytes=len(encoded)), encoded
            )
            with self.assertRaises(rsb.SafetyError):
                rsb.read_file_no_follow(source, "test", max_bytes=len(encoded) - 1)
            output = root / "artifact.json"
            rsb.atomic_write_json_new(output, document, "test")
            self.assertEqual(output.read_bytes(), encoded + b"\n")
            with self.assertRaises(rsb.SafetyError):
                rsb.atomic_write_json_new(output, document, "test")

    def test_input_and_output_must_be_disjoint(self):
        with tempfile.TemporaryDirectory(prefix="s0-output-") as tmp:
            root = Path(tmp)
            input_root = root / "input"
            input_root.mkdir()
            with self.assertRaises(rsb.SafetyError):
                rsb.require_disjoint_roots(input_root, input_root / "output")

    def test_detected_network_filesystem_fails_closed(self):
        with tempfile.TemporaryDirectory(prefix="s0-mount-") as tmp:
            root = Path(tmp)
            with mock.patch.object(rsb.sys, "platform", "linux"), mock.patch.object(
                profile_safety, "_linux_mounts", return_value=[(root, "nfs")]
            ):
                with self.assertRaises(rsb.SafetyError):
                    rsb.reject_network_filesystem(root / "child", "test")

    @unittest.skipUnless(os.name == "posix", "child symlink fixture is POSIX specific")
    def test_child_created_symlink_prevents_result_publication(self):
        with tempfile.TemporaryDirectory(prefix="s0-output-") as tmp:
            root = Path(tmp)
            input_root = root / "input"
            input_root.mkdir()
            (input_root / "seed").write_text("seed")
            workload = root / "workload.json"
            workload.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "workload_id": "unsafe-output",
                        "store_families": ["sample"],
                        "phases": [
                            {
                                "name": "current",
                                "kind": "closed_loop",
                                "families": ["sample"],
                                "warmup": 0,
                                "repetitions": 1,
                                "work": {
                                    "argv": [
                                        "__PYTHON__",
                                        "-c",
                                        "import os,sys; os.symlink(sys.argv[1], sys.argv[2])",
                                        "__RUN_DIR__/target",
                                        "__RUN_DIR__/unsafe",
                                    ]
                                },
                            }
                        ],
                    }
                )
            )
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(workload),
                    "--input",
                    str(input_root),
                    "--output",
                    str(root / "output"),
                ]
            )
            self.assertEqual(rc, 2)
            self.assertFalse((root / "output" / "storage-runtime-baseline-result.json").exists())


class PlatformNormalizationTests(unittest.TestCase):
    def test_platform_aliases_are_normalized(self):
        self.assertEqual(rsb.normalized_platform_name("Darwin"), "macos")
        self.assertEqual(rsb.normalized_platform_name("win32"), "windows")
        self.assertEqual(rsb.normalized_platform_name("Linux"), "linux")


class ProcessTreeTests(unittest.TestCase):
    def test_windows_capability_is_explicitly_best_effort_without_job_object(self):
        capability = rsb.process_tree_capability("windows")
        self.assertEqual(capability["state"], "supported_best_effort")
        self.assertEqual(capability["mechanism"], "psutil_recursive_no_job_object")
        self.assertEqual(capability["child_process_coverage_complete"], "false")
        self.assertIn("Windows Job Object", capability["limitation"])

    @unittest.skipUnless(os.name == "posix", "process-group test is POSIX specific")
    def test_timeout_kills_process_group_and_verifies_descendants(self):
        result = rsb.run_command(
            [
                sys.executable,
                "-c",
                (
                    "import subprocess,sys,time; "
                    "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)']); "
                    "time.sleep(30)"
                ),
            ],
            {},
            0.05,
        )
        self.assertTrue(result["timed_out"])
        self.assertEqual(result["process_tree"]["clean"], "true")
