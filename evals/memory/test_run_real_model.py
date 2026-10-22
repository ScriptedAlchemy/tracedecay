import argparse
import contextlib
import errno
import importlib.util
import io
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("run_real_model.py")
SPEC = importlib.util.spec_from_file_location("run_real_model", MODULE_PATH)
run_real_model = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(run_real_model)

import portable_process

BENCHMARK_PATH = MODULE_PATH.parent.parent.parent / "benchmark_data" / "run_benchmarks.py"
BENCHMARK_SPEC = importlib.util.spec_from_file_location("run_benchmarks", BENCHMARK_PATH)
run_benchmarks = importlib.util.module_from_spec(BENCHMARK_SPEC)
BENCHMARK_SPEC.loader.exec_module(run_benchmarks)

SCORE_PATH = MODULE_PATH.parent.parent / "hermetic" / "score.py"
SCORE_SPEC = importlib.util.spec_from_file_location("hermetic_score", SCORE_PATH)
hermetic_score = importlib.util.module_from_spec(SCORE_SPEC)
SCORE_SPEC.loader.exec_module(hermetic_score)


def setUpModule():
    runs = tempfile.TemporaryDirectory()
    unittest.addModuleCleanup(runs.cleanup)
    patch = mock.patch.object(run_real_model, "RUNS_DIR", Path(runs.name))
    patch.start()
    unittest.addModuleCleanup(patch.stop)


class HermesHomeSelectionTest(unittest.TestCase):
    def test_driver_defaults_resolve_before_launch_and_reporting(self):
        self.assertEqual(run_real_model.parse_args([]).model, "gpt-5.4-mini")
        self.assertEqual(
            run_real_model.parse_args(["--driver", "cursor-agent"]).model,
            "composer-2.5",
        )
        self.assertEqual(
            run_real_model.parse_args(
                ["--driver", "cursor-agent", "--model", "explicit-model"]
            ).model,
            "explicit-model",
        )

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


class BinaryResolutionTest(unittest.TestCase):
    def test_blocked_run_writes_report_without_resolving_a_binary(self):
        for flags in ([], ["--agent-turn"], ["--i-understand-model-cost"]):
            with self.subTest(flags=flags), tempfile.TemporaryDirectory() as tmp:
                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                    mock.patch.object(run_real_model, "resolve_tracedecay_bin") as resolve,
                    mock.patch.object(run_real_model, "load_scenarios", return_value=[{"id": "test"}]),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    self.assertEqual(run_real_model.main(flags), 2)
                resolve.assert_not_called()
                reports = list(Path(tmp).glob("*/report.json"))
                self.assertEqual(len(reports), 1)
                report = json.loads(reports[0].read_text())
                self.assertEqual(report["status"], "blocked")
                self.assertIsNone(report["tracedecay_bin"])

    def test_explicit_binary_is_used_and_must_exist(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "tracedecay"
            binary.write_text("#!/bin/sh\necho tracedecay 0.0.0\n")
            binary.chmod(0o755)
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(
                    run_real_model.resolve_tracedecay_bin(binary), str(binary)
                )
                with self.assertRaises(run_real_model.EvalSetupError):
                    run_real_model.resolve_tracedecay_bin(Path(tmp) / "missing")

    def test_default_never_falls_back_to_a_prebuilt_or_path_binary(self):
        failed = subprocess.CompletedProcess([], 2, stdout="")
        with tempfile.TemporaryDirectory() as tmp:
            path_binary = Path(tmp) / "tracedecay"
            path_binary.write_text("#!/bin/sh\n")
            path_binary.chmod(0o755)
            with (
                mock.patch.dict("os.environ", {"PATH": tmp}),
                mock.patch.object(run_real_model, "run", return_value=failed),
                self.assertRaises(run_real_model.EvalSetupError),
            ):
                run_real_model.resolve_tracedecay_bin(None)


    def test_build_failure_writes_a_failed_report_before_any_scenario_starts(self):
        with tempfile.TemporaryDirectory() as tmp:
            with (
                mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                mock.patch.object(run_real_model, "load_scenarios", return_value=[{"id": "test"}]),
                mock.patch.object(run_real_model, "run", return_value=subprocess.CompletedProcess([], 2, stdout="")),
                mock.patch.object(run_real_model, "run_scenario") as scenario,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(run_real_model.main(["--agent-turn", "--i-understand-model-cost"]), 1)
            report = json.loads(next(Path(tmp).glob("*/report.json")).read_text())
        scenario.assert_not_called()
        self.assertEqual(report["status"], "fail")
        self.assertIn("build-eval-binary.sh exited 2", report["reason"])
        self.assertIsNone(report["tracedecay_bin"])


class CursorComposerSelectionTest(unittest.TestCase):
    def test_cursor_driver_loads_the_installed_plugin_in_composer(self):
        env = run_real_model.create_eval_environment("cursor-composer")
        self.addCleanup(env.cleanup)
        fixture = env.root / "fixture"
        fixture.mkdir()
        log_dir = run_real_model.RUNS_DIR / "cursor-composer-test"
        log_dir.mkdir(parents=True, exist_ok=True)
        self.addCleanup(lambda: log_dir.rmdir())
        self.addCleanup(lambda: (log_dir / "cursor-composer-prompt1.log").unlink(missing_ok=True))
        args = argparse.Namespace(tracedecay_bin="tracedecay", model="composer-2.5")
        scenario = {"id": "cursor-composer", "real_model": {"prompts": ["inspect"]}}
        completed = subprocess.CompletedProcess([], 0, stdout="ok", stderr="")

        with mock.patch.object(run_real_model, "run", return_value=completed) as run_mock:
            run_real_model.drive_cursor_agent(
                args, scenario, fixture, log_dir, env.env
            )

        command = run_mock.call_args_list[1].args[0]
        self.assertEqual(command[0], "cursor-agent")
        self.assertEqual(
            command[command.index("--plugin-dir") + 1],
            str(env.home / ".cursor/plugins/local/tracedecay"),
        )
        self.assertIn("--approve-mcps", command)
        self.assertIn("--force", command)
        self.assertIn("--trust", command)


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

    def test_cancellation_before_ownership_removes_the_profile(self):
        for boundary in ("environment copy", "owner construction"):
            with self.subTest(boundary=boundary), tempfile.TemporaryDirectory() as tmp:
                cancelled = KeyboardInterrupt(f"cancelled during {boundary}")
                interrupt = (
                    mock.patch.object(os.environ, "keys", side_effect=cancelled)
                    if boundary == "environment copy"
                    else mock.patch.object(
                        run_real_model, "EvalEnvironment", side_effect=cancelled
                    )
                )
                with mock.patch.object(tempfile, "tempdir", tmp), interrupt as abort:
                    with self.assertRaises(KeyboardInterrupt) as raised:
                        run_real_model.create_eval_environment("cancelled-setup")
                self.assertIs(raised.exception, cancelled)
                abort.assert_called_once()
                self.assertEqual(list(Path(tmp).iterdir()), [])

    def test_cleanup_removes_fixture_and_isolated_eval_store(self):
        eval_env = run_real_model.create_eval_environment("cleanup-store")
        self.addCleanup(eval_env.cleanup)
        fixture = eval_env.root / "fixture"
        fixture.mkdir()

        eval_env.cleanup()

        self.assertFalse(fixture.exists())
        self.assertFalse(eval_env.root.exists())


FAKE_BIN = """#!/usr/bin/env python3
import os, socket, sys, time
args = sys.argv[1:]
if args[:2] == ["daemon", "run"]:
    if os.environ.get("FAKE_DAEMON_FAIL"):
        sys.exit(3)
    path = args[3]
    srv = socket.socket(socket.AF_UNIX)
    srv.bind(path)
    srv.listen(8)
    open(os.environ["TRACEDECAY_DATA_DIR"] + "/daemon.pid", "w").write(str(os.getpid()))
    time.sleep(600)
elif args[:1] == ["init"]:
    if os.environ.get("FAKE_INIT_FAIL"):
        print("code_index_scheduler_unavailable")
        sys.exit(1)
    c = socket.socket(socket.AF_UNIX)
    c.connect(os.environ["TRACEDECAY_DAEMON_SOCKET"])
elif args[:1] == ["tool"]:
    print('{"fact": {"fact_id": "fact.1"}}')
"""


class IsolatedLifecycleTest(unittest.TestCase):
    def test_no_agent_turns_cannot_pass_even_when_store_assertions_pass(self):
        for driver in ("hermes", "cursor-agent"):
            with self.subTest(driver=driver):
                eval_env = run_real_model.create_eval_environment("empty-agent-turns")
                self.addCleanup(eval_env.cleanup)
                report = {"scenarios": []}
                args = argparse.Namespace(driver=driver, tracedecay_bin="tracedecay")
                with (
                    mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
                    mock.patch.object(eval_env, "start_daemon"),
                    mock.patch.object(run_real_model, "seed_fixture"),
                    mock.patch.object(run_real_model, "drive_hermes", return_value=[]),
                    mock.patch.object(run_real_model, "drive_cursor_agent", return_value=[]),
                    mock.patch.object(run_real_model, "evaluate_assertions", return_value=[
                        {"name": "no-pollution", "passed": True, "actual": 0, "expected": 0, "op": "eq"},
                    ]),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    result = run_real_model.run_scenario(args, self.scenario(), Path("."), report)
                self.assertEqual(result, (False, False))
                entry = report["scenarios"][0]
                self.assertEqual(entry["status"], "error (agent turn invalid, see transcript logs)")
                self.assertEqual(entry["transcripts"], [])
                self.assertTrue(entry["assertions"][0]["passed"])
                self.assertFalse(eval_env.root.exists())

    def make_bin(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        path = Path(tmp.name) / "tracedecay"
        path.write_text(FAKE_BIN)
        path.chmod(0o755)
        return str(path)

    def scenario(self):
        return {
            "id": "lifecycle",
            "setup": {
                "facts": [
                    {"content": "c", "category": "project", "source": "s", "trust": 0.5}
                ]
            },
        }

    def test_profile_allocation_failure_writes_error_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            with (
                mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                mock.patch.object(
                    run_real_model, "load_scenarios", return_value=[self.scenario()]
                ),
                mock.patch.object(
                    run_real_model, "resolve_tracedecay_bin", return_value="/unused/tracedecay"
                ),
                mock.patch.object(
                    run_real_model.tempfile, "mkdtemp",
                    side_effect=OSError("No space left on device"),
                ),
                mock.patch.object(run_real_model.EvalEnvironment, "start_daemon") as daemon,
                mock.patch.object(run_real_model, "drive_hermes") as hermes,
                mock.patch.object(run_real_model, "drive_cursor_agent") as cursor,
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                self.assertEqual(
                    run_real_model.main(["--agent-turn", "--i-understand-model-cost"]), 1
                )
            daemon.assert_not_called()
            hermes.assert_not_called()
            cursor.assert_not_called()
            reports = list(Path(tmp).glob("*/report.json"))
            self.assertEqual(len(reports), 1)
            report = json.loads(reports[0].read_text())
            self.assertEqual(report["status"], "fail")
            self.assertEqual(report["scenarios"], [{
                "id": "lifecycle",
                "contract": "stable",
                "status": "error (setup or agent failure)",
                "error": "No space left on device",
                "assertions": [],
                "transcripts": [],
            }])

    def test_environment_construction_cancellation_removes_unowned_profile(self):
        original_mkdtemp = tempfile.mkdtemp
        for operation in ("dict", "EvalEnvironment"):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as tmp:
                allocated = []

                def allocate(*args, **kwargs):
                    root = Path(original_mkdtemp(*args, **kwargs))
                    allocated.append(root)
                    self.addCleanup(run_real_model.shutil.rmtree, root, ignore_errors=True)
                    return str(root)

                cancellation = KeyboardInterrupt(f"cancelled during {operation}")
                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                    mock.patch.object(
                        run_real_model, "load_scenarios", return_value=[
                            self.scenario(), {**self.scenario(), "id": "must-not-start"}
                        ]
                    ),
                    mock.patch.object(
                        run_real_model, "resolve_tracedecay_bin", return_value="/unused/tracedecay"
                    ),
                    mock.patch.object(run_real_model.tempfile, "mkdtemp", side_effect=allocate),
                    mock.patch.object(run_real_model, operation, side_effect=cancellation, create=True),
                    mock.patch.object(run_real_model.subprocess, "Popen") as spawn,
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    self.assertEqual(
                        run_real_model.main(["--agent-turn", "--i-understand-model-cost"]), 130
                    )
                spawn.assert_not_called()
                reports = list(Path(tmp).glob("*/report.json"))
                self.assertEqual(len(reports), 1)
                report = json.loads(reports[0].read_text())
                self.assertEqual(report["status"], "cancelled")
                self.assertEqual(report["scenarios"], [{
                    "id": "lifecycle",
                    "contract": "stable",
                    "status": "cancelled",
                    "error": str(cancellation),
                    "assertions": [],
                    "transcripts": [],
                }])
                self.assertEqual(len(allocated), 1)
                self.assertFalse(allocated[0].exists(), "unclaimed profile must be removed")

    def test_allocation_cleanup_failure_preserves_cancellation_and_inaccessible_profile(self):
        original_mkdtemp = tempfile.mkdtemp
        original_remove = run_real_model.shutil.rmtree
        original_stat = os.stat
        for operation in ("Path", "EvalEnvironment"):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as tmp:
                allocated = []

                def allocate(*args, **kwargs):
                    root = Path(original_mkdtemp(*args, **kwargs))
                    allocated.append(root)
                    self.addCleanup(original_remove, root, ignore_errors=True)
                    return str(root)

                def remove(path, *args, **kwargs):
                    if allocated and Path(path) == allocated[0]:
                        raise PermissionError("profile removal denied")
                    return original_remove(path, *args, **kwargs)

                def stat(path, *args, **kwargs):
                    if (
                        allocated
                        and isinstance(path, (str, os.PathLike))
                        and Path(path) == allocated[0]
                    ):
                        raise PermissionError("profile stat denied")
                    return original_stat(path, *args, **kwargs)

                cancellation = KeyboardInterrupt(f"cancelled during {operation}")
                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                    mock.patch.object(
                        run_real_model, "load_scenarios", return_value=[
                            self.scenario(), {**self.scenario(), "id": "must-not-start"}
                        ]
                    ),
                    mock.patch.object(
                        run_real_model, "resolve_tracedecay_bin", return_value="/unused/tracedecay"
                    ),
                    mock.patch.object(run_real_model.tempfile, "mkdtemp", side_effect=allocate),
                    mock.patch.object(run_real_model, operation, side_effect=cancellation),
                    mock.patch.object(run_real_model.shutil, "rmtree", side_effect=remove),
                    mock.patch.object(os, "stat", side_effect=stat),
                    mock.patch.object(run_real_model.subprocess, "Popen") as spawn,
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    self.assertEqual(
                        run_real_model.main(["--agent-turn", "--i-understand-model-cost"]), 130
                    )
                spawn.assert_not_called()
                self.assertEqual(len(allocated), 1)
                self.assertTrue(allocated[0].exists())
                reports = list(Path(tmp).glob("*/report.json"))
                self.assertEqual(len(reports), 1)
                report = json.loads(reports[0].read_text())
                self.assertEqual(report["status"], "cancelled")
                self.assertEqual(report["scenarios"], [{
                    "id": "lifecycle",
                    "contract": "stable",
                    "status": "cancelled",
                    "error": str(cancellation),
                    "cleanup_error": "profile removal denied",
                    "retained_profile": str(allocated[0]),
                    "assertions": [],
                    "transcripts": [],
                }])

    def test_fresh_environment_inits_and_seeds_against_isolated_daemon(self):
        binary = self.make_bin()
        eval_env = run_real_model.create_eval_environment("lifecycle")
        try:
            fixture = eval_env.root / "fixture"
            fixture.mkdir()
            eval_env.start_daemon(binary, ready_timeout=15)
            run_real_model.seed_fixture(self.scenario(), binary, eval_env.env, fixture)
            self.assertTrue(fixture.is_relative_to(eval_env.root))
            pid = int((eval_env.data_dir / "daemon.pid").read_text())
        finally:
            eval_env.cleanup()
        self.assertFalse(eval_env.root.exists())
        self.assertFalse(Path(eval_env.env["TRACEDECAY_DAEMON_SOCKET"]).parent.exists())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_init_failure_stops_daemon_before_removing_fixture(self):
        binary = self.make_bin()
        eval_env = run_real_model.create_eval_environment("lifecycle")
        self.addCleanup(eval_env.cleanup)
        eval_env.env["FAKE_INIT_FAIL"] = "1"
        args = argparse.Namespace(tracedecay_bin=binary, driver="hermes")
        report = {"scenarios": []}
        removed = []
        original_remove = run_real_model.shutil.rmtree

        def remove(path, *args, **kwargs):
            path = Path(path)
            if path.is_relative_to(eval_env.root) and path.exists():
                self.assertIsNone(eval_env.daemon, "owned work must stop before deleting inputs")
                pid = int((eval_env.data_dir / "daemon.pid").read_text())
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)
                removed.append(path)
            return original_remove(path, *args, **kwargs)

        with (
            tempfile.TemporaryDirectory() as run_dir,
            mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
            mock.patch.object(run_real_model.shutil, "rmtree", side_effect=remove),
            mock.patch.object(run_real_model, "drive_hermes") as driver,
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            result = run_real_model.run_scenario(
                args, self.scenario(), Path(run_dir), report
            )
        self.assertEqual(result, (False, False))
        driver.assert_not_called()
        self.assertTrue(removed)
        self.assertFalse(eval_env.root.exists())
        self.assertIn("command failed (1)", report["scenarios"][0]["error"])
        self.assertNotIn("cleanup_error", report["scenarios"][0])

    def test_daemon_startup_failure_is_reported_and_cleaned(self):
        binary = self.make_bin()
        eval_env = run_real_model.create_eval_environment("lifecycle")
        eval_env.env["FAKE_DAEMON_FAIL"] = "1"
        try:
            with self.assertRaises(run_real_model.EvalSetupError):
                eval_env.start_daemon(binary, ready_timeout=15)
        finally:
            eval_env.cleanup()
        self.assertFalse(eval_env.root.exists())

    def run_scenario_with(self, binary, driver):
        args = argparse.Namespace(tracedecay_bin=binary, driver="hermes")
        report = {"scenarios": []}
        with tempfile.TemporaryDirectory() as run_dir, mock.patch.object(
            run_real_model, "drive_hermes", side_effect=driver
        ):
            result = run_real_model.run_scenario(
                args, self.scenario(), Path(run_dir), report
            )
        return result, report

    def test_agent_failure_and_cancellation_write_error_entries(self):
        binary = self.make_bin()
        for exc, status, cancelled in (
            (RuntimeError("agent exploded"), "error", False),
            (KeyboardInterrupt("stop"), "cancelled", True),
        ):
            with self.subTest(status=status):
                result, report = self.run_scenario_with(binary, exc)
                self.assertEqual(result, (False, cancelled))
                entry = report["scenarios"][0]
                self.assertTrue(entry["status"].startswith(status))
                self.assertEqual(
                    entry["error"], str(exc) or "interrupted"
                )

    def test_daemon_cleanup_does_not_need_to_launch_a_python_helper(self):
        eval_env = run_real_model.create_eval_environment("cleanup-no-helper")
        self.addCleanup(eval_env.cleanup)
        eval_env.start_daemon(self.make_bin(), ready_timeout=15)
        daemon = eval_env.daemon
        popen = subprocess.Popen

        def process_state_only(command, *args, **kwargs):
            # macOS has no /proc: canonical process-group teardown uses ps
            # to distinguish exited zombies from descendants still running.
            if command[0] == "ps":
                return popen(command, *args, **kwargs)
            raise OSError("cannot spawn cleanup helper")

        with mock.patch.object(
            run_real_model.subprocess, "Popen", side_effect=process_state_only
        ):
            eval_env.cleanup()
        self.assertIsNotNone(daemon.returncode)
        self.assertIsNone(eval_env.daemon)
        self.assertFalse(eval_env.root.exists())

    def test_cleanup_failure_retains_original_error_when_artifact_stat_is_denied(self):
        eval_env = run_real_model.create_eval_environment("cleanup-error")
        self.addCleanup(eval_env.cleanup)
        args = argparse.Namespace(tracedecay_bin=self.make_bin(), driver="hermes")
        report = {"scenarios": []}
        # Start before injecting stop failure so setup still uses real commands.
        eval_env.start_daemon(args.tracedecay_bin, ready_timeout=15)
        daemon = eval_env.daemon
        original_stat = os.stat
        inaccessible = []

        def stat(path, *args, **kwargs):
            if isinstance(path, (str, os.PathLike)):
                artifact = Path(path)
                if (
                    artifact == eval_env.root
                    or artifact == eval_env.socket_dir
                    or artifact.parent == eval_env.root
                ):
                    inaccessible.append(artifact)
                    raise PermissionError(errno.EACCES, "owned artifact stat denied", str(path))
            return original_stat(path, *args, **kwargs)

        with (
            tempfile.TemporaryDirectory() as run_dir,
            mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
            mock.patch.object(eval_env, "start_daemon"),
            mock.patch.object(run_real_model, "seed_fixture"),
            mock.patch.object(run_real_model, "drive_hermes", side_effect=RuntimeError("agent failed")),
            mock.patch.object(run_real_model, "stop_owned_process", side_effect=OSError("stop failed")),
            mock.patch.object(os, "stat", side_effect=stat),
        ):
            result = run_real_model.run_scenario(args, self.scenario(), Path(run_dir), report)
        self.assertEqual(result, (False, False))
        self.assertEqual(report["scenarios"][0]["error"], "agent failed")
        self.assertEqual(report["scenarios"][0]["cleanup_error"], "stop failed")
        entry = report["scenarios"][0]
        fixture = Path(entry["retained_fixture"])
        self.assertEqual(entry["retained_profile"], str(eval_env.root))
        self.assertEqual(entry["retained_socket_dir"], str(eval_env.socket_dir))
        self.assertEqual(set(inaccessible), {fixture, eval_env.root, eval_env.socket_dir})
        self.assertTrue(fixture.exists())
        self.assertIs(eval_env.daemon, daemon)
        self.assertIsNone(daemon.poll())
        self.assertTrue(eval_env.root.exists())
        eval_env.cleanup()
        self.assertFalse(eval_env.root.exists())

    def test_socket_cleanup_failure_reports_retained_directory_and_cleans_profile(self):
        eval_env = run_real_model.create_eval_environment("socket-cleanup-error")
        self.addCleanup(eval_env.cleanup)
        binary = self.make_bin()
        original_remove = run_real_model.shutil.rmtree
        original_unlink = os.unlink
        removed = []
        fixture = None
        daemon = None

        def drive(_args, _scenario, path, _run_dir, _env):
            nonlocal fixture, daemon
            fixture = path
            daemon = eval_env.daemon
            return [{"turn_valid": True}]

        def remove(path, *args, **kwargs):
            path = Path(path)
            if path in (eval_env.socket_dir, eval_env.root):
                self.assertIsNone(eval_env.daemon)
                self.assertIsNotNone(daemon.returncode)
                removed.append(path)
            return original_remove(path, *args, **kwargs)

        def unlink(path, *args, **kwargs):
            if Path(path).name == "daemon.sock":
                raise PermissionError(errno.EACCES, "socket directory deletion failed", path)
            return original_unlink(path, *args, **kwargs)

        with tempfile.TemporaryDirectory() as tmp:
            with (
                mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                mock.patch.object(run_real_model, "load_scenarios", return_value=[self.scenario()]),
                mock.patch.object(run_real_model, "resolve_tracedecay_bin", return_value=binary),
                mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
                mock.patch.object(run_real_model, "drive_hermes", side_effect=drive),
                mock.patch.object(run_real_model, "evaluate_assertions", return_value=[]),
                mock.patch.object(run_real_model.shutil, "rmtree", side_effect=remove),
                mock.patch.object(os, "unlink", side_effect=unlink),
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                code = run_real_model.main(["--agent-turn", "--i-understand-model-cost"])
            report_path, = Path(tmp).glob("*/report.json")
            report = json.loads(report_path.read_text())

        self.assertEqual(code, 1)
        self.assertEqual(report["status"], "fail")
        self.assertEqual(removed, [eval_env.socket_dir, eval_env.root])
        self.assertFalse(eval_env.root.exists())
        self.assertFalse(fixture.exists())
        self.assertTrue(eval_env.socket_dir.exists())
        entry = report["scenarios"][0]
        self.assertEqual(entry["status"], "error (cleanup failure)")
        self.assertIn("socket directory deletion failed", entry["cleanup_error"])
        self.assertEqual(entry["retained_socket_dir"], str(eval_env.socket_dir))
        self.assertNotIn("retained_profile", entry)
        self.assertNotIn("retained_fixture", entry)
        self.assertNotIn("daemon_pid", entry)
        eval_env.cleanup()
        self.assertFalse(eval_env.socket_dir.exists())

    def test_seed_failure_retains_fixture_until_daemon_stop_is_confirmed(self):
        binary = self.make_bin()
        eval_env = run_real_model.create_eval_environment("seed-cleanup-error")
        self.addCleanup(eval_env.cleanup)
        args = argparse.Namespace(tracedecay_bin=binary, driver="hermes")
        scenario = self.scenario()
        # Initialization and the fact write succeed before preload validation
        # fails, so the daemon has already used the fixture being retained.
        scenario["setup"]["facts"][0]["preload_searches"] = 1
        report = {"scenarios": []}
        fixture = None
        original_seed = run_real_model.seed_fixture
        original_stop = run_real_model.stop_owned_process

        def seed(scenario, binary, env, path):
            nonlocal fixture
            fixture = path
            return original_seed(scenario, binary, env, path)

        def stop(process, grace):
            if process is eval_env.daemon:
                raise OSError("daemon stop failed")
            return original_stop(process, grace)

        with (
            tempfile.TemporaryDirectory() as run_dir,
            mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
            mock.patch.object(run_real_model, "seed_fixture", side_effect=seed),
            mock.patch.object(run_real_model, "stop_owned_process", side_effect=stop),
            mock.patch.object(run_real_model, "drive_hermes") as driver,
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            result = run_real_model.run_scenario(args, scenario, Path(run_dir), report)

        self.assertEqual(result, (False, False))
        driver.assert_not_called()
        entry = report["scenarios"][0]
        self.assertIn("sets preload_searches without preload_query", entry["error"])
        self.assertEqual(entry["cleanup_error"], "daemon stop failed")
        self.assertIsNotNone(fixture)
        self.assertTrue(fixture.exists(), "a live daemon must retain its seeded fixture")
        self.assertEqual(entry["retained_fixture"], str(fixture))
        self.assertEqual(entry["retained_profile"], str(eval_env.root))
        self.assertEqual(entry["daemon_pid"], eval_env.daemon.pid)
        self.assertIsNone(eval_env.daemon.poll())
        self.assertEqual(
            (fixture / "src/lib.rs").read_text(), "pub fn eval_fixture_marker() {}\n"
        )
        eval_env.cleanup()
        self.assertFalse(eval_env.root.exists())
        self.assertFalse(fixture.exists())

    def test_signals_stop_agent_descendants_and_persist_cancelled_report(self):
        # A real child and grandchild that ignore SIGTERM exercise forced
        # process-group teardown rather than a mocked KeyboardInterrupt.
        script = """
import json, os, pathlib, signal, subprocess, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
ready = pathlib.Path(sys.argv[1] + '.ready')
child = subprocess.Popen([sys.executable, '-c',
    'import pathlib,signal,sys,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); '
    'pathlib.Path(sys.argv[1]).touch(); time.sleep(600)', str(ready)])
while not ready.exists(): time.sleep(0.01)
pathlib.Path(sys.argv[1]).write_text(json.dumps([os.getpid(), child.pid]))
time.sleep(600)
"""
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=sig), tempfile.TemporaryDirectory() as tmp:
                pidfile = Path(tmp) / "agent-pids.json"
                eval_env = run_real_model.create_eval_environment("signal-cleanup")
                self.addCleanup(eval_env.cleanup)
                notified = threading.Event()

                def interrupt_agent():
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        if pidfile.exists() and pidfile.stat().st_size:
                            notified.set()
                            os.kill(os.getpid(), sig)
                            return
                        time.sleep(0.01)

                def drive(*_args):
                    run_real_model.run([sys.executable, "-c", script, str(pidfile)], timeout=20)
                    self.fail("the interrupted agent must not complete")

                thread = threading.Thread(target=interrupt_agent, daemon=True)
                previous_handlers = {s: signal.getsignal(s) for s in (signal.SIGINT, signal.SIGTERM)}
                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp) / "runs"),
                    mock.patch.object(run_real_model, "load_scenarios", return_value=[self.scenario()]),
                    mock.patch.object(run_real_model, "resolve_tracedecay_bin", return_value=self.make_bin()),
                    mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
                    mock.patch.object(run_real_model, "drive_hermes", side_effect=drive),
                    mock.patch.object(run_real_model, "PROCESS_STOP_GRACE_SECONDS", 0.2),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    thread.start()
                    code = run_real_model.main(["--agent-turn", "--i-understand-model-cost"])
                    thread.join(timeout=1)
                self.assertTrue(notified.is_set())
                self.assertEqual(code, 130)
                report_path, = (Path(tmp) / "runs").glob("*/report.json")
                report = json.loads(report_path.read_text())
                self.assertEqual(report["status"], "cancelled")
                self.assertEqual(report["scenarios"][0]["status"], "cancelled")
                leader, child = json.loads(pidfile.read_text())
                with self.assertRaises(ProcessLookupError):
                    os.kill(leader, 0)
                # Linux can retain an orphaned zombie until PID 1 reaps it;
                # use the canonical runnable-member check for the descendant.
                self.assertFalse(portable_process._group_alive(leader), child)
                self.assertIsNone(eval_env.daemon)
                self.assertFalse(eval_env.root.exists())
                self.assertEqual(previous_handlers, {
                    s: signal.getsignal(s) for s in (signal.SIGINT, signal.SIGTERM)
                })

    def test_cancelled_command_stop_failure_retains_its_files_and_primary_error(self):
        script = """
import os, pathlib, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
pathlib.Path(sys.argv[1]).write_text(str(os.getpid()))
time.sleep(30)
"""
        # Exercise main before scenario allocation, fixture seeding, and an
        # established agent turn with their respective process owners.
        for phase in ("binary resolution", "fixture setup", "agent turn"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as tmp:
                pidfile = Path(tmp) / "command.pid"
                eval_env = (
                    None if phase == "binary resolution"
                    else run_real_model.create_eval_environment("command-stop-failure")
                )
                fixture = None
                command = None
                stop_attempts = 0
                notified = threading.Event()
                finished = threading.Event()
                original_stop = run_real_model.stop_owned_process
                original_seed = run_real_model.seed_fixture

                def interrupt_command():
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline and not finished.wait(0.01):
                        if pidfile.exists() and pidfile.stat().st_size:
                            notified.set()
                            os.kill(os.getpid(), signal.SIGTERM)
                            return

                def run_command():
                    run_real_model.run(
                        [sys.executable, "-c", script, str(pidfile)],
                        cwd=fixture,
                        timeout=20,
                    )
                    self.fail("the interrupted command must not complete")

                def seed(scenario, binary, env, path):
                    nonlocal fixture
                    fixture = path
                    if phase == "fixture setup":
                        (path / "owned-input").write_text("must survive a failed stop")
                        run_command()
                    else:
                        original_seed(scenario, binary, env, path)

                def fail_command_stop(process, grace):
                    nonlocal command, stop_attempts
                    if str(pidfile) in process.args:
                        command = process
                        stop_attempts += 1
                        raise OSError("command stop failed")
                    return original_stop(process, grace)

                thread = threading.Thread(target=interrupt_command, daemon=True)
                try:
                    with (
                        mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp) / "runs"),
                        mock.patch.object(run_real_model, "load_scenarios", return_value=[
                            self.scenario(), {"id": "must-not-run-after-cancellation"},
                        ]),
                        mock.patch.object(
                            run_real_model, "resolve_tracedecay_bin",
                            side_effect=(
                                (lambda _explicit: run_command()) if phase == "binary resolution"
                                else (lambda _explicit: self.make_bin())
                            ),
                        ),
                        mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env) as create_env,
                        mock.patch.object(run_real_model, "seed_fixture", side_effect=seed),
                        mock.patch.object(run_real_model, "drive_hermes", side_effect=lambda *_args: run_command()),
                        mock.patch.object(run_real_model, "stop_owned_process", side_effect=fail_command_stop),
                        mock.patch.object(run_real_model, "PROCESS_STOP_GRACE_SECONDS", 0.2),
                        contextlib.redirect_stdout(io.StringIO()),
                        contextlib.redirect_stderr(io.StringIO()),
                    ):
                        thread.start()
                        code = run_real_model.main(["--agent-turn", "--i-understand-model-cost"])
                    finished.set()
                    thread.join(timeout=1)
                    self.assertTrue(notified.is_set())
                    self.assertEqual(code, 130)
                    self.assertEqual(stop_attempts, 2, "the enclosing owner must retry the command stop")
                    self.assertIsNotNone(command)
                    self.assertIsNone(command.poll())
                    report_path, = (Path(tmp) / "runs").glob("*/report.json")
                    report = json.loads(report_path.read_text())
                    self.assertEqual(report["status"], "cancelled")
                    if phase == "binary resolution":
                        create_env.assert_not_called()
                        self.assertEqual(report["scenarios"], [])
                        self.assertEqual(report["phase"], phase)
                        entry = report
                        primary_error = entry["reason"]
                    else:
                        self.assertEqual(len(report["scenarios"]), 1)
                        entry = report["scenarios"][0]
                        self.assertEqual(entry["status"], "cancelled")
                        primary_error = entry["error"]
                        self.assertEqual(entry["retained_profile"], str(eval_env.root))
                        self.assertEqual(entry["retained_fixture"], str(fixture))
                        self.assertEqual(entry["daemon_pid"], eval_env.daemon.pid)
                        self.assertTrue(eval_env.root.exists())
                        self.assertTrue(fixture.exists())
                        self.assertIsNone(eval_env.daemon.poll())
                        if phase == "fixture setup":
                            self.assertEqual(
                                (fixture / "owned-input").read_text(),
                                "must survive a failed stop",
                            )
                    self.assertIn("cancelled by signal", primary_error)
                    self.assertEqual(entry["cleanup_error"], "command stop failed")
                    self.assertEqual(entry["command_pid"], command.pid)
                finally:
                    finished.set()
                    thread.join(timeout=1)
                    if command is not None:
                        original_stop(command, 0.2)
                    if eval_env is not None:
                        eval_env.cleanup()

    def test_first_signal_during_command_stop_finishes_owned_group_cleanup(self):
        script = """
import json, os, pathlib, subprocess, sys, time
ready = pathlib.Path(sys.argv[1] + '.ready')
child = subprocess.Popen([sys.executable, '-c',
    'import pathlib,signal,sys,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); '
    'pathlib.Path(sys.argv[1]).touch(); time.sleep(30)', str(ready)],
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
while not ready.exists(): time.sleep(0.01)
pathlib.Path(sys.argv[1]).write_text(json.dumps([os.getpid(), child.pid]))
"""
        with tempfile.TemporaryDirectory() as tmp:
            pidfile = Path(tmp) / "agent-pids.json"
            eval_env = run_real_model.create_eval_environment("signal-during-stop")
            self.addCleanup(eval_env.cleanup)
            interrupted = False
            original_signal_group = portable_process._signal_group

            def interrupt_stop(pid, sig):
                nonlocal interrupted
                original_signal_group(pid, sig)
                if not interrupted and sig == signal.SIGTERM and pidfile.exists():
                    leader, _child = json.loads(pidfile.read_text())
                    if pid == leader:
                        interrupted = True
                        os.kill(os.getpid(), signal.SIGTERM)

            def drive(*_args):
                run_real_model.run([sys.executable, "-c", script, str(pidfile)], timeout=20)
                self.fail("the interrupted command stop must cancel the scenario")

            try:
                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp) / "runs"),
                    mock.patch.object(run_real_model, "load_scenarios", return_value=[self.scenario()]),
                    mock.patch.object(run_real_model, "resolve_tracedecay_bin", return_value=self.make_bin()),
                    mock.patch.object(run_real_model, "create_eval_environment", return_value=eval_env),
                    mock.patch.object(run_real_model, "drive_hermes", side_effect=drive),
                    mock.patch.object(portable_process, "_signal_group", side_effect=interrupt_stop),
                    mock.patch.object(run_real_model, "PROCESS_STOP_GRACE_SECONDS", 0.2),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    code = run_real_model.main(["--agent-turn", "--i-understand-model-cost"])
                self.assertTrue(interrupted)
                self.assertEqual(code, 130)
                leader, child = json.loads(pidfile.read_text())
                self.assertFalse(portable_process._group_alive(leader), child)
                self.assertIsNone(eval_env.daemon)
                self.assertFalse(eval_env.root.exists())
                report_path, = (Path(tmp) / "runs").glob("*/report.json")
                entry = json.loads(report_path.read_text())["scenarios"][0]
                self.assertEqual(entry["status"], "cancelled")
                self.assertIn("cancelled by signal", entry["error"])
                self.assertNotIn("cleanup_error", entry)
            finally:
                if pidfile.exists():
                    leader, _child = json.loads(pidfile.read_text())
                    portable_process._stop_group(leader, 0.2)


    def test_daemon_cleanup_failure_retains_completed_results_and_owned_artifacts(self):
        binary = self.make_bin()
        for agent_failure, cleanup_failure in (
            (RuntimeError("agent failed"), OSError("cleanup failed")),
            (None, OSError("cleanup failed")),
            (None, KeyboardInterrupt("cancelled during cleanup")),
        ):
            with self.subTest(agent=agent_failure, cleanup=cleanup_failure):
                environment = run_real_model.create_eval_environment("cleanup-failure")
                self.addCleanup(environment.cleanup)
                daemon_pid = None
                outcomes = [{
                    "name": "completed assertion",
                    "passed": True,
                    "actual": 1,
                    "op": "eq",
                    "expected": 1,
                }]
                transcripts = [{"turn_valid": True}]

                def stop_then_fail(env):
                    nonlocal daemon_pid
                    daemon_pid = env.daemon.pid
                    run_real_model.stop_owned_process(env.daemon, 5)
                    raise cleanup_failure

                with tempfile.TemporaryDirectory() as tmp:
                    with (
                        mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                        mock.patch.object(run_real_model, "load_scenarios", return_value=[self.scenario()]),
                        mock.patch.object(run_real_model, "resolve_tracedecay_bin", return_value=binary),
                        mock.patch.object(run_real_model, "create_eval_environment", return_value=environment),
                        mock.patch.object(run_real_model, "drive_hermes", side_effect=agent_failure, return_value=transcripts),
                        mock.patch.object(run_real_model, "evaluate_assertions", return_value=outcomes),
                        mock.patch.object(run_real_model.EvalEnvironment, "stop_daemon", autospec=True, side_effect=stop_then_fail),
                        contextlib.redirect_stdout(io.StringIO()),
                        contextlib.redirect_stderr(io.StringIO()),
                    ):
                        cancelled = isinstance(cleanup_failure, KeyboardInterrupt)
                        self.assertEqual(
                            run_real_model.main(["--agent-turn", "--i-understand-model-cost"]),
                            130 if cancelled else 1,
                        )
                    report = json.loads(next(Path(tmp).glob("*/report.json")).read_text())
                self.assertEqual(report["status"], "cancelled" if cancelled else "fail")
                self.assertNotIn("reason", report)
                entry = report["scenarios"][0]
                if agent_failure is not None:
                    self.assertEqual(entry["error"], str(agent_failure))
                else:
                    if cancelled:
                        self.assertEqual(entry["error"], str(cleanup_failure))
                    else:
                        self.assertNotIn("error", entry)
                    self.assertEqual(entry["assertions"], outcomes)
                    self.assertEqual(entry["transcripts"], transcripts)
                self.assertIn(str(cleanup_failure), entry["cleanup_error"])
                # An interrupted/failed stop has not returned confirmation to
                # its owner, even when this injected failure follows a reap.
                self.assertTrue(environment.root.exists())
                self.assertTrue(environment.socket_dir.exists())
                self.assertEqual(entry["retained_profile"], str(environment.root))
                self.assertTrue(Path(entry["retained_fixture"]).exists())
                self.assertIsNotNone(environment.daemon)
                with self.assertRaises(ProcessLookupError):
                    os.kill(daemon_pid, 0)
                environment.cleanup()
                self.assertFalse(environment.root.exists())
                self.assertFalse(environment.socket_dir.exists())


class ProcessCancellationTest(unittest.TestCase):
    def test_stop_failure_still_kills_and_reaps_the_owned_process(self):
        process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(600)"],
            start_new_session=True,
        )
        try:
            with mock.patch.object(portable_process, "_stop_group", side_effect=OSError("stop failed")):
                with self.assertRaisesRegex(OSError, "stop failed"):
                    portable_process.stop_owned_process(process, 1)
            self.assertIsNotNone(process.returncode)
            with self.assertRaises(ProcessLookupError):
                os.kill(process.pid, 0)
        finally:
            portable_process.stop_owned_process(process, 1)

    def test_failed_forced_stop_preserves_the_stop_error_and_live_process_owner(self):
        process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(600)"],
            start_new_session=True,
        )
        stop_error = OSError("graceful stop failed")
        force_error = PermissionError("forced stop denied")
        try:
            with (
                mock.patch.object(portable_process, "_stop_group", side_effect=stop_error),
                mock.patch.object(portable_process, "_signal_group", side_effect=force_error),
                self.assertRaises(OSError) as raised,
            ):
                portable_process.stop_owned_process(process, 1)
            self.assertIs(raised.exception, stop_error)
            self.assertIs(raised.exception.__cause__, force_error)
            self.assertIsNone(process.poll())
            os.kill(process.pid, 0)
        finally:
            portable_process.stop_owned_process(process, 1)

    def test_signals_during_binary_resolution_and_turns_stop_owned_groups_and_write_reports(self):
        workload = """
import os, subprocess, sys, time
from pathlib import Path
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)'])
Path(sys.argv[1]).write_text(str(os.getpid()))
time.sleep(600)
"""
        entrypoint = """
import sys
from pathlib import Path
from unittest import mock
sys.path.insert(0, sys.argv[1])
import run_real_model as runner
root, phase = Path(sys.argv[2]), sys.argv[3]
runner.RUNS_DIR = root / 'runs'
scenario = {'id': 'cancel', 'setup': {}, 'assertions': []}
def work(*args, **kwargs):
    return runner.run([sys.executable, '-c', sys.argv[4], str(root / 'work.pid')])
with mock.patch.object(runner, 'load_scenarios', return_value=[scenario]):
    if phase == 'build':
        runner.resolve_tracedecay_bin = work
    else:
        runner.resolve_tracedecay_bin = lambda explicit: sys.argv[5]
        runner.drive_hermes = work
    sys.exit(runner.main(['--agent-turn', '--i-understand-model-cost']))
"""
        for phase in ("build", "turn"):
            for sig in (signal.SIGINT, signal.SIGTERM):
                with self.subTest(phase=phase, signal=sig), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    binary = root / "tracedecay"
                    binary.write_text(FAKE_BIN)
                    binary.chmod(0o755)
                    process = subprocess.Popen(
                        [sys.executable, "-c", entrypoint, str(MODULE_PATH.parent), tmp,
                         phase, workload, str(binary)],
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                        start_new_session=True,
                    )
                    group = None
                    try:
                        deadline = time.monotonic() + 15
                        while not (root / "work.pid").exists() and process.poll() is None:
                            if time.monotonic() >= deadline:
                                self.fail("owned workload did not start")
                            time.sleep(0.02)
                        self.assertTrue((root / "work.pid").exists())
                        group = int((root / "work.pid").read_text())
                        process.send_signal(sig)
                        output, _ = process.communicate(timeout=15)
                        self.assertEqual(process.returncode, 130, output)
                        report = json.loads(next((root / "runs").glob("*/report.json")).read_text())
                        self.assertEqual(report["status"], "cancelled", report)
                        if phase == "turn":
                            self.assertEqual(report["scenarios"][0]["status"], "cancelled")
                        self.assertEqual(
                            subprocess.run(
                                [sys.executable, "-S", str(run_real_model.PROCESS_HELPER),
                                 "group-alive", "--pid", str(group)],
                                check=False,
                            ).returncode,
                            1,
                            "a cancelled workload left runnable descendants",
                        )
                        with self.assertRaises(ProcessLookupError):
                            os.kill(group, 0)
                    finally:
                        run_real_model.stop_owned_process(process, 1)
                        if group is not None:
                            try:
                                os.killpg(group, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        if process.stdout is not None:
                            process.stdout.close()

    def test_timeout_stops_descendants_and_reaps_the_leader(self):
        with tempfile.TemporaryDirectory() as tmp:
            pid_path = Path(tmp) / "pid"
            with self.assertRaises(subprocess.TimeoutExpired):
                run_real_model.run(
                    [sys.executable, "-c",
                     "import os, subprocess, sys, time; "
                     "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)']); "
                     "open(sys.argv[1], 'w').write(str(os.getpid())); time.sleep(600)",
                     str(pid_path)], timeout=0.5,
                )
            pid = int(pid_path.read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)
            self.assertEqual(subprocess.run(
                [sys.executable, "-S", str(run_real_model.PROCESS_HELPER),
                 "group-alive", "--pid", str(pid)], check=False,
            ).returncode, 1)


class ReportLifecycleTest(unittest.TestCase):
    def test_command_preserves_captured_stderr_and_closes_both_pipes(self):
        processes = []
        original_popen = subprocess.Popen

        def spawn(*args, **kwargs):
            process = original_popen(*args, **kwargs)
            processes.append(process)
            return process

        with mock.patch.object(run_real_model.subprocess, "Popen", side_effect=spawn):
            result = run_real_model.run(
                [sys.executable, "-c", "import sys; print('output'); print('detail', file=sys.stderr)"],
                stderr=subprocess.PIPE,
            )
        self.assertEqual(result.stdout, "output\n")
        self.assertEqual(result.stderr, "detail\n")
        process, = processes
        self.assertTrue(process.stdout.closed)
        self.assertTrue(process.stderr.closed)

    def test_command_cleanup_failure_keeps_the_original_command_error(self):
        with mock.patch.object(
            run_real_model, "stop_owned_process", side_effect=OSError("stop failed")
        ):
            with self.assertRaises(run_real_model.OwnedProcessCleanupError) as raised:
                run_real_model.run([sys.executable, "-c", "print('original output'); exit(7)"])
        error = raised.exception
        self.assertIsInstance(error.original_error, run_real_model.EvalSetupError)
        self.assertIn("command failed (7)", error.primary_detail())
        self.assertIn("original output", error.primary_detail())
        self.assertEqual(str(error.cleanup_error), "stop failed")
        self.assertFalse(error.cancelled)
        self.assertEqual(error.process.returncode, 7)
        self.assertTrue(error.process.stdout.closed)

    def test_resolution_failure_and_cancellation_always_write_report(self):
        for error, expected_status, expected_code in (
            (run_real_model.EvalSetupError("build failed"), "fail", 1),
            (KeyboardInterrupt("build interrupted"), "cancelled", 130),
        ):
            with self.subTest(status=expected_status), tempfile.TemporaryDirectory() as tmp:
                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                    mock.patch.object(run_real_model, "load_scenarios", return_value=[{"id": "test"}]),
                    mock.patch.object(run_real_model, "resolve_tracedecay_bin", side_effect=error),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    code = run_real_model.main(["--agent-turn", "--i-understand-model-cost"])
                self.assertEqual(code, expected_code)
                report_path, = Path(tmp).glob("*/report.json")
                report = json.loads(report_path.read_text())
                self.assertEqual(report["status"], expected_status)
                self.assertEqual(report["reason"], str(error))
                self.assertEqual(report["phase"], "binary resolution")
                self.assertEqual(report["scenarios"], [])

    def test_cancellation_between_scenarios_and_during_report_write(self):
        class InterruptedScenarios(list):
            def __iter__(self):
                yield self[0]
                raise KeyboardInterrupt("between scenarios")

        for during_write in (False, True):
            with self.subTest(during_write=during_write), tempfile.TemporaryDirectory() as tmp:
                scenarios = [{"id": "first"}] if during_write else InterruptedScenarios([{"id": "first"}])
                original_write = Path.write_text
                attempted = False

                def write(path, contents, *args, **kwargs):
                    nonlocal attempted
                    if during_write and path.name == "report.json" and not attempted:
                        attempted = True
                        os.kill(os.getpid(), signal.SIGTERM)
                    return original_write(path, contents, *args, **kwargs)

                def run_scenario(_args, scenario, _run_dir, report):
                    report["scenarios"].append({"id": scenario["id"], "status": "pass"})
                    return True, False

                with (
                    mock.patch.object(run_real_model, "RUNS_DIR", Path(tmp)),
                    mock.patch.object(run_real_model, "load_scenarios", return_value=scenarios),
                    mock.patch.object(run_real_model, "resolve_tracedecay_bin", return_value="/unused"),
                    mock.patch.object(run_real_model, "run_scenario", side_effect=run_scenario),
                    mock.patch.object(Path, "write_text", new=write),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    self.assertEqual(run_real_model.main(["--agent-turn", "--i-understand-model-cost"]), 130)
                report_path, = Path(tmp).glob("*/report.json")
                report = json.loads(report_path.read_text())
                self.assertEqual(report["status"], "cancelled")
                self.assertEqual(report["scenarios"], [{"id": "first", "status": "pass"}])


class ExactFactStoreEvaluationTest(unittest.TestCase):
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
        with (
            tempfile.TemporaryDirectory() as fixture,
            mock.patch.object(
                run_real_model,
                "run",
                side_effect=lambda *args, **kwargs: next(responses),
            ),
        ):
            with self.assertRaisesRegex(
                run_real_model.ExactFactStoreError, "did not return FactId"
            ):
                run_real_model.seed_fixture(scenario, "tracedecay", {}, Path(fixture))

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
