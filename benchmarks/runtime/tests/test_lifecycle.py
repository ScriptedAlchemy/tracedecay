"""Tests for hermetic runtime process lifecycle and evidence capture."""

from __future__ import annotations

import json
import os
import signal
import socket
import sys
import tempfile
import unittest
from pathlib import Path


RUNTIME_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RUNTIME_ROOT))

import lifecycle  # noqa: E402


def fake_host(name: str) -> Path:
    return RUNTIME_ROOT / "fixtures" / "fake_hosts" / name


def reserve_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def dashboard_command(mode: str, port: int, *extra: str) -> list[str]:
    return [
        sys.executable,
        str(fake_host("dashboard_server.py")),
        "--mode",
        mode,
        "--port",
        str(port),
        *extra,
    ]


class DashboardLifecycleTests(unittest.TestCase):
    def test_warming_daemon_becomes_ready_and_survives_host_run(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)
            port = reserve_port()
            url = f"http://127.0.0.1:{port}/dashboard"
            daemon = lifecycle.OwnedDaemon(
                dashboard_command("warming", port, "--warmup-requests", "2"),
                env=os.environ,
                log_dir=root / "daemon-logs",
                readiness=lambda: lifecycle.probe_dashboard_once(
                    url,
                    request_timeout=0.05,
                ),
                readiness_timeout=2.0,
                poll_interval=0.01,
                termination_grace=0.1,
            )

            with daemon:
                self.assertEqual(daemon.evidence.activation_state, "active")
                self.assertEqual(daemon.evidence.restart_state, "not_required")
                self.assertEqual(daemon.evidence.availability_state, "available")
                self.assertEqual(daemon.evidence.availability_detail, None)
                self.assertIn("partial", daemon.evidence.readiness_availability_history)
                self.assertEqual(daemon.evidence.timeout_phase, None)
                self.assertEqual(daemon.evidence.process_count, 1)
                with self.assertRaisesRegex(lifecycle.LifecycleError, "already started"):
                    daemon.start()

                result = lifecycle.run_host(
                    [
                        sys.executable,
                        str(fake_host("verbose_host.py")),
                        "--capture-id",
                        "capture-fixed",
                        "--repeat-capture-id",
                        "2",
                        "--activated",
                    ],
                    env=os.environ,
                    log_dir=root / "host-logs",
                    input_payload=b'{"request":"fixture"}\n',
                    timeout=1.0,
                    termination_grace=0.1,
                    daemon=daemon,
                )

                self.assertTrue(result.evidence.daemon_survived)
                self.assertEqual(result.evidence.activation_state, "active")
                self.assertEqual(result.evidence.restart_state, "not_required")
                self.assertEqual(result.evidence.availability_state, "available")
                self.assertEqual(result.evidence.availability_detail, None)
                self.assertEqual(result.evidence.capture_id, "capture-fixed")
                self.assertEqual(result.evidence.capture_id_occurrences, 2)
                self.assertEqual(
                    result.evidence.request_payload_bytes,
                    len(b'{"request":"fixture"}\n'),
                )
                self.assertEqual(result.evidence.response_payload_bytes, len(result.stdout))
                self.assertGreater(result.evidence.end_to_end_host_wall_seconds, 0.0)
                self.assertGreater(result.evidence.end_to_end_hook_wall_seconds, 0.0)
                self.assertEqual(result.evidence.sample_count, 1)
                self.assertEqual(
                    result.evidence.measurement_class,
                    "n=1 regression sample",
                )

            self.assertEqual(daemon.evidence.process_count_after_cleanup, 0)
            self.assertTrue(daemon.evidence.term_sent)

    def test_unresponsive_daemon_times_out_during_response_and_is_cleaned_up(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)
            port = reserve_port()
            url = f"http://127.0.0.1:{port}/dashboard"
            daemon = lifecycle.OwnedDaemon(
                dashboard_command("unresponsive", port, "--delay", "5"),
                env=os.environ,
                log_dir=root / "daemon-logs",
                readiness=lambda: lifecycle.probe_dashboard_once(
                    url,
                    request_timeout=0.03,
                ),
                readiness_timeout=0.25,
                poll_interval=0.01,
                termination_grace=0.05,
            )

            with self.assertRaises(lifecycle.ReadinessTimeout) as raised:
                daemon.start()

            self.assertEqual(raised.exception.evidence.timeout_phase, "dashboard_response")
            self.assertEqual(raised.exception.evidence.availability_state, "unavailable")
            self.assertIsNotNone(raised.exception.evidence.availability_detail)
            self.assertEqual(raised.exception.evidence.process_count_after_cleanup, 0)
            self.assertTrue(raised.exception.evidence.term_sent)

    def test_dashboard_http_variants_remain_typed_failures(self) -> None:
        variants = {
            "malformed": ("dashboard_malformed", 200, "failed"),
            "no-content": ("dashboard_empty", 204, "unavailable"),
            "not-found": ("dashboard_http_404", 404, "unsupported"),
        }
        for mode, (expected_phase, expected_status, availability_state) in variants.items():
            with self.subTest(mode=mode):
                with tempfile.TemporaryDirectory(
                    prefix="runtime-lifecycle-test-"
                ) as directory:
                    root = Path(directory)
                    port = reserve_port()
                    url = f"http://127.0.0.1:{port}/dashboard"
                    daemon = lifecycle.OwnedDaemon(
                        dashboard_command(mode, port),
                        env=os.environ,
                        log_dir=root / "daemon-logs",
                        readiness=lambda: lifecycle.probe_dashboard_once(
                            url,
                            request_timeout=0.05,
                        ),
                        readiness_timeout=0.2,
                        poll_interval=0.01,
                        termination_grace=0.05,
                    )

                    with self.assertRaises(lifecycle.ReadinessTimeout) as raised:
                        daemon.start()

                    evidence = raised.exception.evidence
                    self.assertEqual(evidence.timeout_phase, expected_phase)
                    self.assertEqual(evidence.dashboard_status_code, expected_status)
                    self.assertEqual(evidence.activation_state, "unknown")
                    self.assertEqual(evidence.availability_state, availability_state)
                    self.assertIsNotNone(evidence.availability_detail)
                    self.assertEqual(evidence.process_count_after_cleanup, 0)


class HostLifecycleTests(unittest.TestCase):
    def test_verbose_hanging_child_is_drained_then_term_killed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)

            result = lifecycle.run_host(
                [
                    sys.executable,
                    str(fake_host("verbose_host.py")),
                    "--spawn-child",
                    "--child-lines",
                    "1500",
                    "--child-hang-seconds",
                    "5",
                    "--child-ignore-term",
                ],
                env=os.environ,
                log_dir=root / "host-logs",
                timeout=0.2,
                termination_grace=0.05,
                check=False,
            )

            self.assertEqual(result.evidence.timeout_phase, "child_io")
            self.assertEqual(result.evidence.availability_state, "failed")
            self.assertIsNotNone(result.evidence.availability_detail)
            self.assertTrue(result.evidence.term_sent)
            self.assertTrue(result.evidence.kill_sent)
            self.assertGreater(result.evidence.stdout_bytes, 32_000)
            self.assertGreater(result.evidence.stderr_bytes, 32_000)
            self.assertEqual(
                lifecycle.process_group_process_count(result.process_group_id),
                0,
            )
            self.assertEqual(
                result.evidence.stdout_bytes,
                result.stdout_log.stat().st_size,
            )
            self.assertEqual(
                result.evidence.stderr_bytes,
                result.stderr_log.stat().st_size,
            )

    def test_nonzero_host_error_carries_evidence_after_cleanup(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)

            with self.assertRaises(lifecycle.HostProcessError) as raised:
                lifecycle.run_host(
                    [
                        sys.executable,
                        str(fake_host("verbose_host.py")),
                        "--capture-id",
                        "capture-error",
                        "--exit-code",
                        "7",
                    ],
                    env=os.environ,
                    log_dir=root / "host-logs",
                    timeout=1.0,
                    termination_grace=0.05,
                )

            result = raised.exception.result
            self.assertEqual(result.evidence.exit_code, 7)
            self.assertEqual(result.evidence.capture_id, "capture-error")
            self.assertEqual(result.evidence.availability_state, "failed")
            self.assertEqual(
                lifecycle.process_group_process_count(result.process_group_id),
                0,
            )

    def test_repeated_capture_ids_are_accepted_but_conflicts_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)
            repeated = lifecycle.run_host(
                [
                    sys.executable,
                    str(fake_host("verbose_host.py")),
                    "--capture-id",
                    "capture-repeat",
                    "--repeat-capture-id",
                    "4",
                    "--restart-required",
                ],
                env=os.environ,
                log_dir=root / "repeated",
                timeout=1.0,
                termination_grace=0.05,
            )

            self.assertEqual(repeated.evidence.capture_id, "capture-repeat")
            self.assertEqual(repeated.evidence.capture_id_occurrences, 4)
            self.assertEqual(repeated.evidence.restart_state, "required")
            self.assertEqual(repeated.evidence.availability_state, "available")

            with self.assertRaises(lifecycle.HostProcessError) as raised:
                lifecycle.run_host(
                    [
                        sys.executable,
                        str(fake_host("verbose_host.py")),
                        "--capture-id",
                        "capture-first",
                        "--conflicting-capture-id",
                        "capture-second",
                    ],
                    env=os.environ,
                    log_dir=root / "conflicting",
                    timeout=1.0,
                    termination_grace=0.05,
                )

            self.assertIn("conflicting capture IDs", str(raised.exception))

    def test_signal_path_stops_the_owned_daemon_group(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)
            port = reserve_port()
            url = f"http://127.0.0.1:{port}/dashboard"
            daemon = lifecycle.OwnedDaemon(
                dashboard_command("ok", port),
                env=os.environ,
                log_dir=root / "daemon-logs",
                readiness=lambda: lifecycle.probe_dashboard_once(
                    url,
                    request_timeout=0.05,
                ),
                readiness_timeout=1.0,
                poll_interval=0.01,
                termination_grace=0.05,
            )
            daemon.start()
            process_group_id = daemon.process_group_id

            with self.assertRaises(lifecycle.LifecycleInterrupted) as raised:
                daemon.handle_signal(signal.SIGINT, None)

            self.assertEqual(raised.exception.signum, signal.SIGINT)
            self.assertEqual(
                lifecycle.process_group_process_count(process_group_id),
                0,
            )
            self.assertEqual(daemon.evidence.process_count_after_cleanup, 0)


class RunWorkspaceTests(unittest.TestCase):
    def test_workspace_cleanup_and_preserve_on_failure_are_explicit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-lifecycle-test-") as directory:
            root = Path(directory)
            normal = lifecycle.RunWorkspace(root, preserve_on_failure=False)
            with normal:
                normal_path = normal.path
                (normal.path / "evidence.json").write_text("{}\n", encoding="utf-8")
            self.assertFalse(normal_path.exists())

            failed = lifecycle.RunWorkspace(root, preserve_on_failure=False)
            with self.assertRaisesRegex(RuntimeError, "failed"):
                with failed:
                    failed_path = failed.path
                    raise RuntimeError("failed")
            self.assertFalse(failed_path.exists())

            preserved = lifecycle.RunWorkspace(root, preserve_on_failure=True)
            with self.assertRaisesRegex(RuntimeError, "preserve"):
                with preserved:
                    preserved_path = preserved.path
                    (preserved.path / "evidence.json").write_text(
                        json.dumps({"sample_count": 1}) + "\n",
                        encoding="utf-8",
                    )
                    raise RuntimeError("preserve")
            self.assertTrue(preserved_path.is_dir())
            self.assertEqual(
                json.loads((preserved_path / "evidence.json").read_text(encoding="utf-8")),
                {"sample_count": 1},
            )


if __name__ == "__main__":
    unittest.main()
