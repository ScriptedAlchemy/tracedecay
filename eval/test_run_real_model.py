import argparse
import importlib.util
import json
import sqlite3
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("run_real_model.py")
SPEC = importlib.util.spec_from_file_location("run_real_model", MODULE_PATH)
run_real_model = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(run_real_model)

BENCHMARK_PATH = MODULE_PATH.parent.parent / "benchmarks" / "run_benchmarks.py"
BENCHMARK_SPEC = importlib.util.spec_from_file_location("run_benchmarks", BENCHMARK_PATH)
run_benchmarks = importlib.util.module_from_spec(BENCHMARK_SPEC)
BENCHMARK_SPEC.loader.exec_module(run_benchmarks)


class HermesHomeSelectionTest(unittest.TestCase):
    def test_default_profile_uses_unique_temp_hermes_home(self):
        first = run_real_model.resolve_hermes_profile(run_real_model.parse_args([]))
        second = run_real_model.resolve_hermes_profile(run_real_model.parse_args([]))
        self.addCleanup(first.cleanup)
        self.addCleanup(second.cleanup)

        self.assertEqual(first.profile, run_real_model.DEFAULT_PROFILE)
        self.assertEqual(second.profile, run_real_model.DEFAULT_PROFILE)
        self.assertNotEqual(first.profile_dir, second.profile_dir)
        self.assertIn(tempfile.gettempdir(), str(first.profile_dir))
        self.assertNotEqual(
            first.profile_dir,
            Path.home() / ".hermes/profiles" / run_real_model.DEFAULT_PROFILE,
        )

    def test_explicit_profile_keeps_legacy_user_profile_location(self):
        selected = run_real_model.resolve_hermes_profile(
            run_real_model.parse_args(["--profile", "custom-eval"])
        )
        self.addCleanup(selected.cleanup)

        self.assertEqual(selected.profile, "custom-eval")
        self.assertEqual(selected.profile_dir, Path.home() / ".hermes/profiles/custom-eval")

    def test_explicit_hermes_home_wins(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "explicit-hermes"
            selected = run_real_model.resolve_hermes_profile(
                run_real_model.parse_args(["--hermes-home", str(home)])
            )
            self.addCleanup(selected.cleanup)

            self.assertEqual(selected.profile, run_real_model.DEFAULT_PROFILE)
            self.assertEqual(selected.profile_dir, home)


class EvalStorageIsolationTest(unittest.TestCase):
    def test_eval_environment_pins_home_and_profile_storage_to_tempdir(self):
        env = run_real_model.create_eval_environment("memory-no-pollution")
        self.addCleanup(env.cleanup)

        self.assertNotEqual(Path(env.env["HOME"]), Path.home())
        self.assertEqual(env.env["USERPROFILE"], env.env["HOME"])
        self.assertEqual(Path(env.env["TRACEDECAY_DATA_DIR"]), env.data_dir)
        self.assertEqual(Path(env.env["TRACEDECAY_GLOBAL_DB"]), env.global_db)
        self.assertTrue(env.data_dir.is_relative_to(env.root))
        self.assertNotEqual(env.global_db, Path.home() / ".tracedecay/global.db")

    def test_store_path_is_resolved_from_runtime_status_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "fixture"
            fixture.mkdir()
            db_path = Path(tmp) / "profile" / "projects" / "eval" / "tracedecay.db"
            db_path.parent.mkdir(parents=True)
            db_path.write_text("")
            env = {"TRACEDECAY_DATA_DIR": str(Path(tmp) / "profile")}
            completed = subprocess.CompletedProcess(
                ["tracedecay", "status", "--runtime", "--json"],
                0,
                stdout=json.dumps({"database": {"db_path": str(db_path)}}),
                stderr="",
            )

            with mock.patch.object(run_real_model, "run", return_value=completed) as run_mock:
                resolved = run_real_model.resolve_store_db_path("tracedecay", fixture, env)

            self.assertEqual(resolved, db_path)
            run_mock.assert_called_once()
            cmd = run_mock.call_args.args[0]
            self.assertEqual(cmd, ["tracedecay", "status", "--runtime", "--json"])
            self.assertEqual(run_mock.call_args.kwargs["cwd"], fixture)
            self.assertEqual(run_mock.call_args.kwargs["env"], env)

    def test_keep_fixture_preserves_isolated_eval_store(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "fixture"
            fixture.mkdir()
            eval_env = run_real_model.create_eval_environment("keep-store")
            self.addCleanup(eval_env.cleanup)

            args = argparse.Namespace(keep_fixture=True)
            run_real_model.cleanup_eval_artifacts(args, fixture, eval_env)

            self.assertTrue(fixture.exists())
            self.assertTrue(eval_env.data_dir.exists())

    def test_cleanup_removes_fixture_and_isolated_eval_store_by_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "fixture"
            fixture.mkdir()
            eval_env = run_real_model.create_eval_environment("cleanup-store")

            args = argparse.Namespace(keep_fixture=False)
            run_real_model.cleanup_eval_artifacts(args, fixture, eval_env)

            self.assertFalse(fixture.exists())
            self.assertFalse(eval_env.root.exists())


class AssertionEvaluationTest(unittest.TestCase):
    def make_db(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        db_path = Path(tmp.name) / "tracedecay.db"
        conn = sqlite3.connect(db_path)
        conn.execute("CREATE TABLE memory_facts (content TEXT)")
        conn.execute("INSERT INTO memory_facts VALUES ('kept')")
        conn.commit()
        conn.close()
        return db_path

    def test_unsupported_assertion_kind_is_structured_failure(self):
        db_path = self.make_db()
        scenario = {
            "assertions": [
                {
                    "kind": "search-rank",
                    "name": "unsupported_search_rank",
                    "phase": "both",
                }
            ]
        }

        outcomes = run_real_model.evaluate_assertions(scenario, db_path)

        self.assertEqual(len(outcomes), 1)
        self.assertFalse(outcomes[0]["passed"])
        self.assertEqual(outcomes[0]["kind"], "search-rank")
        self.assertIn("unsupported assertion kind", outcomes[0]["error"])

    def test_assertions_for_other_phases_are_skipped(self):
        db_path = self.make_db()
        scenario = {
            "assertions": [
                {
                    "kind": "search-rank",
                    "name": "violation_only_rank",
                    "phase": "violation-only",
                },
                {
                    "kind": "sql",
                    "name": "deterministic_sql",
                    "sql": "SELECT 1",
                    "op": "eq",
                    "value": 1,
                    "deterministic_only": True,
                },
            ]
        }

        self.assertEqual(run_real_model.evaluate_assertions(scenario, db_path), [])

    def test_sql_errors_are_reported_as_assertion_failures(self):
        db_path = self.make_db()
        scenario = {
            "assertions": [
                {
                    "kind": "sql",
                    "name": "missing_table",
                    "sql": "SELECT COUNT(*) FROM missing_table",
                    "op": "eq",
                    "value": 0,
                }
            ]
        }

        outcomes = run_real_model.evaluate_assertions(scenario, db_path)

        self.assertEqual(len(outcomes), 1)
        self.assertFalse(outcomes[0]["passed"])
        self.assertEqual(outcomes[0]["name"], "missing_table")
        self.assertEqual(outcomes[0]["error_type"], "sqlite")
        self.assertIn("missing_table", outcomes[0]["error"])


class BenchmarkRunnerTest(unittest.TestCase):
    def test_tracedecay_benchmark_uses_temp_profile_and_status_db_size(self):
        calls = []

        def fake_run_timed(cmd, cwd=None, env=None):
            calls.append((cmd, cwd, env))
            stdout = ""
            if cmd == ["tracedecay", "status", "--json"]:
                stdout = json.dumps(
                    {
                        "db_size_bytes": 12345,
                        "node_count": 7,
                        "edge_count": 8,
                        "file_count": 9,
                    }
                )
            return 0.01, 0, subprocess.CompletedProcess(cmd, 0, stdout=stdout, stderr="")

        class DummyMcp:
            def __init__(self, root, env=None):
                self.root = root
                self.env = env

            def __enter__(self):
                return self

            def __exit__(self, *_exc):
                return None

            def call_tool(self, *_args, **_kwargs):
                return {"result": {"_meta": {"duration_us": 1}, "content": [{"text": "[]"}]}}

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with (
                mock.patch.object(run_benchmarks, "run_timed", side_effect=fake_run_timed),
                mock.patch.object(run_benchmarks, "TraceDecayMcp", DummyMcp),
            ):
                out = run_benchmarks.benchmark_tracedecay("fixture", root, [])

        self.assertEqual(out["cache_size_bytes"], 12345)
        tracedecay_envs = [env for _cmd, _cwd, env in calls if env is not None]
        self.assertTrue(tracedecay_envs)
        for env in tracedecay_envs:
            self.assertIn("TRACEDECAY_DATA_DIR", env)
            self.assertIn("TRACEDECAY_GLOBAL_DB", env)
            self.assertTrue(Path(env["TRACEDECAY_GLOBAL_DB"]).is_relative_to(Path(env["TRACEDECAY_DATA_DIR"])))

    def test_skip_clone_requires_existing_checkout(self):
        with tempfile.TemporaryDirectory() as tmp:
            with (
                mock.patch.object(run_benchmarks, "CLONE_DIR", Path(tmp)),
                mock.patch.object(run_benchmarks.subprocess, "run") as run_mock,
            ):
                with self.assertRaises(FileNotFoundError):
                    run_benchmarks.clone_repo("missing", "https://example.invalid/repo.git", skip_clone=True)

        run_mock.assert_not_called()


if __name__ == "__main__":
    unittest.main()
