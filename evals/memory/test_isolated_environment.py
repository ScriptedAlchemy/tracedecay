import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import isolated_environment as runtime


@unittest.skipUnless(os.name == "posix", "the retained daemon harness uses Unix process groups")
class OwnedEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.environment = runtime.create_eval_environment("lifecycle")
        self.addCleanup(self.environment.cleanup)

    def command(self, text):
        script = self.environment.root / "controlled-command.py"
        script.write_text(f"#!{sys.executable} -S\n" + text)
        script.chmod(0o700)
        return str(script)

    def group_is_alive(self, process):
        return subprocess.run(
            [sys.executable, "-S", str(runtime.PROCESS_HELPER), "group-alive", "--pid", str(process.pid)],
            capture_output=True,
        ).returncode == 0

    def test_environment_discards_operator_profile_overrides(self):
        with mock.patch.dict(os.environ, {
            "TRACEDECAY_DAEMON_SOCKET": "/operator/daemon.sock",
            "TRACEDECAY_PROFILE": "operator",
            "TRACEDECAY_GLOBAL_DB": "/operator/private.db",
            "TRACEDECAY_DASHBOARD_TOKEN": "not-for-the-eval",
            "HERMES_HOME": "/operator/hermes",
        }):
            other = runtime.create_eval_environment("isolation")
        try:
            self.assertEqual(other.env["HOME"], str(other.home))
            self.assertEqual(other.env["TRACEDECAY_GLOBAL_DB"], str(other.global_db))
            self.assertTrue(Path(other.env["TRACEDECAY_DAEMON_SOCKET"]).is_relative_to(other.root))
            self.assertNotIn("TRACEDECAY_PROFILE", other.env)
            self.assertNotIn("TRACEDECAY_DASHBOARD_TOKEN", other.env)
            self.assertNotIn("HERMES_HOME", other.env)
        finally:
            other.cleanup()
        self.assertFalse(other.root.exists())

    def test_daemon_is_ready_before_use_and_reaped_before_profile_removal(self):
        binary = self.command("""import socket, sys
path = sys.argv[sys.argv.index('--socket') + 1]
listener = socket.socket(socket.AF_UNIX)
listener.bind(path)
listener.listen()
while True:
    peer, _ = listener.accept()
    peer.close()
""")
        self.environment.start_daemon(binary, self.environment.root / "daemon.log", ready_timeout=5)
        process = self.environment._daemon
        self.environment.require_daemon_alive()
        self.assertTrue(Path(self.environment.env["TRACEDECAY_DAEMON_SOCKET"]).exists())
        self.environment.cleanup()
        self.assertIsNotNone(process.poll())
        self.assertFalse(self.group_is_alive(process))
        self.assertFalse(self.environment.root.exists())

    def test_failed_startup_is_not_ready_and_cleanup_reaps_it(self):
        binary = self.command("raise SystemExit(23)\n")
        with self.assertRaisesRegex(RuntimeError, "readiness failed"):
            self.environment.start_daemon(binary, self.environment.root / "daemon.log", ready_timeout=0.3)
        process = self.environment._daemon
        self.environment.cleanup()
        self.assertEqual(process.returncode, 23)
        self.assertFalse(self.environment.root.exists())

    def test_a_successful_command_preserves_output(self):
        result = runtime.run([sys.executable, "-c", "print('measured')"], timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "measured\n")

    def test_failed_command_raises_a_catchable_exception_with_output(self):
        with self.assertRaises(subprocess.CalledProcessError) as error:
            runtime.run([sys.executable, "-c", "print('failed setup'); raise SystemExit(17)"], timeout=5)
        self.assertEqual(error.exception.returncode, 17)
        self.assertIn("failed setup", error.exception.output)
        result = runtime.run([sys.executable, "-c", "raise SystemExit(17)"], timeout=5, check=False)
        self.assertEqual(result.returncode, 17)

    def test_timeout_reaps_the_leader_and_its_descendants(self):
        processes = []
        spawn = runtime.spawn_owned_command
        def capture(*args, **kwargs):
            process = spawn(*args, **kwargs)
            processes.append(process)
            return process
        source = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-S','-c','import time; time.sleep(30)']); time.sleep(30)"
        with mock.patch.object(runtime, "spawn_owned_command", side_effect=capture):
            with self.assertRaises(subprocess.TimeoutExpired):
                runtime.run([sys.executable, "-S", "-c", source], timeout=0.5)
        self.assertEqual(len(processes), 1)
        self.assertIsNotNone(processes[0].poll())
        self.assertFalse(self.group_is_alive(processes[0]))

    def test_keyboard_interrupt_cleans_up_without_replacing_the_interrupt(self):
        process = runtime.spawn_owned_command([sys.executable, "-c", "import time; time.sleep(30)"])
        with mock.patch.object(runtime, "spawn_owned_command", return_value=process):
            with mock.patch.object(process, "communicate", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    runtime.run(["already-spawned"], timeout=5)
        self.assertIsNotNone(process.poll())
        self.assertFalse(self.group_is_alive(process))


if __name__ == "__main__":
    unittest.main()
