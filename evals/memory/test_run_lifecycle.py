"""Offline orchestration tests: mock model work, not cleanup or reporting."""
import contextlib
import copy
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import isolated_environment
import run_real_model as runner


def scenario(identity="lifecycle"):
    return {"id": identity, "setup": {"facts": [], "files": {}},
            "real_model": {"prompts": ["inspect"]}, "assertions": [
                {"name": "fact count", "kind": "fact-count", "op": "eq", "value": 0}
            ]}


class RunnerLifecycleTests(unittest.TestCase):
    def setUp(self):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        self.runs = Path(root.name) / "runs"
        self.environments = []
        self.order = []
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(mock.patch.object(runner, "RUNS_DIR", self.runs))
        self.scenarios = self.stack.enter_context(mock.patch.object(runner, "load_scenarios", return_value=[scenario()]))
        self.resolve = self.stack.enter_context(mock.patch.object(runner, "resolve_tracedecay_bin", return_value="/explicit/test-binary"))
        original_create = isolated_environment.create_eval_environment
        def create(identity):
            environment = original_create(identity)
            self.environments.append(environment)
            return environment
        self.create = self.stack.enter_context(mock.patch.object(runner, "create_eval_environment", side_effect=create))
        def start(_binary, log):
            self.order.append("daemon")
            Path(log).write_text("controlled startup diagnostic\n")
        self.start = self.stack.enter_context(mock.patch.object(isolated_environment.EvalEnvironment, "start_daemon", side_effect=start))
        self.stack.enter_context(mock.patch.object(isolated_environment.EvalEnvironment, "require_daemon_alive"))
        def build(_scenario, _binary, _env, fixture):
            self.order.append("fixture")
            fixture.mkdir()
            (fixture / "partial-setup").write_text("owned fixture")
            return fixture
        self.build = self.stack.enter_context(mock.patch.object(runner, "build_fixture", side_effect=build))
        def drive(*_args, transcripts):
            self.order.append("agent")
            transcripts.append({"turn_valid": True, "exit_code": 0})
        self.drive = self.stack.enter_context(mock.patch.object(runner, "drive_hermes", side_effect=drive))
        self.assertions = self.stack.enter_context(mock.patch.object(runner, "evaluate_assertions", return_value=[{"name": "count", "passed": True}]))

    def execute(self, opted_in=True):
        argv = ["--agent-turn", "--i-understand-model-cost"] if opted_in else []
        with contextlib.redirect_stdout(io.StringIO()):
            status = runner.main(argv)
        reports = list(self.runs.glob("*/report.json"))
        self.assertEqual(len(reports), 1)
        report = json.loads(reports[0].read_text())
        for env in self.environments:
            self.assertFalse(env.root.exists(), "owned fixture/profile must be cleaned")
        return status, report, reports[0].parent

    def test_cost_gate_runs_before_binary_lookup_or_environment_creation(self):
        status, report, _ = self.execute(opted_in=False)
        self.assertEqual(status, 2)
        self.assertEqual(report["status"], "blocked")
        self.resolve.assert_not_called()
        self.create.assert_not_called()
        self.start.assert_not_called()
        self.drive.assert_not_called()

    def test_binary_failure_is_recorded_without_allocating_a_profile(self):
        self.resolve.side_effect = FileNotFoundError("missing binary")
        status, report, _ = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual(report["error"]["stage"], "binary")
        self.create.assert_not_called()

    def test_success_starts_the_daemon_before_fixture_and_agent(self):
        status, report, _ = self.execute()
        self.assertEqual(status, 0)
        self.assertEqual(report["status"], "pass")
        self.assertEqual(self.order, ["daemon", "fixture", "agent"])
        self.assertEqual(report["scenarios"][0]["logs"], ["1/daemon.log"])

    def test_setup_failure_preserves_output_cleans_partial_fixture_and_skips_agent(self):
        def fail(_scenario, _binary, _env, fixture):
            fixture.mkdir()
            (fixture / "partial").write_text("allocated before setup failed")
            raise subprocess.CalledProcessError(17, ["tracedecay", "init"], output="setup refused")
        self.build.side_effect = fail
        status, report, directory = self.execute()
        self.assertEqual(status, 1)
        row = report["scenarios"][0]
        self.assertEqual(row["error"]["stage"], "fixture")
        self.assertEqual(row["error"]["exit_code"], 17)
        self.assertEqual((directory / "1/fixture-failure.log").read_text(), "setup refused")
        self.drive.assert_not_called()

    def test_daemon_start_failure_is_reported_and_cleans_its_profile(self):
        self.start.side_effect = RuntimeError("daemon could not bind")
        status, report, _ = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual(report["scenarios"][0]["error"]["stage"], "daemon")
        self.build.assert_not_called()
        self.drive.assert_not_called()

    def test_invalid_or_empty_turns_do_not_grade_a_noop_as_success(self):
        def invalid(*_args, transcripts):
            transcripts.append({"turn_valid": False, "exit_code": 19})
        self.drive.side_effect = invalid
        status, report, _ = self.execute()
        self.assertEqual(status, 1)
        row = report["scenarios"][0]
        self.assertEqual(row["error"]["stage"], "agent")
        self.assertEqual(row["transcripts"][0]["exit_code"], 19)
        self.assertions.assert_not_called()

    def test_keyboard_interrupt_preserves_partial_transcripts_and_stops_the_batch(self):
        self.scenarios.return_value = [scenario("first"), scenario("second")]
        def interrupt(*_args, transcripts):
            transcripts.append({"turn_valid": True, "exit_code": 0})
            raise KeyboardInterrupt()
        self.drive.side_effect = interrupt
        status, report, _ = self.execute()
        self.assertEqual(status, 130)
        self.assertEqual([r["status"] for r in report["scenarios"]], ["error", "not_run"])
        self.assertEqual(len(report["scenarios"][0]["transcripts"]), 1)
        self.assertEqual(len(self.environments), 1)

    def test_sigterm_exit_is_reported_and_cleanup_is_not_bypassed(self):
        self.drive.side_effect = SystemExit(143)
        status, report, _ = self.execute()
        self.assertEqual(status, 143)
        self.assertEqual(report["scenarios"][0]["error"]["type"], "SystemExit")

    def test_failed_scenario_does_not_erase_later_success(self):
        self.scenarios.return_value = [scenario("first"), scenario("second")]
        self.start.side_effect = [RuntimeError("startup failed"), None]
        status, report, _ = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual([r["status"] for r in report["scenarios"]], ["error", "pass"])

    def test_cleanup_error_prevents_a_pass_even_after_successful_assertions(self):
        cleanup = runner.cleanup_eval_artifacts
        def failed_cleanup(fixture, env):
            cleanup(fixture, env)
            raise RuntimeError("forced daemon shutdown")
        with mock.patch.object(runner, "cleanup_eval_artifacts", side_effect=failed_cleanup):
            status, report, _ = self.execute()
        self.assertEqual(status, 1)
        self.assertEqual(report["scenarios"][0]["cleanup_error"]["stage"], "cleanup")


class CurrentFactContractTests(unittest.TestCase):
    def record(self, identity):
        return {"fact_id": identity, "content": "fact", "source_label": "source",
                "trust_score_millionths": 500000, "telemetry": {"retrieval_count": 1}}

    def page(self, identity, after):
        return {"owner": {"kind": "profile"}, "next_after": after,
                "facts": [{"kind": "available", "fact": self.record(identity)}]}

    def test_list_collects_every_page_using_the_opaque_continuation(self):
        with mock.patch.object(runner, "call_exact_tool", side_effect=[self.page("first", "next"), self.page("second", None)]) as call:
            records = runner.list_fact_records("bin", Path("fixture"), {})
        self.assertEqual([r["fact_id"] for r in records], ["first", "second"])
        self.assertEqual(call.call_args_list[1].args[-1], {"limit": 200, "after": "next"})

    def test_list_refuses_an_owner_change_instead_of_combining_projects(self):
        second = self.page("second", None)
        second["owner"] = {"kind": "project", "project_id": "foreign"}
        with mock.patch.object(runner, "call_exact_tool", side_effect=[self.page("first", "next"), second]):
            with self.assertRaises(runner.ExactFactStoreError):
                runner.list_fact_records("bin", Path("fixture"), {})

    def test_real_model_excludes_deterministic_well_behaved_only_assertions(self):
        for phase in ["well-behaved-only", "violation-only"]:
            self.assertFalse(runner.assertion_applies_to_real_model({"phase": phase}))
        for phase in ["both", "real-model", "real-model-only"]:
            self.assertTrue(runner.assertion_applies_to_real_model({"phase": phase}))

    def test_an_empty_successful_process_did_not_prove_a_real_model_turn(self):
        self.assertFalse(runner.turn_is_valid(subprocess.CompletedProcess([], 0, stdout=" \n")))
        self.assertTrue(runner.turn_is_valid(subprocess.CompletedProcess([], 0, stdout="inspected")))


if __name__ == "__main__":
    unittest.main()
