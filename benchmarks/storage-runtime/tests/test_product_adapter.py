import importlib.util
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock


HERE = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("product_adapter", HERE / "product_adapter.py")
assert SPEC and SPEC.loader
adapter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(adapter)


class ProductAdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.binary = self.root / "tracedecay"
        self.binary.write_bytes(b"binary")
        self.evidence_binary = self.root / "storage-runtime-evidence"
        self.evidence_binary.write_bytes(b"evidence-binary")
        self.fixture = self.root / "fixture"
        (self.fixture / "project").mkdir(parents=True)
        (self.fixture / "profile").mkdir()
        (self.fixture / adapter.MANIFEST_NAME).write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "project_root": "project",
                    "profile_root": "profile",
                    "fts_queries": {"graph": "needle", "session": "message"},
                }
            ),
            encoding="utf-8",
        )
        (self.fixture / "project" / "fixture-data.txt").write_text(
            "source fixture", encoding="utf-8"
        )
        self.sandbox = self.root / "sandbox"
        self.sandbox.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def test_constructs_exact_graph_and_session_tool_commands(self):
        manifest, project, _ = adapter.load_fixture(self.fixture)
        graph = adapter.product_command(self.binary, manifest, project, "graph")
        session = adapter.product_command(self.binary, manifest, project, "session")
        self.assertEqual(
            graph[:5], [str(self.binary), "tool", "--project", str(project), "search"]
        )
        self.assertEqual(
            session[:5],
            [str(self.binary), "tool", "--project", str(project), "message_search"],
        )
        self.assertEqual(
            json.loads(graph[-1]),
            {"query": "needle", "format": "json", "limit": 50},
        )
        self.assertEqual(
            json.loads(session[-1]),
            {"query": "message", "format": "json", "limit": 50},
        )

    @mock.patch.object(adapter.subprocess, "run")
    def test_invocation_uses_fresh_copied_fixture_and_private_environment(self, run):
        product_output = {"status": "ok", "fixture_text": "must-not-be-published"}
        run.return_value = mock.Mock(
            returncode=0, stdout=json.dumps(product_output), stderr=""
        )
        with mock.patch.dict(
            os.environ,
            {
                "TRACEDECAY_DATA_DIR": "/live",
                "TRACEDECAY_GLOBAL_DB": "/live/global.db",
                "HOME": "/live-home",
                "XDG_CONFIG_HOME": "/live-config",
                "XDG_CACHE_HOME": "/live-cache",
                "TMPDIR": "/live-tmp",
                "NEXTEST_TEST_NAME": "leak",
            },
        ):
            result = adapter.run_fts(self.binary, self.fixture, self.sandbox, "session")
        self.assertEqual(result["schema"], adapter.RESULT_SCHEMA)
        self.assertEqual(result["status"], "not_evidence")
        self.assertEqual(result["evidence_status"]["state"], "not_evidence")
        self.assertEqual(adapter.runner.result_contains_absolute_paths(result), [])
        canonical_output = json.dumps(
            product_output, sort_keys=True, separators=(",", ":")
        )
        self.assertEqual(
            result["product_output"],
            {
                "redacted": True,
                "sha256": adapter.runner.sha256_text(canonical_output),
                "byte_count": len(canonical_output.encode("utf-8")),
            },
        )
        self.assertNotIn("must-not-be-published", json.dumps(result))
        argv = run.call_args.args[0]
        project = Path(argv[argv.index("--project") + 1])
        invocation = project.parents[1]
        copied_fixture = invocation / "fixture"
        child = invocation / "sandbox"
        self.assertNotEqual(project, self.fixture / "project")
        self.assertTrue(project.is_relative_to(copied_fixture))
        self.assertEqual((project / "fixture-data.txt").read_text(), "source fixture")
        env = run.call_args.kwargs["env"]
        self.assertEqual(env["HOME"], str(child / "home"))
        self.assertEqual(env["XDG_CONFIG_HOME"], str(child / "config"))
        self.assertEqual(env["XDG_CACHE_HOME"], str(child / "cache"))
        self.assertEqual(env["TMPDIR"], str(child / "temp"))
        self.assertEqual(env["TRACEDECAY_DATA_DIR"], str(copied_fixture / "profile"))
        self.assertEqual(
            env["TRACEDECAY_GLOBAL_DB"],
            str(copied_fixture / "profile" / "global.db"),
        )
        self.assertEqual(run.call_args.kwargs["cwd"], str(child / "cwd"))
        self.assertNotEqual(env["TRACEDECAY_DATA_DIR"], "/live")
        self.assertNotIn("NEXTEST_TEST_NAME", env)
        output = child / "output" / "product-adapter-result.json"
        self.assertEqual(json.loads(output.read_text(encoding="utf-8")), result)
        self.assertEqual(output.stat().st_nlink, 1)
        (project / "fixture-data.txt").write_text("copied mutation", encoding="utf-8")
        self.assertEqual(
            (self.fixture / "project" / "fixture-data.txt").read_text(), "source fixture"
        )

    def test_missing_manifest_and_escaping_fixture_paths_fail_closed(self):
        (self.fixture / adapter.MANIFEST_NAME).unlink()
        with self.assertRaises(adapter.AdapterError):
            adapter.load_fixture(self.fixture)
        (self.fixture / adapter.MANIFEST_NAME).write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "project_root": "../outside",
                    "profile_root": "profile",
                    "fts_queries": {"graph": "needle"},
                }
            ),
            encoding="utf-8",
        )
        with self.assertRaises(adapter.AdapterError):
            adapter.load_fixture(self.fixture)

    @mock.patch.object(adapter.subprocess, "run")
    def test_nested_fixture_symlink_is_refused_before_product_execution(self, run):
        nested = self.fixture / "project" / "nested"
        nested.mkdir()
        target = self.root / "outside"
        target.mkdir()
        try:
            (nested / "alias").symlink_to(target, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unsupported on this platform")
        with self.assertRaises(adapter.AdapterError):
            adapter.run_fts(self.binary, self.fixture, self.sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_hardlinked_fixture_file_is_refused_before_product_execution(self, run):
        source = self.root / "operator-owned.sqlite"
        source.write_text("not a fixture", encoding="utf-8")
        try:
            os.link(source, self.fixture / "profile" / "alias.sqlite")
        except OSError:
            self.skipTest("hardlinks unsupported on this filesystem")
        with self.assertRaises(adapter.AdapterError):
            adapter.run_fts(self.binary, self.fixture, self.sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_custom_live_profile_fixture_is_refused_before_product_execution(self, run):
        with mock.patch.dict(
            os.environ, {"TRACEDECAY_DATA_DIR": str(self.fixture)}, clear=False
        ):
            with self.assertRaises(adapter.AdapterError):
                adapter.run_fts(self.binary, self.fixture, self.sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_custom_live_profile_binary_and_sandbox_are_refused(self, run):
        live = self.root / "live-profile"
        live.mkdir()
        live_binary = live / "tracedecay"
        live_binary.write_bytes(b"binary")
        live_sandbox = live / "sandbox"
        live_sandbox.mkdir()
        with mock.patch.dict(
            os.environ, {"TRACEDECAY_DATA_DIR": str(live)}, clear=False
        ):
            for binary, sandbox in (
                (live_binary, self.sandbox),
                (self.binary, live_sandbox),
            ):
                with self.subTest(binary=binary, sandbox=sandbox):
                    with self.assertRaises(adapter.AdapterError):
                        adapter.run_fts(binary, self.fixture, sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_custom_live_profile_alias_is_refused_before_product_execution(self, run):
        live = self.root / "live-profile"
        live.mkdir()
        live_fixture = live / "fixture"
        shutil.copytree(self.fixture, live_fixture)
        alias = self.root / "live-profile-alias"
        try:
            alias.symlink_to(live, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unsupported on this platform")
        with mock.patch.dict(
            os.environ, {"TRACEDECAY_DATA_DIR": str(alias)}, clear=False
        ):
            with self.assertRaises(adapter.AdapterError):
                adapter.run_fts(self.binary, live_fixture, self.sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_default_live_profile_fixture_is_refused_before_product_execution(self, run):
        home = self.root / "home"
        live = home / ".tracedecay"
        live.mkdir(parents=True)
        live_fixture = live / "fixture"
        shutil.copytree(self.fixture, live_fixture)
        with mock.patch.object(adapter.Path, "home", return_value=home):
            with self.assertRaises(adapter.AdapterError):
                adapter.run_fts(self.binary, live_fixture, self.sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_fixture_and_sandbox_overlap_is_refused_before_product_execution(self, run):
        overlap = self.fixture / "sandbox"
        overlap.mkdir()
        with self.assertRaises(adapter.AdapterError):
            adapter.run_fts(self.binary, self.fixture, overlap, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_fixture_inode_replacement_after_snapshot_is_refused(self, run):
        original_copy = adapter.runner.copy_safe_tree
        replacement = self.root / "replacement-manifest.json"
        replacement.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "project_root": "project",
                    "profile_root": "profile",
                    "fts_queries": {"graph": "replacement", "session": "replacement"},
                }
            ),
            encoding="utf-8",
        )

        def replace_then_copy(source, destination, role, **kwargs):
            os.replace(replacement, Path(source) / adapter.MANIFEST_NAME)
            return original_copy(source, destination, role, **kwargs)

        with mock.patch.object(
            adapter.runner, "copy_safe_tree", side_effect=replace_then_copy
        ):
            with self.assertRaises(adapter.AdapterError):
                adapter.run_fts(self.binary, self.fixture, self.sandbox, "graph")
        run.assert_not_called()

    @mock.patch.object(adapter.subprocess, "run")
    def test_nonzero_or_non_json_product_output_fails_closed(self, run):
        run.return_value = mock.Mock(returncode=1, stdout="", stderr="failure")
        with self.assertRaises(adapter.AdapterError):
            adapter.run_fts(self.binary, self.fixture, self.sandbox, "graph")
        run.return_value = mock.Mock(returncode=0, stdout="not json", stderr="")
        with self.assertRaises(adapter.AdapterError):
            adapter.run_fts(self.binary, self.fixture, self.sandbox, "graph")

    def test_s11_gate_commands_are_fixed_and_bind_concrete_s6_apis(self):
        output = self.sandbox / "gate-output"
        output.mkdir()
        commands = adapter.s11_gate_commands(
            self.evidence_binary,
            self.fixture,
            output,
            fixture_sha256="a" * 64,
            product_commit_sha="b" * 40,
            product_binary_sha256="c" * 64,
            evidence_binary_sha256="d" * 64,
        )
        self.assertEqual(
            [item["gate_id"] for item in commands],
            [
                "storage-runtime-maintenance-doctor-v1",
                "storage-runtime-crash-recovery-repair-v1",
                "storage-runtime-backup-restore-v1",
            ],
        )
        for item in commands:
            self.assertEqual(
                item["argv"][:2],
                [
                    str(self.evidence_binary),
                    "--gate",
                ],
            )
            self.assertEqual(item["argv"][2], item["gate_id"])
            self.assertIn("--fixture-sha256", item["argv"])
            self.assertIn("a" * 64, item["argv"])
            self.assertIn("--product-commit-sha", item["argv"])
            self.assertIn("b" * 40, item["argv"])
            self.assertIn("c" * 64, item["argv"])
            self.assertIn("d" * 64, item["argv"])
            self.assertNotIn(str(self.binary), item["argv"])
            self.assertNotIn("cargo", item["argv"])
            self.assertNotIn("sh", item["argv"])
        self.assertEqual(
            commands[0]["api_bindings"],
            [
                "MaintenanceCoordinator",
                "SqliteMaintenanceDriver",
                "SqliteDoctorHealthLane",
            ],
        )
        self.assertEqual(
            commands[1]["api_bindings"],
            [
                "MaintenanceCoordinator",
                "SqliteDoctorHealthLane",
                "SqliteCorruptionProbe",
                "SqliteRepairDriver",
                "FilesystemQuarantineStore",
            ],
        )
        self.assertEqual(
            commands[2]["api_bindings"],
            [
                "BackupRoot",
                "FilesystemBackupStore",
                "SqliteOnlineBackupDriver",
                "RestorePublicationAuthority",
                "BackupRestoreOrchestrator",
            ],
        )

    def test_s11_typed_gate_evidence_rejects_missing_api_binding(self):
        document = adapter.pending_gate_evidence(
            "storage-runtime-maintenance-doctor-v1",
            "adapter command has not executed",
        )
        self.assertIsNone(document["product_binary_sha256"])
        self.assertIsNone(document["evidence_binary_sha256"])
        document["api_bindings"].pop()
        with self.assertRaisesRegex(adapter.AdapterError, "typed evidence"):
            adapter.validate_s11_gate_evidence(
                "storage-runtime-maintenance-doctor-v1", document
            )

    def test_s11_refuses_product_binary_as_evidence_binary(self):
        identity = adapter.runner.binary_identity(self.binary)["sha256"]
        with self.assertRaisesRegex(adapter.AdapterError, "distinct artifacts"):
            adapter.run_s11(
                self.binary,
                self.binary,
                self.fixture,
                self.sandbox,
                fixture_sha256=adapter.runner.fingerprint_tree(
                    self.fixture, "test fixture"
                )["aggregate_sha256"],
                product_commit_sha="b" * 40,
                product_binary_sha256=identity,
                evidence_binary_sha256=identity,
            )

    @mock.patch.object(adapter, "execute_fixed_argv", new_callable=mock.AsyncMock)
    def test_s11_suite_never_promotes_unexecuted_gate(self, execute):
        execute.return_value = {
            "exit_code": 2,
            "timed_out": False,
            "process_tree_clean": True,
            "stdout": b"",
            "stderr": b"",
            "stdout_truncated": False,
            "stderr_truncated": False,
        }
        result = adapter.run_s11(
            self.binary,
            self.evidence_binary,
            self.fixture,
            self.sandbox,
            fixture_sha256=adapter.runner.fingerprint_tree(
                self.fixture, "test fixture"
            )["aggregate_sha256"],
            product_commit_sha="b" * 40,
            product_binary_sha256=adapter.runner.binary_identity(self.binary)[
                "sha256"
            ],
            evidence_binary_sha256=adapter.runner.binary_identity(
                self.evidence_binary
            )["sha256"],
        )
        self.assertEqual(result["status"], "not_evidence")
        self.assertEqual(result["evidence_status"]["state"], "not_evidence")
        self.assertEqual(
            [gate["status"] for gate in result["gates"]],
            ["not_run", "not_run", "not_run"],
        )


if __name__ == "__main__":
    unittest.main()
