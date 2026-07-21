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


if __name__ == "__main__":
    unittest.main()
