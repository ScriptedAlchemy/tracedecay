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

    def resource_limits(self) -> dict[str, str]:
        return dict(
            option.removeprefix("--local_resources=").split("=", 1)
            for line in self.rc.read_text().splitlines()
            for option in shlex.split(line, comments=True)[1:]
            if option.startswith("--local_resources=")
        )

    def test_linux_executor_shares_compilation_with_bounded_hosted_cpu(self) -> None:
        self.endpoint.listen()
        self.assertTrue(self.configure())
        options = self.options()
        self.assertEqual(options["--config"], "rbe-linux")
        self.assertIn("--noremote_local_fallback", options)
        self.assertNotIn("--remote_local_fallback", options)
        self.assertEqual(self.resource_limits(), {"cpu": "2", "memory": "HOST_RAM*.5"})
        self.assertEqual(options["--dynamic_local_strategy"], "sandboxed")
        self.assertEqual(options["--dynamic_remote_strategy"], "remote")
        self.assertNotIn("--experimental_local_lockfree_output", options)
        self.assertEqual(options["--remote_retries"], "10")
        strategies = [
            line for line in self.rc.read_text().splitlines() if "--strategy=" in line
        ]
        self.assertIn("build --strategy=Rustc=dynamic", strategies)
        self.assertIn("build --strategy=TestRunner=dynamic,local", strategies)
        self.assertEqual(self.output.read_text(), "enabled=true\n")

    def test_windows_executor_shares_compilation_with_bounded_hosted_cpu(self) -> None:
        self.endpoint.listen()
        self.assertTrue(self.configure(runner_os="Windows"))
        options = self.options()
        self.assertEqual(options["--config"], "rbe-windows")
        self.assertEqual(options["--jobs"], "16")
        self.assertEqual(options["--spawn_strategy"], "local")
        self.assertIn("--noremote_local_fallback", options)
        self.assertNotIn("--remote_local_fallback", options)
        self.assertEqual(
            self.resource_limits(), {"cpu": "2", "memory": "HOST_RAM*.5"}
        )
        host, port = self.endpoint.getsockname()
        self.assertEqual(options["--remote_executor"], f"grpc://{host}:{port}")
        self.assertEqual(options["--bes_backend"], f"grpc://{host}:1985")
        self.assertIn("--internal_spawn_scheduler", options)
        self.assertEqual(options["--dynamic_local_strategy"], "local")
        self.assertEqual(options["--dynamic_remote_strategy"], "remote")
        self.assertIn("--experimental_local_lockfree_output", options)
        self.assertIn("build --strategy=Rustc=dynamic", self.rc.read_text())
        self.assertNotIn("build --strategy=Rustc=remote", self.rc.read_text())
        self.assertIn("build --strategy=TestRunner=local", self.rc.read_text())
        self.assertEqual(self.output.read_text(), "enabled=true\n")

    def test_macos_executor_shares_compilation_with_bounded_hosted_cpu(self) -> None:
        self.endpoint.listen()
        self.assertTrue(self.configure(runner_os="macOS", arch="ARM64"))
        options = self.options()
        self.assertEqual(options["--config"], "rbe-macos")
        self.assertIn("--noremote_local_fallback", options)
        self.assertNotIn("--remote_local_fallback", options)
        self.assertEqual(self.resource_limits(), {"cpu": "2", "memory": "HOST_RAM*.5"})
        self.assertEqual(options["--dynamic_local_strategy"], "sandboxed")
        self.assertEqual(options["--dynamic_remote_strategy"], "remote")
        self.assertNotIn("--experimental_local_lockfree_output", options)
        self.assertIn("build --strategy=Rustc=dynamic", self.rc.read_text())

    def test_unsupported_windows_architecture_does_not_contact_executor(self) -> None:
        self.endpoint.listen()
        self.endpoint.setblocking(False)
        self.assertFalse(self.configure(runner_os="Windows", arch="ARM64"))
        self.assertEqual(self.options(), {})
        with self.assertRaises(BlockingIOError):
            self.endpoint.accept()

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
