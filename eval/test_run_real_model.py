import argparse
import contextlib
import importlib.util
import io
import json
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

SCORE_PATH = MODULE_PATH.parent / "hermetic" / "score.py"
SCORE_SPEC = importlib.util.spec_from_file_location("hermetic_score", SCORE_PATH)
hermetic_score = importlib.util.module_from_spec(SCORE_SPEC)
SCORE_SPEC.loader.exec_module(hermetic_score)


class HermesHomeSelectionTest(unittest.TestCase):
    def test_removed_profile_flags_are_rejected(self):
        for args in (["--profile", "custom-eval"], ["--hermes-home", "/tmp/hermes"]):
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    run_real_model.parse_args(args)

    def test_install_uses_the_isolated_user_hermes_home(self):
        env = run_real_model.create_eval_environment("hermes-user-install")
        self.addCleanup(env.cleanup)
        fixture = env.root / "fixture"
        fixture.mkdir()
        completed = subprocess.CompletedProcess([], 0, stdout="", stderr="")

        with mock.patch.object(run_real_model, "run", return_value=completed) as run_mock:
            hermes_home = run_real_model.ensure_hermes_install(
                "model", "provider", "tracedecay", fixture, env.env
            )

        self.assertEqual(hermes_home, env.home / ".hermes")
        self.assertTrue((hermes_home / "config.yaml").is_file())
        cmd = run_mock.call_args.args[0]
        self.assertEqual(
            cmd,
            ["tracedecay", "install", "--agent", "hermes", "--no-dashboard"],
        )
        self.assertNotIn("--profile", cmd)
        self.assertNotIn("--project-root", cmd)
        self.assertEqual(run_mock.call_args.kwargs["cwd"], fixture)
        self.assertEqual(run_mock.call_args.kwargs["env"], env.env)


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


class ExactFactStoreEvaluationTest(unittest.TestCase):
    def test_fixture_seeds_through_exact_fact_store_add(self):
        scenario = {
            "id": "exact-fixture",
            "setup": {
                "facts": [
                    {
                        "content": "Kept through the public fact-store path",
                        "category": "project",
                        "source": "fixture",
                        "trust": 0.8,
                    }
                ]
            },
        }
        completed = subprocess.CompletedProcess(
            [],
            0,
            stdout=json.dumps({"fact": {"fact_id": "fact.project.fixture"}}),
            stderr="",
        )
        with mock.patch.object(run_real_model, "run", return_value=completed) as run_mock:
            fixture = run_real_model.build_fixture(scenario, "tracedecay", {})
        self.addCleanup(run_real_model.shutil.rmtree, fixture, ignore_errors=True)

        tool_call = next(
            call.args[0]
            for call in run_mock.call_args_list
            if call.args[0][1:3] == ["tool", "tracedecay_fact_store_add"]
        )
        payload = json.loads(tool_call[-1])
        self.assertEqual(payload["source"], "fixture")
        self.assertEqual(payload["trust"], 0.8)
        self.assertEqual(payload["format"], "json")

    def test_fixture_preload_requires_the_seeded_fact_in_search_results(self):
        scenario = {
            "id": "exact-preload",
            "setup": {
                "facts": [
                    {
                        "content": "Warm this fact through exact search",
                        "category": "project",
                        "source": "hot",
                        "trust": 0.8,
                        "preload_query": "warm",
                        "preload_searches": 1,
                    }
                ]
            },
        }
        responses = iter(
            [
                subprocess.CompletedProcess([], 0, stdout="", stderr=""),
                subprocess.CompletedProcess(
                    [], 0, stdout=json.dumps({"fact": {"fact_id": "fact.hot"}}), stderr=""
                ),
                subprocess.CompletedProcess(
                    [], 0, stdout=json.dumps({"facts": []}), stderr=""
                ),
            ]
        )
        with mock.patch.object(
            run_real_model,
            "run",
            side_effect=lambda *args, **kwargs: next(responses),
        ):
            with self.assertRaisesRegex(
                run_real_model.ExactFactStoreError, "did not return FactId"
            ):
                run_real_model.build_fixture(scenario, "tracedecay", {})

    def test_source_count_reads_exact_fact_store_list(self):
        scenario = {
            "assertions": [
                {
                    "kind": "source-count",
                    "name": "one_kept_fact",
                    "source": "kept",
                    "op": "eq",
                    "value": 1,
                }
            ]
        }
        completed = subprocess.CompletedProcess(
            [],
            0,
            stdout=json.dumps(
                {
                    "facts": [
                        {
                            "fact_id": "fact.project.kept",
                            "content": "kept",
                            "source": "kept",
                            "trust_score": 0.8,
                            "retrieval_count": 0,
                        }
                    ]
                }
            ),
            stderr="",
        )
        with mock.patch.object(run_real_model, "run", return_value=completed) as run_mock:
            outcomes = run_real_model.evaluate_assertions(
                scenario, "tracedecay", Path("/fixture"), {}
            )

        self.assertEqual(outcomes[0]["actual"], 1)
        self.assertTrue(outcomes[0]["passed"])
        self.assertEqual(run_mock.call_args.args[0][2], "tracedecay_fact_store_list")

    def test_non_real_model_assertions_are_skipped_without_a_store_read(self):
        scenario = {
            "assertions": [
                {
                    "kind": "fact-count",
                    "name": "violation_only_count",
                    "phase": "violation-only",
                    "op": "eq",
                    "value": 0,
                }
            ]
        }
        with mock.patch.object(run_real_model, "run") as run_mock:
            outcomes = run_real_model.evaluate_assertions(
                scenario, "tracedecay", Path("/fixture"), {}
            )

        self.assertEqual(outcomes, [])
        run_mock.assert_not_called()

    def test_public_tool_errors_are_structured_failures(self):
        scenario = {
            "assertions": [
                {
                    "kind": "fact-count",
                    "name": "list_unavailable",
                    "op": "eq",
                    "value": 0,
                }
            ]
        }
        failed = subprocess.CompletedProcess([], 1, stdout="unavailable", stderr="")
        with mock.patch.object(run_real_model, "run", return_value=failed):
            outcomes = run_real_model.evaluate_assertions(
                scenario, "tracedecay", Path("/fixture"), {}
            )

        self.assertFalse(outcomes[0]["passed"])
        self.assertEqual(outcomes[0]["error_type"], "fact-store")


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


class HermeticScoreTest(unittest.TestCase):
    def test_expected_cli_fragments_can_pass_without_mcp_tool_use(self):
        scenario = {
            "id": "cli-fallback",
            "expected_cli": ["tracedecay tool diff_context", "--args -"],
            "anti_tools": ["grep"],
        }

        result = hermetic_score.evaluate_scenario(
            scenario,
            session_id=None,
            transcript=None,
            td_tools=[],
            native_tools=["Bash"],
            commands=[
                "git diff --name-only | jq -R -s '{files: split(\"\\n\")[:-1]}' | "
                "tracedecay tool diff_context --args -"
            ],
        )

        self.assertTrue(result["pass"], result)
        self.assertEqual(result["expected_cli_missing"], [])

    def test_expected_cli_fragments_match_prefixed_tool_aliases(self):
        scenario = {
            "id": "cli-prefixed-tool",
            "expected_cli": ["tracedecay tool insert_at", "tool search"],
        }

        result = hermetic_score.evaluate_scenario(
            scenario,
            session_id=None,
            transcript=None,
            td_tools=[],
            native_tools=["Bash"],
            commands=[
                "tracedecay tool tracedecay_insert_at --args '{}'",
                "tracedecay tool tracedecay_search --args '{\"query\":\"x\"}'",
            ],
        )

        self.assertTrue(result["pass"], result)
        self.assertEqual(result["expected_cli_missing"], [])

    def test_missing_expected_mcp_tool_fails(self):
        scenario = {
            "id": "mcp-first",
            "expected_tools": ["tracedecay_diff_context"],
        }

        result = hermetic_score.evaluate_scenario(
            scenario,
            session_id="s1",
            transcript=None,
            td_tools=["tracedecay_search"],
            native_tools=[],
            commands=[],
        )

        self.assertFalse(result["pass"], result)
        self.assertEqual(result["expected_tools_missing"], ["tracedecay_diff_context"])

    def test_codex_jsonl_collects_tool_names_and_commands(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "codex.jsonl"
            path.write_text(
                json.dumps(
                    {
                        "type": "function_call",
                        "name": "exec_command",
                        "arguments": {
                            "cmd": "tracedecay tool diff_context --args -"
                        },
                    }
                )
                + "\n"
                + json.dumps({"tool_name": "tracedecay_search"})
                + "\n"
            )

            td_tools, native_tools, commands = hermetic_score.count_codex_tools(path)

        self.assertEqual(td_tools, ["tracedecay_search"])
        self.assertIn("exec_command", native_tools)
        self.assertEqual(commands, ["tracedecay tool diff_context --args -"])


if __name__ == "__main__":
    unittest.main()
