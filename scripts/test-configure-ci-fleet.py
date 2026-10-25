#!/usr/bin/env python3
"""Exercise CI executor selection against real reachable and closed sockets."""

from __future__ import annotations

import importlib.util
import os
import shlex
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


HELPER = Path(__file__).parent.parent / ".github/actions/setup-bazel/configure_fleet.py"
SPEC = importlib.util.spec_from_file_location("configure_ci_fleet", HELPER)
assert SPEC is not None and SPEC.loader is not None
FLEET = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FLEET)


class ConfigureCiFleetTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.workspace = Path(self.temp.name)
        self.output = self.workspace / "output"
        self.rc = self.workspace / ".bazelrc.local"
        self.endpoint = socket.socket()
        self.addCleanup(self.endpoint.close)
        self.endpoint.bind(("127.0.0.1", 0))

    def configure(
        self, outcome: str = "success", runner_os: str = "Linux", arch: str = "X64"
    ) -> bool:
        host, port = self.endpoint.getsockname()
        return FLEET.configure_fleet(
            self.workspace, self.output, outcome, runner_os, arch, host, port
        )

    def options(self) -> dict[str, str]:
        return {
            option.partition("=")[0]: option.partition("=")[2]
            for line in self.rc.read_text().splitlines()
            for option in shlex.split(line, comments=True)[1:]
        }

    def test_reachable_executor_prefers_remote_rust_with_sandboxed_fallback(self) -> None:
        self.endpoint.listen()
        self.assertTrue(self.configure())
        options = self.options()
        self.assertEqual(options["--config"], "rbe-linux")
        self.assertIn("--remote_local_fallback", options)
        self.assertEqual(options["--remote_local_fallback_strategy"], "sandboxed")
        strategies = [
            line for line in self.rc.read_text().splitlines() if "--strategy=" in line
        ]
        self.assertIn("build --strategy=Rustc=remote", strategies)
        self.assertIn("build --strategy=TestRunner=dynamic,local", strategies)
        self.assertEqual(self.output.read_text(), "enabled=true\n")

    def test_offline_executor_clears_previous_fleet_selection(self) -> None:
        self.rc.write_text("build --remote_executor=grpc://old-executor:50051\n")
        self.assertFalse(self.configure())
        self.assertEqual(self.options(), {})
        self.assertEqual(self.output.read_text(), "enabled=false\n")

    def test_failed_enrollment_does_not_contact_executor(self) -> None:
        self.endpoint.listen()
        self.endpoint.setblocking(False)
        self.assertFalse(self.configure(outcome="failure"))
        self.assertEqual(self.options(), {})
        self.assertEqual(self.output.read_text(), "enabled=false\n")
        with self.assertRaises(BlockingIOError):
            self.endpoint.accept()

    def test_no_identity_keeps_hosted_configuration(self) -> None:
        self.assertFalse(self.configure(outcome="skipped"))
        self.assertEqual(self.options(), {})
        self.assertEqual(self.output.read_text(), "enabled=false\n")

    def test_configuration_write_failure_is_not_treated_as_fleet_outage(self) -> None:
        self.endpoint.listen()
        self.rc.mkdir()
        with self.assertRaises(OSError):
            self.configure()
        self.assertFalse(self.output.exists())

    def test_action_helper_runs_from_a_separate_release_checkout(self) -> None:
        result = subprocess.run(
            [sys.executable, str(HELPER.absolute())],
            cwd=self.workspace,
            env={
                **os.environ,
                "GITHUB_OUTPUT": str(self.output),
                "TAILSCALE_OUTCOME": "skipped",
                "RUNNER_OS": "Linux",
                "RUNNER_ARCH": "X64",
            },
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.options(), {})
        self.assertEqual(self.output.read_text(), "enabled=false\n")


if __name__ == "__main__":
    unittest.main()
