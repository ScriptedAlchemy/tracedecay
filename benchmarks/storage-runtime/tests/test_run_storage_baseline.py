"""Unit tests for the S0 storage-runtime baseline runner.

Stdlib unittest only; no live daemon, no live profile, no product fixtures.
Run from this directory:

    python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

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


class PercentileTests(unittest.TestCase):
    def test_nearest_rank_known_values(self):
        samples = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100]
        summary = rsb.summarize_latency(samples)
        self.assertEqual(summary["count"], 10)
        self.assertEqual(summary["min_ns"], 10)
        self.assertEqual(summary["p50_ns"], 50)
        self.assertEqual(summary["p95_ns"], 100)
        self.assertEqual(summary["p99_ns"], 100)
        self.assertEqual(summary["max_ns"], 100)
        self.assertEqual(summary["percentile_method"], "nearest_rank")

    def test_empty_samples(self):
        summary = rsb.summarize_latency([])
        self.assertEqual(summary["count"], 0)
        self.assertIsNone(summary["p50_ns"])

    def test_single_sample_zero_stddev(self):
        summary = rsb.summarize_latency([42])
        self.assertEqual(summary["p50_ns"], 42)
        self.assertEqual(summary["sample_stddev_ns"], 0.0)


class CountsInvariantTests(unittest.TestCase):
    def test_consistent_counts_pass(self):
        counts = rsb.new_counts()
        counts.update({"offered": 10, "admitted": 9, "completed": 8, "failed": 1})
        counts["shed"]["runner_in_flight_cap"] = 1
        self.assertEqual(rsb.counts_invariants_ok(counts), [])

    def test_offered_mismatch_detected(self):
        counts = rsb.new_counts()
        counts.update({"offered": 10, "admitted": 10, "completed": 10})
        counts["shed"]["runner_in_flight_cap"] = 1
        problems = rsb.counts_invariants_ok(counts)
        self.assertTrue(any("offered" in p for p in problems))

    def test_admitted_mismatch_detected(self):
        counts = rsb.new_counts()
        counts.update({"offered": 10, "admitted": 10, "completed": 8, "failed": 1})
        problems = rsb.counts_invariants_ok(counts)
        self.assertTrue(any("admitted" in p for p in problems))


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


class SubstitutionTests(unittest.TestCase):
    def test_tokens_replaced(self):
        argv = rsb.substitute_argv(
            ["__INPUT__/store.db", "literal", "__REPETITION__"],
            {"INPUT": "/in", "REPETITION": "7"},
        )
        self.assertEqual(argv, ["/in/store.db", "literal", "7"])


class FingerprintTests(unittest.TestCase):
    def test_deterministic_and_path_independent(self):
        with tempfile.TemporaryDirectory(prefix="s0-fp-") as tmp:
            root_a = Path(tmp) / "a"
            root_b = Path(tmp) / "b"
            for root in (root_a, root_b):
                root.mkdir()
                (root / "x.txt").write_text("one")
                sub = root / "sub"
                sub.mkdir()
                (sub / "y.txt").write_text("two")
            fp_a = rsb.fingerprint_tree(root_a)
            fp_b = rsb.fingerprint_tree(root_b)
            self.assertEqual(fp_a["aggregate_sha256"], fp_b["aggregate_sha256"])
            self.assertEqual(fp_a["file_count"], 2)


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
                    ["__BINARY__/escape"],
                    {"BINARY": str(binary)},
                    {"BINARY": root},
                )

    def test_atomic_output_cannot_be_replaced(self):
        with tempfile.TemporaryDirectory(prefix="s0-output-") as tmp:
            path = Path(tmp) / "artifact.json"
            rsb.atomic_write_new(path, "first\n", "test")
            self.assertEqual(path.read_text(), "first\n")
            with self.assertRaises(rsb.SafetyError):
                rsb.atomic_write_new(path, "second\n", "test")

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
                rsb, "_linux_mounts", return_value=[(root, "nfs")]
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
    def test_windows_capability_is_explicitly_unsupported(self):
        capability = rsb.process_tree_capability("windows")
        self.assertEqual(capability["state"], "unsupported_no_safe_stdlib_job_object")
        self.assertEqual(capability["descendant_verification"], "unsupported")

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


class LogicalSqliteEvidenceTests(unittest.TestCase):
    def test_logical_sqlite_evidence_redacts_rows_and_schema_text(self):
        with tempfile.TemporaryDirectory(prefix="s0-sqlite-") as tmp:
            database = Path(tmp) / "store.sqlite"
            connection = sqlite3.connect(database)
            connection.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, secret TEXT)")
            connection.execute("INSERT INTO items (secret) VALUES ('token-alpha')")
            connection.commit()
            connection.close()
            evidence = rsb.capture_logical_sqlite_evidence(database, {"tables": ["items"]})
            self.assertEqual(evidence["schema"], rsb.LOGICAL_SQLITE_EVIDENCE_SCHEMA)
            self.assertEqual(evidence["integrity"]["status"], "ok")
            self.assertEqual(evidence["tables"], [{"table_id": "items", "row_count": 1}])
            self.assertNotIn("token-alpha", json.dumps(evidence))
            self.assertNotIn("CREATE TABLE", json.dumps(evidence))

    def test_fts_rank_and_snippet_are_hashed_not_published(self):
        with tempfile.TemporaryDirectory(prefix="s0-sqlite-") as tmp:
            database = Path(tmp) / "store.sqlite"
            connection = sqlite3.connect(database)
            try:
                connection.execute("CREATE VIRTUAL TABLE docs USING fts5(body)")
            except sqlite3.OperationalError as exc:
                self.skipTest(f"SQLite FTS5 unavailable: {exc}")
            connection.execute("INSERT INTO docs (body) VALUES ('token-alpha token-beta')")
            connection.commit()
            connection.close()
            evidence = rsb.capture_logical_sqlite_evidence(
                database,
                {
                    "fts_probes": [
                        {
                            "name": "query",
                            "table": "docs",
                            "query": "token",
                            "projection": "rowid_rank_snippet",
                        }
                    ]
                },
            )
            probe = evidence["fts"][0]
            self.assertEqual(probe["projection"], "rowid_rank_snippet")
            self.assertEqual(len(probe["result_sha256"]), 64)
            self.assertNotIn("token-alpha", json.dumps(evidence))


class WorkloadValidationTests(unittest.TestCase):
    def _write(self, root: Path, doc: dict) -> Path:
        path = root / "workload.json"
        path.write_text(json.dumps(doc))
        return path

    def _minimal(self) -> dict:
        return {
            "schema_version": 1,
            "workload_id": "t",
            "store_families": ["graph"],
            "phases": [
                {
                    "name": "current",
                    "kind": "closed_loop",
                    "families": ["graph"],
                    "work": {"argv": ["true"]},
                }
            ],
        }

    def test_minimal_workload_loads(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            workload = rsb.load_workload(self._write(Path(tmp), self._minimal()))
            self.assertEqual(workload["workload_id"], "t")

    def test_wrong_schema_version_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["schema_version"] = 99
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_case_insensitive_family_collision_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["store_families"] = ["graph", "GRAPH"]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_windows_reserved_identifier_rejected(self):
        with self.assertRaises(rsb.ConfigError):
            rsb.require_safe_identifier("CON.json", "test identifier")

    def test_malformed_argv_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["phases"][0]["work"]["argv"] = "true"
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_product_evidence_rejects_synthetic_logical_file(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["evidence_eligible"] = True
            doc["phases"][0]["evidence"] = [
                {"name": "state", "capture": "logical_file", "path": "__RUN_DIR__/state"}
            ]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_unknown_family_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["phases"][0]["families"] = ["nope"]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_recovery_requires_depends_on(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["phases"] = [
                {"name": "recovery", "kind": "recovery", "families": ["graph"]}
            ]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_null_argv_is_pending(self):
        phase = {"name": "p", "kind": "closed_loop", "work": {"argv": None}}
        self.assertIsNotNone(rsb.phase_pending_reason(phase))

    def test_concrete_argv_not_pending(self):
        phase = {"name": "p", "kind": "closed_loop", "work": {"argv": ["true"]}}
        self.assertIsNone(rsb.phase_pending_reason(phase))


class CheckedInWorkloadTests(unittest.TestCase):
    HERE = Path(__file__).resolve().parent.parent

    def test_s0_workload_parses_and_is_fully_pending(self):
        workload = rsb.load_workload(self.HERE / "workload-s0.json")
        self.assertEqual(workload["workload_id"], "storage-runtime-s0-baseline-v1")
        self.assertTrue(workload["evidence_eligible"])
        self.assertEqual(
            workload["store_families"], ["graph", "profile", "project", "session"]
        )
        names = [phase["name"] for phase in workload["phases"]]
        self.assertEqual(
            names,
            [
                "current",
                "ten_x",
                "overload",
                "crash",
                "recovery",
                "fts",
                "backup_restore",
                "aa_noise",
            ],
        )
        for phase in workload["phases"]:
            if phase["kind"] == "aa_pairs":
                continue
            self.assertIsNotNone(
                rsb.phase_pending_reason(phase),
                f"S0 phase {phase['name']!r} must remain pending until wired",
            )

    def test_s0_pending_reasons_name_executable_prerequisites(self):
        workload = rsb.load_workload(self.HERE / "workload-s0.json")
        reasons = {
            phase["name"]: rsb.phase_pending_reason(phase) or ""
            for phase in workload["phases"]
            if phase["kind"] != "aa_pairs"
        }
        self.assertIn("explicit fixture paths", reasons["current"])
        self.assertIn("documented saturation outcomes", reasons["overload"])
        self.assertIn("ready file before SIGKILL", reasons["crash"])
        self.assertIn("reopen/integrity command", reasons["recovery"])
        self.assertIn("storage-runtime-fixture-v1.json", reasons["fts"])
        self.assertIn(
            "backup, manifest-verification, and restore", reasons["backup_restore"]
        )

    def test_dry_run_workload_has_no_pending_phases(self):
        workload = rsb.load_workload(self.HERE / "workload-dry-run.json")
        self.assertFalse(workload["evidence_eligible"])
        for phase in workload["phases"]:
            self.assertIsNone(
                rsb.phase_pending_reason(phase),
                f"dry-run phase {phase['name']!r} must be executable",
            )


class FreezeTests(unittest.TestCase):
    def test_freeze_captures_hashes_without_paths(self):
        with tempfile.TemporaryDirectory(prefix="s0-freeze-") as tmp:
            root = Path(tmp)
            binary = root / "fake-binary"
            binary.write_text("#!/bin/sh\nexit 0\n")
            schema = root / "schema.sql"
            schema.write_text("-- operator supplied released schema export\n")
            workload = root / "workload.json"
            workload.write_text('{"schema_version": 1}\n')
            corpus = root / "corpus"
            corpus.mkdir()
            (corpus / "fixture.txt").write_text("fixture")
            config = root / "config.toml"
            config.write_text("mode = 'isolated'\n")
            out = root / "frozen-identity.json"
            rc = rsb.main(
                [
                    "freeze",
                    "--binary",
                    str(binary),
                    "--binary-version-argv",
                    "--schema-manifest",
                    str(schema),
                    "--workload",
                    str(workload),
                    "--corpus",
                    str(corpus),
                    "--config",
                    str(config),
                    "--output",
                    str(out),
                ]
            )
            self.assertEqual(rc, 0)
            identity = json.loads(out.read_text())
            self.assertEqual(
                identity["artifact_id"], "storage-runtime-frozen-identity-v2"
            )
            self.assertEqual(identity["binary"]["basename"], "fake-binary")
            self.assertNotIn(str(root), json.dumps(identity["binary"]))
            self.assertEqual(len(identity["binary"]["sha256"]), 64)
            self.assertEqual(len(identity["schema_manifest"]["sha256"]), 64)
            self.assertEqual(len(identity["workload"]["sha256"]), 64)
            self.assertEqual(identity["corpus"]["kind"], "tree")
            self.assertEqual(len(identity["config"]["sha256"]), 64)


class FrozenIdentityBindingTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "workload execution needs POSIX tree verification")
    def test_bound_synthetic_run_remains_not_evidence(self):
        with tempfile.TemporaryDirectory(prefix="s0-binding-") as tmp:
            root = Path(tmp)
            corpus = root / "corpus"
            corpus.mkdir()
            (corpus / "seed.txt").write_text("seed")
            binary = root / "binary"
            binary.write_text("not executed")
            schema = root / "schema.sql"
            schema.write_text("-- schema\n")
            config = root / "config.toml"
            config.write_text("mode = 'baseline'\n")
            workload = root / "workload.json"
            workload.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "workload_id": "bound-run",
                        "store_families": ["sample"],
                        "phases": [
                            {
                                "name": "current",
                                "kind": "closed_loop",
                                "families": ["sample"],
                                "warmup": 0,
                                "repetitions": 1,
                                "setup": {
                                    "argv": [
                                        "__PYTHON__",
                                        "-c",
                                        "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('seed')",
                                        "__RUN_DIR__/state.txt",
                                    ]
                                },
                                "work": {
                                    "argv": [
                                        "__PYTHON__",
                                        "-c",
                                        "import pathlib,sys; p=pathlib.Path(sys.argv[1]); p.write_text(p.read_text()+'x')",
                                        "__RUN_DIR__/state.txt",
                                    ]
                                },
                                "evidence": [
                                    {
                                        "name": "state",
                                        "capture": "logical_file",
                                        "path": "__RUN_DIR__/state.txt",
                                    }
                                ],
                            }
                        ],
                    }
                )
            )
            identity = root / "identity.json"
            self.assertEqual(
                rsb.main(
                    [
                        "freeze",
                        "--binary",
                        str(binary),
                        "--binary-version-argv",
                        "--schema-manifest",
                        str(schema),
                        "--workload",
                        str(workload),
                        "--corpus",
                        str(corpus),
                        "--config",
                        str(config),
                        "--output",
                        str(identity),
                    ]
                ),
                0,
            )
            output = root / "output"
            self.assertEqual(
                rsb.main(
                    [
                        "run",
                        "--workload",
                        str(workload),
                        "--input",
                        str(corpus),
                        "--output",
                        str(output),
                        "--frozen-identity",
                        str(identity),
                        "--binary",
                        str(binary),
                        "--schema-manifest",
                        str(schema),
                        "--config",
                        str(config),
                    ]
                ),
                0,
            )
            result = json.loads((output / "storage-runtime-baseline-result.json").read_text())
            self.assertEqual(result["status"], "not_evidence")
            self.assertEqual(result["evidence_status"]["state"], "not_evidence")
            self.assertIn(
                "workload is explicitly ineligible for product evidence",
                result["evidence_status"]["reasons"],
            )
            self.assertEqual(result["identity_binding"]["status"], "bound")
            tampered = json.loads(json.dumps(result))
            tampered["status"] = "completed"
            tampered["evidence_status"] = {"state": "evidence", "reasons": []}
            tampered["execution_scope"]["mode"] = "partial"
            self.assertTrue(
                any("partial" in problem for problem in rsb.validate_result(tampered))
            )
            current = result["runs"][0]
            self.assertTrue((output / current["run_dir"] / "store" / "seed.txt").is_file())
            self.assertEqual(
                (output / current["run_dir"] / "sandbox" / "config" / "config.toml").read_text(),
                "mode = 'baseline'\n",
            )
            self.assertEqual((corpus / "seed.txt").read_text(), "seed")

            config.write_text("mode = 'mutated'\n")
            identity_doc = json.loads(identity.read_text())
            with self.assertRaises(rsb.ConfigError):
                rsb.bind_frozen_identity(
                    identity_doc,
                    binary_path=binary,
                    schema_manifest_path=schema,
                    workload_path=workload,
                    corpus_root=corpus,
                    config_path=config,
                )


class ResultValidationTests(unittest.TestCase):
    def test_absolute_path_leak_detected(self):
        result = {"runs": [{"run_dir": "/abs/leak"}]}
        hits = rsb.result_contains_absolute_paths(result)
        self.assertTrue(hits)

    def test_relative_paths_pass(self):
        result = {"runs": [{"run_dir": "work/current/graph"}]}
        self.assertEqual(rsb.result_contains_absolute_paths(result), [])

    def test_windows_unc_path_leak_detected(self):
        result = {"runs": [{"run_dir": "\\\\server\\share\\leak"}]}
        self.assertTrue(rsb.result_contains_absolute_paths(result))

    def test_open_loop_ledger_must_bind_retries_to_requests(self):
        counts = rsb.new_counts()
        counts.update({"offered": 1, "admitted": 1, "completed": 1, "retried": 1})
        run = {
            "phase": "overload",
            "requests": [
                {
                    "request_id": 0,
                    "terminal": True,
                    "scheduled_at_ns": 0,
                    "admitted_at_ns": 1,
                    "started_at_ns": 2,
                    "terminal_at_ns": 3,
                    "outcome": "completed",
                    "attempts": 1,
                }
            ],
        }
        self.assertTrue(rsb.validate_open_loop_ledger(run, counts))


class EndToEndDryRunTests(unittest.TestCase):
    HERE = Path(__file__).resolve().parent.parent

    def test_dry_run_produces_valid_result(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            output_dir = root / "output"
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-dry-run.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(output_dir),
                ]
            )
            self.assertEqual(rc, 0)
            result = json.loads(
                (output_dir / "storage-runtime-baseline-result.json").read_text()
            )
            self.assertEqual(rsb.validate_result(result), [])
            self.assertEqual(rsb.result_contains_absolute_paths(result), [])
            self.assertEqual(result["status"], "not_evidence")
            self.assertEqual(result["evidence_status"]["state"], "not_evidence")
            phases = {run["phase"] for run in result["runs"]}
            self.assertTrue(
                {
                    "current",
                    "ten_x",
                    "overload",
                    "crash",
                    "recovery",
                    "fts",
                    "backup_restore",
                    "aa_noise",
                }
                <= phases
            )
            overload = next(
                run for run in result["runs"] if run["phase"] == "overload"
            )
            counts = overload["counts"]
            self.assertEqual(counts["offered"], 20)
            self.assertGreater(counts["completed"], 0)
            latency = overload["latency"]["response_ns"]
            self.assertIsNotNone(latency["p99_ns"])
            self.assertIn("schedule_lag_ns", overload["latency"])
            self.assertEqual(len(overload["requests"]), counts["offered"])
            self.assertTrue(all(request["terminal"] for request in overload["requests"]))
            self.assertGreater(counts["shed"]["runner_in_flight_cap"], 0)
            runner_shed = next(
                request
                for request in overload["requests"]
                if request["outcome"] == "shed_runner_in_flight_cap"
            )
            self.assertIsNone(runner_shed["admitted_at_ns"])
            self.assertIsNone(runner_shed["started_at_ns"])
            self.assertTrue(
                all(
                    key in request
                    for request in overload["requests"]
                    for key in (
                        "scheduled_at_ns",
                        "admitted_at_ns",
                        "started_at_ns",
                        "terminal_at_ns",
                        "outcome",
                    )
                )
            )
            fts = next(run for run in result["runs"] if run["phase"] == "fts")
            self.assertTrue(
                all(
                    evidence["output"]["redacted"]
                    for evidence in fts["evidence"].values()
                )
            )
            aa = next(
                run
                for run in result["runs"]
                if run["phase"] == "aa_noise" and "aa" in run
            )
            floor = aa["aa"]["noise_floor"]["p50_response_ns"]
            self.assertIsNotNone(floor["aa_noise_floor_relative"])
            self.assertIsNotNone(floor["regression_margin_relative"])

    def test_s0_workload_fails_closed_without_allow_pending(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-s0.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(root / "output"),
                ]
            )
            self.assertEqual(rc, 2)

    def test_only_output_is_explicitly_partial_not_evidence(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            output_dir = root / "output"
            self.assertEqual(
                rsb.main(
                    [
                        "run",
                        "--workload",
                        str(self.HERE / "workload-dry-run.json"),
                        "--input",
                        str(input_dir),
                        "--output",
                        str(output_dir),
                        "--only",
                        "current",
                    ]
                ),
                0,
            )
            result = json.loads((output_dir / "storage-runtime-baseline-result.json").read_text())
            self.assertEqual(result["status"], "not_evidence")
            self.assertEqual(result["execution_scope"]["mode"], "partial")
            self.assertIn("--only", " ".join(result["evidence_status"]["reasons"]))

    def test_failed_operations_return_nonzero_and_cannot_be_evidence(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            workload = json.loads((self.HERE / "workload-dry-run.json").read_text())
            workload["phases"] = [workload["phases"][0]]
            workload["phases"][0]["warmup"] = 0
            workload["phases"][0]["repetitions"] = 1
            workload["phases"][0]["work"] = {
                "argv": ["__PYTHON__", "-c", "import sys; sys.exit(1)"]
            }
            workload_path = root / "workload.json"
            workload_path.write_text(json.dumps(workload))
            output = root / "output"
            self.assertEqual(
                rsb.main(
                    [
                        "run",
                        "--workload",
                        str(workload_path),
                        "--input",
                        str(input_dir),
                        "--output",
                        str(output),
                    ]
                ),
                2,
            )
            result = json.loads((output / "storage-runtime-baseline-result.json").read_text())
            self.assertEqual(result["status"], "failed_validation")
            self.assertTrue(any("failed operations" in item for item in result["validation_problems"]))

    def test_s0_workload_records_pending_with_allow_pending_and_identity(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            binary = root / "fake-binary"
            binary.write_text("fake")
            config = root / "config.toml"
            config.write_text("mode = 'pending'\n")
            identity = root / "frozen-identity.json"
            self.assertEqual(
                rsb.main(
                    [
                        "freeze",
                        "--binary",
                        str(binary),
                        "--binary-version-argv",
                        "--schema-manifest",
                        str(binary),
                        "--workload",
                        str(self.HERE / "workload-s0.json"),
                        "--corpus",
                        str(input_dir),
                        "--config",
                        str(config),
                        "--output",
                        str(identity),
                    ]
                ),
                0,
            )
            output_dir = root / "output"
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-s0.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(output_dir),
                    "--frozen-identity",
                    str(identity),
                    "--binary",
                    str(binary),
                    "--schema-manifest",
                    str(binary),
                    "--config",
                    str(config),
                    "--allow-pending",
                ]
            )
            self.assertEqual(rc, 0)
            result = json.loads(
                (output_dir / "storage-runtime-baseline-result.json").read_text()
            )
            self.assertTrue(
                all(run["status"] == "pending" for run in result["runs"])
            )
            self.assertEqual(result["frozen_identity"]["status"], "supplied")
            self.assertEqual(result["identity_binding"]["status"], "bound")
            self.assertEqual(result["status"], "not_evidence")

    def test_run_refuses_live_profile_output(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            live = root / "home" / ".tracedecay"
            live.mkdir(parents=True)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            old_env = os.environ.get("TRACEDECAY_DATA_DIR")
            os.environ["TRACEDECAY_DATA_DIR"] = str(live)
            try:
                rc = rsb.main(
                    [
                        "run",
                        "--workload",
                        str(self.HERE / "workload-dry-run.json"),
                        "--input",
                        str(input_dir),
                        "--output",
                        str(live / "out"),
                    ]
                )
            finally:
                if old_env is None:
                    os.environ.pop("TRACEDECAY_DATA_DIR", None)
                else:
                    os.environ["TRACEDECAY_DATA_DIR"] = old_env
            self.assertEqual(rc, 2)
            self.assertFalse((live / "out").exists())

    def test_absolute_host_label_prevents_result_publication(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            output = root / "output"
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-dry-run.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(output),
                    "--host-label",
                    str(root),
                ]
            )
            self.assertEqual(rc, 2)
            self.assertFalse((output / "storage-runtime-baseline-result.json").exists())


if __name__ == "__main__":
    unittest.main()
