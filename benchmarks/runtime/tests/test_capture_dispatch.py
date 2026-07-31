#!/usr/bin/env python3
"""Black-box capture dispatch tests for the runtime harness."""

from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

from benchmarks.runtime.schema import read_jsonl, validate_report


ROOT = Path(__file__).resolve().parents[3]
RUNNER = ROOT / "benchmarks" / "runtime" / "run.py"


def make_fake_binary(path: Path, *, tool_exit: int = 0) -> None:
    path.write_text(
        textwrap.dedent(
            f"""\
            #!/usr/bin/env python3
            import json
            import os
            import signal
            import socket
            import sys
            import time
            from pathlib import Path

            record = os.environ.get("TRACEDECAY_TEST_INVOCATIONS")
            if record:
                with Path(record).open("a", encoding="utf-8") as stream:
                    stream.write(json.dumps(sys.argv[1:]) + "\\n")

            if sys.argv[1:3] == ["daemon", "run"]:
                socket_path = Path(sys.argv[sys.argv.index("--socket") + 1])
                socket_path.parent.mkdir(parents=True, exist_ok=True)
                try:
                    socket_path.unlink()
                except FileNotFoundError:
                    pass
                server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                server.bind(str(socket_path))
                server.listen()
                signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
                while True:
                    connection, _ = server.accept()
                    connection.close()

            if sys.argv[1:2] == ["--version"]:
                print("tracedecay 0.0.0")
                raise SystemExit(0)

            if sys.argv[1:2] == ["init"]:
                raise SystemExit(0)

            if sys.argv[1:2] == ["tool"]:
                if {tool_exit}:
                    print("forced tool failure", file=sys.stderr)
                    raise SystemExit({tool_exit})
                print(json.dumps({{
                    "structuredContent": {{
                        "symbols": [{{"name": "fixture_catalog"}}]
                    }},
                    "_meta": {{"duration_us": 123}}
                }}, sort_keys=True))
                raise SystemExit(0)

            if sys.argv[1:2] == ["hook-cursor-after-shell"]:
                sys.stdin.buffer.read()
                raise SystemExit(0)

            if sys.argv[1:3] == ["lsp", "bridge"]:
                sys.stdin.buffer.read()
                payload = json.dumps({{
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {{
                        "uri": "file:///fixture/src/catalog.py",
                        "diagnostics": [],
                        "version": 1
                    }}
                }}, separators=(",", ":"), sort_keys=True).encode()
                sys.stdout.buffer.write(
                    f"Content-Length: {{len(payload)}}\\r\\n\\r\\n".encode() + payload
                )
                sys.stdout.buffer.flush()
                raise SystemExit(0)

            raise SystemExit("unexpected command")
            """
        ),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def make_fake_cargo(path: Path) -> None:
    path.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env python3
            import json
            import os
            import sys
            from pathlib import Path

            record = Path(os.environ["TRACEDECAY_TEST_CARGO_INVOCATIONS"])
            with record.open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(sys.argv[1:]) + "\\n")

            executable = os.environ["TRACEDECAY_TEST_AUTHORITY_BINARY"]
            print(json.dumps({
                "reason": "compiler-artifact",
                "target": {
                    "name": "diagnostic_publication_stress",
                    "kind": ["test"],
                },
                "profile": {"test": True},
                "executable": executable,
            }))
            """
        ),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def make_fake_authority_test_binary(path: Path) -> None:
    path.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env python3
            import json
            import os
            import sys
            from pathlib import Path

            record = Path(os.environ["TRACEDECAY_TEST_AUTHORITY_INVOCATIONS"])
            with record.open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(sys.argv[1:]) + "\\n")

            if os.environ.get("TRACEDECAY_FAKE_ZERO_SELECTED") == "1":
                print("running 0 tests")
                print("test result: ok. 0 passed; 0 failed; 0 ignored")
            else:
                print("running 1 test")
                print("test result: ok. 1 passed; 0 failed; 0 ignored")
            """
        ),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def run_runner(
    *arguments: os.PathLike[str] | str,
    environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, os.fspath(RUNNER), *(os.fspath(value) for value in arguments)],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
        timeout=20,
    )


class CaptureDispatchTest(unittest.TestCase):
    def test_capture_runs_owned_daemon_and_writes_valid_artifacts(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            binary = root / "fake-tracedecay"
            make_fake_binary(binary)
            output = root / "artifacts" / "capture.json"
            invocations = root / "invocations.jsonl"
            environment = os.environ.copy()
            environment["TRACEDECAY_TEST_INVOCATIONS"] = os.fspath(invocations)

            result = run_runner(
                "capture",
                "--binary",
                binary,
                "--output",
                output,
                environment=environment,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertIs(validate_report(report), report)
            samples = read_jsonl(output.with_suffix(".samples.jsonl"))
            self.assertEqual(len(samples), 1)
            self.assertEqual(samples[0]["outcome"]["status"], "success")
            self.assertTrue(samples[0]["lifecycle"]["daemon_survived"])
            self.assertTrue(samples[0]["observations"]["process_tree_reaped"])
            self.assertIn("wal_bytes", samples[0]["observations"])
            receipt = json.loads(
                output.with_suffix(".policy.json").read_text(encoding="utf-8")
            )
            self.assertEqual(receipt["policy_id"], "runtime-acceptance-v1")
            self.assertEqual(
                receipt["artifact_sha256"],
                hashlib.sha256(output.read_bytes()).hexdigest(),
            )
            commands = [
                json.loads(line)
                for line in invocations.read_text(encoding="utf-8").splitlines()
            ]
            self.assertTrue(any(command[:1] == ["init"] for command in commands))
            self.assertTrue(any(command[:2] == ["daemon", "run"] for command in commands))
            tool_command = next(command for command in commands if command[:1] == ["tool"])
            payload = json.loads(tool_command[tool_command.index("--args") + 1])
            self.assertEqual(payload["name"], "fixture_catalog")

    def test_capture_rejects_binary_that_does_not_match_prepared_fixture(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            prepared_binary = root / "prepared-binary"
            other_binary = root / "other-binary"
            make_fake_binary(prepared_binary)
            make_fake_binary(other_binary)
            other_binary.write_text(
                other_binary.read_text(encoding="utf-8") + "\n# different\n",
                encoding="utf-8",
            )
            prepared = root / "prepared"
            prepare_result = run_runner(
                "prepare",
                "--binary",
                prepared_binary,
                "--output",
                prepared,
            )
            self.assertEqual(prepare_result.returncode, 0, prepare_result.stderr)
            output = root / "capture.json"

            result = run_runner(
                "capture",
                "--binary",
                other_binary,
                "--prepared",
                prepared,
                "--output",
                output,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match", result.stderr)
            self.assertFalse(output.exists())

    def test_capture_records_command_failure_and_exits_nonzero(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            binary = root / "failing-tracedecay"
            make_fake_binary(binary, tool_exit=23)
            output = root / "capture.json"

            result = run_runner(
                "capture",
                "--binary",
                binary,
                "--output",
                output,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("tool command failed", result.stderr)
            self.assertFalse(output.exists())

    def test_paired_runs_same_input_abba_and_keeps_p95_pending(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            baseline = root / "baseline-tracedecay"
            treatment = root / "treatment-tracedecay"
            make_fake_binary(baseline)
            make_fake_binary(treatment)
            treatment.write_text(
                treatment.read_text(encoding="utf-8") + "\n# treatment\n",
                encoding="utf-8",
            )
            output = root / "paired.json"

            result = run_runner(
                "paired",
                "--baseline",
                baseline,
                "--treatment",
                treatment,
                "--samples-per-variant",
                "2",
                "--output",
                output,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            samples = read_jsonl(output.with_suffix(".samples.jsonl"))
            self.assertEqual(
                [sample["identity"]["variant"] for sample in samples],
                ["baseline", "treatment", "treatment", "baseline"],
            )
            self.assertEqual(
                [sample["identity"]["abba_position"] for sample in samples],
                [0, 1, 2, 3],
            )
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(report["fixture"]["same_input"], True)
            self.assertEqual(report["variants"]["baseline"]["sample_count"], 2)
            self.assertIsNotNone(report["variants"]["baseline"]["latency_ns"]["p50"])
            self.assertEqual(
                report["variants"]["baseline"]["latency_ns"]["p95"],
                {
                    "available": False,
                    "value": None,
                    "minimum_samples": 40,
                },
            )
            self.assertEqual(report["comparison"]["paired"]["pair_count"], 2)
            self.assertEqual(
                len(
                    report["comparison"]["paired"][
                        "log_ratio_confidence_interval"
                    ]
                ),
                2,
            )

    def test_prepare_creates_complete_immutable_fixture_snapshot(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            binary = root / "fake-tracedecay"
            make_fake_binary(binary)
            prepared = root / "prepared"

            result = run_runner(
                "prepare",
                "--binary",
                binary,
                "--output",
                prepared,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((prepared / "evidence" / "prepared.json").is_file())
            self.assertTrue((prepared / "home" / "workspace" / "runtime-fixture").is_dir())
            self.assertTrue((prepared / "bin" / "tracedecay").is_file())

    def test_missing_daemon_incident_driver_records_typed_unavailability(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            binary = root / "fake-tracedecay"
            make_fake_binary(binary)
            output = root / "missing-daemon.json"

            result = run_runner(
                "incident",
                "--binary",
                binary,
                "--workload",
                "missing-daemon-after-shell",
                "--samples",
                "2",
                "--output",
                output,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            samples = read_jsonl(output.with_suffix(".samples.jsonl"))
            self.assertEqual(len(samples), 2)
            self.assertTrue(
                all(sample["availability"]["state"] == "unavailable"
                    for sample in samples)
            )
            self.assertTrue(
                all(sample["lifecycle"]["daemon_survived"] is None
                    for sample in samples)
            )
            self.assertTrue(
                all(sample["observations"]["process_tree_reaped"]
                    for sample in samples)
            )
            self.assertTrue(
                all(
                    sample["observations"]["process_startup_control_ns"] >= 0
                    for sample in samples
                )
            )
            self.assertTrue(
                all(
                    sample["observations"]["hook_residual_ns"]
                    == max(
                        0,
                        sample["observations"]["direct_hook_wall_ns"]
                        - sample["observations"]["process_startup_control_ns"]
                    )
                    for sample in samples
                )
            )
            self.assertTrue(
                all(
                    sample["observations"]["lifecycle_wrapper_overhead_ns"]
                    == max(
                        0,
                        sample["observations"]["missing_daemon_fail_fast_ns"]
                        - sample["observations"]["direct_hook_wall_ns"],
                    )
                    for sample in samples
                )
            )

    def test_diagnostic_flood_driver_records_bounded_event_and_queue_counts(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            binary = root / "fake-tracedecay"
            make_fake_binary(binary)
            output = root / "diagnostic-flood.json"

            result = run_runner(
                "incident",
                "--binary",
                binary,
                "--workload",
                "diagnostic-dedup-batch-rate",
                "--samples",
                "1",
                "--events",
                "100",
                "--output",
                output,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            samples = read_jsonl(output.with_suffix(".samples.jsonl"))
            self.assertEqual(len(samples), 1)
            observations = samples[0]["observations"]
            self.assertEqual(observations["diagnostic_generated_count"], 100)
            self.assertEqual(observations["diagnostic_deduplicated_count"], 99)
            self.assertEqual(observations["diagnostic_batch_count"], 1)
            self.assertEqual(observations["queue_depth"], 1)
            self.assertTrue(observations["process_tree_reaped"])

    def test_diagnostic_authority_capture_is_typed_and_process_reaped(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            cargo = root / "cargo"
            make_fake_cargo(cargo)
            authority_binary = root / "diagnostic_publication_stress"
            make_fake_authority_test_binary(authority_binary)
            cargo_invocations = root / "cargo-invocations.jsonl"
            authority_invocations = root / "authority-invocations.jsonl"
            environment = dict(os.environ)
            environment["PATH"] = f"{root}{os.pathsep}{environment['PATH']}"
            environment["TRACEDECAY_TEST_CARGO_INVOCATIONS"] = str(
                cargo_invocations
            )
            environment["TRACEDECAY_TEST_AUTHORITY_BINARY"] = str(authority_binary)
            environment["TRACEDECAY_TEST_AUTHORITY_INVOCATIONS"] = str(
                authority_invocations
            )
            output = root / "diagnostic-authority.json"

            result = run_runner(
                "incident",
                "--workload",
                "diagnostic-dedup-batch-rate",
                "--authority-test",
                "--events",
                "10000",
                "--samples",
                "2",
                "--output",
                output,
                environment=environment,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(output.read_text())
            self.assertEqual(
                report["authority"]["kind"], "prebuilt-integration-test-scenario"
            )
            self.assertEqual(
                report["authority"]["target"], "diagnostic_publication_stress"
            )
            self.assertEqual(
                report["authority"]["scenario"],
                "publication_rate_and_queue_memory_stay_bounded_under_backpressure",
            )
            self.assertEqual(
                report["authority"]["anti_vacuity"],
                "scripts/require-exact-test.sh",
            )
            self.assertEqual(
                report["authority"]["timing_scope"],
                "test-executable-scenario-only",
            )
            self.assertNotIn("candidate_binary_sha256", report["authority"])
            self.assertEqual(report["sample_count"], 2)
            self.assertEqual(report["event_counts"]["attempted_total"], 20_000)
            self.assertEqual(report["event_counts"]["emitted_total"], 2)
            self.assertEqual(report["event_counts"]["queue_depth_max"], 1)
            self.assertEqual(report["outcome"]["process_leak_count"], 0)
            cargo_commands = [
                json.loads(line)
                for line in cargo_invocations.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(cargo_commands), 1)
            build_command = cargo_commands[0]
            self.assertEqual(build_command[0], "test")
            self.assertEqual(
                build_command[build_command.index("--test") + 1],
                "diagnostic_publication_stress",
            )
            self.assertIn("--no-run", build_command)
            self.assertIn("--message-format=json", build_command)
            authority_commands = [
                json.loads(line)
                for line in authority_invocations.read_text(
                    encoding="utf-8"
                ).splitlines()
            ]
            self.assertEqual(
                len(authority_commands),
                3,
                "one anti-vacuity validation plus two measured samples",
            )
            for command in authority_commands:
                self.assertEqual(
                    command,
                    [
                        "publication_rate_and_queue_memory_stay_bounded_under_backpressure",
                        "--exact",
                        "--test-threads=1",
                    ],
                )
                self.assertNotIn(
                    "cargo",
                    command,
                )
                self.assertNotIn(
                    "publication_rate_and_queue_memory_stay_bounded_under_backpressure",
                    build_command,
                )

    def test_diagnostic_authority_rejects_empty_libtest_selection(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-capture-test-") as directory:
            root = Path(directory)
            make_fake_cargo(root / "cargo")
            authority_binary = root / "diagnostic_publication_stress"
            make_fake_authority_test_binary(authority_binary)
            output = root / "diagnostic-authority.json"
            environment = dict(os.environ)
            environment["PATH"] = f"{root}{os.pathsep}{environment['PATH']}"
            environment["TRACEDECAY_TEST_CARGO_INVOCATIONS"] = str(
                root / "cargo-invocations.jsonl"
            )
            environment["TRACEDECAY_TEST_AUTHORITY_BINARY"] = str(authority_binary)
            authority_invocations = root / "authority-invocations.jsonl"
            environment["TRACEDECAY_TEST_AUTHORITY_INVOCATIONS"] = str(
                authority_invocations
            )
            environment["TRACEDECAY_FAKE_ZERO_SELECTED"] = "1"

            result = run_runner(
                "incident",
                "--workload",
                "diagnostic-dedup-batch-rate",
                "--authority-test",
                "--events",
                "10000",
                "--samples",
                "1",
                "--output",
                output,
                environment=environment,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "diagnostic publication stress scenario did not pass validation",
                result.stderr,
            )
            commands = [
                json.loads(line)
                for line in authority_invocations.read_text(
                    encoding="utf-8"
                ).splitlines()
            ]
            self.assertEqual(
                commands,
                [
                    [
                        "publication_rate_and_queue_memory_stay_bounded_under_backpressure",
                        "--exact",
                        "--test-threads=1",
                    ]
                ],
            )
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
