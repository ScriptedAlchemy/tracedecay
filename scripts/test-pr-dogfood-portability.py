#!/usr/bin/env python3
"""Behavioral tests for the portable PR dogfood process harness."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest


ROOT = Path(__file__).resolve().parent.parent
HELPER = ROOT / "scripts/lib/portable_process.py"
DAEMON_HARNESS = ROOT / "scripts/with-isolated-tracedecay-daemon.sh"
DOGFOOD_SCRIPT = ROOT / "scripts/ci-pr-dogfood-smoke.sh"


def helper(*args: str, timeout: float = 10) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-S", str(HELPER), *args],
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def make_two_commit_project(directory: Path) -> tuple[Path, str, str]:
    project = directory / "project"
    project.mkdir()
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    hooks = directory / "empty-hooks"
    hooks.mkdir()
    subprocess.run(
        ["git", "-C", str(project), "config", "core.hooksPath", str(hooks)],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(project), "config", "user.name", "Dogfood Test"],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(project),
            "config",
            "user.email",
            "dogfood@example.invalid",
        ],
        check=True,
    )
    tracked = project / "fixture.txt"
    tracked.write_text("base\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(project), "add", "fixture.txt"], check=True)
    subprocess.run(
        ["git", "-C", str(project), "commit", "-qm", "base"], check=True
    )
    base_oid = subprocess.check_output(
        ["git", "-C", str(project), "rev-parse", "HEAD"], text=True
    ).strip()
    tracked.write_text("head\n", encoding="utf-8")
    subprocess.run(
        ["git", "-C", str(project), "commit", "-qam", "head"], check=True
    )
    head_oid = subprocess.check_output(
        ["git", "-C", str(project), "rev-parse", "HEAD"], text=True
    ).strip()
    return project, base_oid, head_oid


class PortableProcessTests(unittest.TestCase):
    def test_run_preserves_output_and_exit_status(self) -> None:
        completed = helper(
            "run",
            "--timeout",
            "2",
            "--kill-after",
            "0.2",
            "--",
            sys.executable,
            "-S",
            "-c",
            "import sys; print('phase stdout'); print('phase stderr', file=sys.stderr); sys.exit(7)",
        )
        self.assertEqual(completed.returncode, 7)
        self.assertEqual(completed.stdout, "phase stdout\n")
        self.assertEqual(completed.stderr, "phase stderr\n")

    def test_timeout_returns_124_and_kills_the_process_tree(self) -> None:
        with tempfile.TemporaryDirectory(prefix="portable-timeout-") as tmp:
            survived = Path(tmp) / "descendant-survived"
            descendant = textwrap.dedent(
                f"""
                import pathlib, signal, time
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                time.sleep(0.8)
                pathlib.Path({str(survived)!r}).write_text("survived", encoding="utf-8")
                """
            )
            parent = textwrap.dedent(
                f"""
                import signal, subprocess, sys, time
                subprocess.Popen([sys.executable, "-S", "-c", {descendant!r}])
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                time.sleep(10)
                """
            )
            started = time.monotonic()
            completed = helper(
                "run",
                "--timeout",
                "0.15",
                "--kill-after",
                "0.15",
                "--",
                sys.executable,
                "-S",
                "-c",
                parent,
                timeout=3,
            )
            elapsed = time.monotonic() - started
            self.assertEqual(completed.returncode, 124, completed.stderr)
            self.assertLess(elapsed, 1.5)
            time.sleep(0.7)
            self.assertFalse(survived.exists(), "timed-out descendant escaped cleanup")

    def test_successful_command_cannot_leave_background_work(self) -> None:
        with tempfile.TemporaryDirectory(prefix="portable-background-") as tmp:
            survived = Path(tmp) / "background-survived"
            descendant = textwrap.dedent(
                f"""
                import pathlib, signal, time
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                time.sleep(0.6)
                pathlib.Path({str(survived)!r}).write_text("survived", encoding="utf-8")
                """
            )
            parent = (
                "import subprocess, sys; "
                f"subprocess.Popen([sys.executable, '-S', '-c', {descendant!r}])"
            )
            completed = helper(
                "run",
                "--timeout",
                "2",
                "--kill-after",
                "0.1",
                "--",
                sys.executable,
                "-S",
                "-c",
                parent,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            time.sleep(0.6)
            self.assertFalse(survived.exists(), "background descendant escaped cleanup")

    def test_monotonic_clock_and_realpath_are_portable(self) -> None:
        first = helper("monotonic-ms")
        second = helper("monotonic-ms")
        self.assertEqual(first.returncode, 0)
        self.assertGreaterEqual(int(second.stdout), int(first.stdout))

        resolved = helper("realpath", str(HELPER.parent / ".." / "lib" / HELPER.name))
        self.assertEqual(resolved.returncode, 0)
        self.assertEqual(Path(resolved.stdout.strip()), HELPER.resolve())

    def test_group_probes_ignore_an_unreaped_zombie_leader(self) -> None:
        holder_source = textwrap.dedent(
            """
            import subprocess, sys, time
            from pathlib import Path

            leader = subprocess.Popen(
                [sys.executable, "-S", "-c", "raise SystemExit(0)"],
                start_new_session=True,
            )
            stat = Path(f"/proc/{leader.pid}/stat")
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if stat.is_file():
                    try:
                        state = stat.read_text().rsplit(")", 1)[1].split()[0]
                    except (OSError, IndexError):
                        state = ""
                    if state == "Z":
                        break
                    time.sleep(0.01)
                else:
                    # Non-Linux: give the leader a moment to finish exiting.
                    time.sleep(0.5)
                    break
            print(leader.pid, flush=True)
            # Hold the zombie: this parent never reaps; it exits when killed.
            time.sleep(30)
            """
        )
        holder = subprocess.Popen(
            [sys.executable, "-S", "-c", holder_source],
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            assert holder.stdout is not None
            leader_pid = int(holder.stdout.readline())
            alive = helper("group-alive", "--pid", str(leader_pid))
            self.assertEqual(
                alive.returncode,
                1,
                "an unreaped zombie leader must not count as a live group",
            )
            started = time.monotonic()
            stopped = helper(
                "stop-group", "--pid", str(leader_pid), "--grace", "5"
            )
            elapsed = time.monotonic() - started
            self.assertEqual(
                stopped.returncode,
                0,
                "a fully exited group must stop gracefully, not forced",
            )
            self.assertLess(
                elapsed,
                4,
                "stopping a fully exited group must not consume the grace period",
            )
        finally:
            holder.kill()
            holder.wait()
            if holder.stdout is not None:
                holder.stdout.close()

    def test_missing_command_has_bounded_error_output(self) -> None:
        completed = helper(
            "run",
            "--timeout",
            "1",
            "--kill-after",
            "0.1",
            "--",
            "/definitely/not/a/command",
        )
        self.assertEqual(completed.returncode, 127)
        self.assertIn("No such file or directory", completed.stderr)
        self.assertNotIn("Traceback", completed.stderr)


class IsolatedDaemonHarnessTests(unittest.TestCase):
    def make_fake_daemon(self, directory: Path) -> Path:
        daemon = directory / "fake-tracedecay"
        daemon.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env python3
                import os
                from pathlib import Path
                import signal
                import socket
                import subprocess
                import sys
                import time

                socket_path = sys.argv[sys.argv.index("--socket") + 1]
                print("fake daemon boot", flush=True)
                if os.environ.get("FAKE_DAEMON_NO_SOCKET") == "1":
                    time.sleep(30)
                    raise SystemExit(0)

                term_marker = os.environ.get("FAKE_DESCENDANT_TERM_MARKER")
                ready_marker = os.environ.get("FAKE_DESCENDANT_READY_MARKER")
                survived_marker = os.environ.get("FAKE_DESCENDANT_SURVIVED_MARKER")
                if term_marker and ready_marker and survived_marker:
                    child = '''
                from pathlib import Path
                import os, signal, sys, time
                def on_term(_signum, _frame):
                    Path(os.environ["FAKE_DESCENDANT_TERM_MARKER"]).write_text("term", encoding="utf-8")
                    raise SystemExit(0)
                signal.signal(signal.SIGTERM, on_term)
                Path(os.environ["FAKE_DESCENDANT_READY_MARKER"]).write_text("ready", encoding="utf-8")
                time.sleep(1.5)
                Path(os.environ["FAKE_DESCENDANT_SURVIVED_MARKER"]).write_text("survived", encoding="utf-8")
                '''
                    subprocess.Popen([sys.executable, "-S", "-c", child])
                    deadline = time.monotonic() + 2
                    while not Path(ready_marker).exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    if not Path(ready_marker).exists():
                        raise RuntimeError("fake daemon descendant did not start")

                listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                listener.bind(socket_path)
                listener.listen()
                if os.environ.get("FAKE_DAEMON_IGNORE_TERM") == "1":
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                else:
                    signal.signal(signal.SIGTERM, lambda _signum, _frame: sys.exit(0))
                while True:
                    listener.accept()[0].close()
                """
            ),
            encoding="utf-8",
        )
        daemon.chmod(0o755)
        return daemon

    def test_harness_waits_for_readiness_forwards_output_and_cleans_tree(self) -> None:
        with tempfile.TemporaryDirectory(prefix="daemon-harness-") as tmp:
            tmp_path = Path(tmp)
            daemon = self.make_fake_daemon(tmp_path)
            term_marker = tmp_path / "descendant-term"
            ready_marker = tmp_path / "descendant-ready"
            survived_marker = tmp_path / "descendant-survived"
            env = os.environ.copy()
            env["FAKE_DESCENDANT_TERM_MARKER"] = str(term_marker)
            env["FAKE_DESCENDANT_READY_MARKER"] = str(ready_marker)
            env["FAKE_DESCENDANT_SURVIVED_MARKER"] = str(survived_marker)
            profile_path_marker = tmp_path / "profile-path"
            env["FAKE_PROFILE_PATH_MARKER"] = str(profile_path_marker)
            completed = subprocess.run(
                [
                    str(DAEMON_HARNESS),
                    "--bin",
                    str(daemon),
                    "--ready-timeout",
                    "2",
                    "--stop-timeout",
                    "1",
                    "--lifecycle-label",
                    "fake daemon",
                    "--",
                    sys.executable,
                    "-S",
                    "-c",
                    "import os, pathlib; assert os.path.exists(os.environ['TRACEDECAY_DAEMON_SOCKET']); pathlib.Path(os.environ['FAKE_PROFILE_PATH_MARKER']).write_text(os.environ['TRACEDECAY_DATA_DIR'], encoding='utf-8'); print('smoke command output')",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=6,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("== starting fake daemon", completed.stdout)
            self.assertIn("smoke command output", completed.stdout)
            self.assertIn("== stopping fake daemon", completed.stderr)
            self.assertTrue(
                term_marker.exists(), "descendant did not receive group TERM"
            )
            time.sleep(0.6)
            self.assertFalse(
                survived_marker.exists(), "daemon descendant escaped group KILL"
            )
            profile_path = Path(profile_path_marker.read_text(encoding="utf-8"))
            self.assertFalse(profile_path.exists(), "isolated profile was retained")

    def test_graceful_cleanup_preserves_command_failure_status(self) -> None:
        with tempfile.TemporaryDirectory(prefix="daemon-graceful-") as tmp:
            daemon = self.make_fake_daemon(Path(tmp))
            completed = subprocess.run(
                [
                    str(DAEMON_HARNESS),
                    "--bin",
                    str(daemon),
                    "--ready-timeout",
                    "2",
                    "--stop-timeout",
                    "1",
                    "--",
                    sys.executable,
                    "-S",
                    "-c",
                    "raise SystemExit(7)",
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=5,
            )
            self.assertEqual(completed.returncode, 7, completed.stderr)

    def test_forced_cleanup_fails_an_otherwise_green_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="daemon-forced-") as tmp:
            daemon = self.make_fake_daemon(Path(tmp))
            env = os.environ.copy()
            env["FAKE_DAEMON_IGNORE_TERM"] = "1"
            completed = subprocess.run(
                [
                    str(DAEMON_HARNESS),
                    "--bin",
                    str(daemon),
                    "--ready-timeout",
                    "2",
                    "--stop-timeout",
                    "1",
                    "--lifecycle-label",
                    "fake daemon",
                    "--",
                    sys.executable,
                    "-S",
                    "-c",
                    "print('green command')",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=6,
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn("green command", completed.stdout)
            self.assertIn("== force stopping fake daemon", completed.stderr)
            self.assertIn("fake daemon boot", completed.stderr)

    def test_readiness_timeout_is_bounded_and_surfaces_daemon_output(self) -> None:
        with tempfile.TemporaryDirectory(prefix="daemon-not-ready-") as tmp:
            daemon = self.make_fake_daemon(Path(tmp))
            env = os.environ.copy()
            env["FAKE_DAEMON_NO_SOCKET"] = "1"
            started = time.monotonic()
            completed = subprocess.run(
                [
                    str(DAEMON_HARNESS),
                    "--bin",
                    str(daemon),
                    "--ready-timeout",
                    "1",
                    "--stop-timeout",
                    "1",
                    "--",
                    sys.executable,
                    "-S",
                    "-c",
                    "raise SystemExit('command must not run')",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=5,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertLess(time.monotonic() - started, 4)
            self.assertIn("did not become ready within 1 seconds", completed.stderr)
            self.assertIn("fake daemon boot", completed.stderr)
            self.assertNotIn("command must not run", completed.stderr)

    def test_shell_drivers_do_not_require_gnu_or_linux_only_commands(self) -> None:
        forbidden = ("setsid", "timeout --", "readlink -f", "date +%s%N")
        for script in (DAEMON_HARNESS, DOGFOOD_SCRIPT):
            source = script.read_text(encoding="utf-8")
            for token in forbidden:
                self.assertNotIn(token, source, f"{script.name} still requires {token}")
            syntax = subprocess.run(
                ["bash", "-n", str(script)],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(syntax.returncode, 0, syntax.stderr)


class DogfoodJourneyOutputTests(unittest.TestCase):
    def test_run_mode_polls_until_strict_readiness_then_validates_journey(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dogfood-output-") as tmp:
            tmp_path = Path(tmp)
            output = tmp_path / "output"
            output.mkdir()
            project, base_oid, head_oid = make_two_commit_project(tmp_path)

            fake_binary = tmp_path / "fake-tracedecay"
            fake_binary.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env bash
                    set -euo pipefail
                    if [[ "${1:-}" == "--version" ]]; then
                      echo "tracedecay fake-test"
                    elif [[ "${1:-}" == "init" ]]; then
                      :
                    elif [[ "${1:-}" == "status" ]]; then
                      count=0
                      if [[ -f "$FAKE_STATUS_COUNTER" ]]; then
                        count="$(cat "$FAKE_STATUS_COUNTER")"
                      fi
                      count="$((count + 1))"
                      echo "$count" >"$FAKE_STATUS_COUNTER"
                      if [[ "${FAKE_NEVER_READY:-0}" == "1" || "$count" -lt 3 ]]; then
                        echo '{"code_index_freshness":{"status":"current","worktree":{"coverage":"complete","staleness_state":"fresh","latest_generation_id":"generation.text-only"}},"graph_statistics":{"state":"unavailable","reason":"exact_scope_generation_not_ready"}}'
                      else
                        echo '{"code_index_freshness":{"status":"current","worktree":{"coverage":"complete","staleness_state":"fresh","latest_generation_id":"generation.ready","code_graph_serving":{"state":"ready"}}},"graph_statistics":{"state":"observed","generation_id":"generation.ready","symbol_count":2,"edge_count":1}}'
                      fi
                    elif [[ "${1:-} ${2:-}" == "tool context" ]]; then
                      echo '{"coverage":{"exact":"complete","lexical":"complete","graph":"complete","semantic":{"status":"unavailable","reason":"disabled"},"recall":"partial"},"search_matches":[{"file":"src/main.rs"}],"symbols":[{"node_id":"symbol:main"}]}'
                    elif [[ "${1:-} ${2:-}" == "tool pr_context" ]]; then
                      project=""
                      base=""
                      head=""
                      shift 2
                      while (($#)); do
                        case "$1" in
                          --project) project="$2"; shift 2 ;;
                          --base-ref) base="$2"; shift 2 ;;
                          --head-ref) head="$2"; shift 2 ;;
                          *) shift ;;
                        esac
                      done
                      base_oid="$(git -C "$project" rev-parse "$base^{commit}")"
                      head_oid="$(git -C "$project" rev-parse "$head^{commit}")"
                      merge_base="$(git -C "$project" merge-base "$base" "$head")"
                      printf '{"base_oid":"%s","head_oid":"%s","merge_base":"%s","graph_generation":"code-graph:sha256:ready-generation","files_changed":1,"changes":[{"path":"fixture.txt","status":"modified"}],"next_cursor":"pr-context.cursor.next","symbol_page":{"limit":1,"returned":1,"has_more":true,"complete":false,"selection":"stable_prefix","continuation_available":true},"analysis_coverage":{"seed_symbols_analyzed":1,"symbols_returned":1,"symbols_complete":false,"impact_nodes_admitted":2,"impact_nodes_returned":2,"direct_call_edges_admitted":1,"impact_bytes_admitted":256,"impact_partial":false,"complete":false}}\\n' \\
                        "$base_oid" "$head_oid" "$merge_base"
                    else
                      echo "unexpected fake TraceDecay arguments: $*" >&2
                      exit 2
                    fi
                    """
                ),
                encoding="utf-8",
            )
            fake_binary.chmod(0o755)
            env = os.environ.copy()
            env["TRACEDECAY_BIN"] = str(fake_binary)
            status_counter = tmp_path / "status-counter"
            env["FAKE_STATUS_COUNTER"] = str(status_counter)
            env["TRACEDECAY_DOGFOOD_READINESS_TIMEOUT"] = "1"
            env["TRACEDECAY_DOGFOOD_READINESS_POLL_INTERVAL"] = "0.05"
            completed = subprocess.run(
                [
                    str(DOGFOOD_SCRIPT),
                    "--run",
                    str(project),
                    base_oid,
                    head_oid,
                    str(output),
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=10,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(completed.stderr, "")
            for phase in ("init", "status", "context", "pr_context", "runtime_status"):
                self.assertRegex(
                    completed.stdout,
                    rf"tracedecay_ci_timing phase={phase} elapsed_ms=\d+ status=0",
                )
            self.assertIn(
                "TraceDecay PR dogfood pr_context output is valid", completed.stdout
            )
            self.assertIn("tracedecay_ci_readiness attempts=3", completed.stdout)
            self.assertEqual(status_counter.read_text(encoding="utf-8").strip(), "4")
            self.assertIn("tracedecay_ci_dogfood outcome=complete", completed.stdout)

    def test_run_mode_bounds_never_ready_graph_and_surfaces_last_reason(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dogfood-not-ready-") as tmp:
            tmp_path = Path(tmp)
            output = tmp_path / "output"
            output.mkdir()
            project, base_oid, head_oid = make_two_commit_project(tmp_path)

            status_counter = tmp_path / "status-counter"
            fake_binary = tmp_path / "fake-tracedecay"
            fake_binary.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env bash
                    set -euo pipefail
                    if [[ "${1:-}" == "--version" ]]; then
                      echo "tracedecay fake-test"
                    elif [[ "${1:-}" == "init" ]]; then
                      :
                    elif [[ "${1:-}" == "status" ]]; then
                      count=0
                      [[ ! -f "$FAKE_STATUS_COUNTER" ]] || count="$(cat "$FAKE_STATUS_COUNTER")"
                      echo "$((count + 1))" >"$FAKE_STATUS_COUNTER"
                      echo '{"code_index_freshness":{"status":"current","worktree":{"coverage":"complete","staleness_state":"fresh","latest_generation_id":"generation.text-only"}},"graph_statistics":{"state":"unavailable","reason":"exact_scope_generation_not_ready"}}'
                    else
                      echo "journey advanced before graph readiness" >&2
                      exit 2
                    fi
                    """
                ),
                encoding="utf-8",
            )
            fake_binary.chmod(0o755)
            env = os.environ.copy()
            env["TRACEDECAY_BIN"] = str(fake_binary)
            env["FAKE_STATUS_COUNTER"] = str(status_counter)
            env["TRACEDECAY_DOGFOOD_READINESS_TIMEOUT"] = "0.3"
            env["TRACEDECAY_DOGFOOD_READINESS_POLL_INTERVAL"] = "0.05"
            started = time.monotonic()
            completed = subprocess.run(
                [
                    str(DOGFOOD_SCRIPT),
                    "--run",
                    str(project),
                    base_oid,
                    head_oid,
                    str(output),
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=5,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertLess(time.monotonic() - started, 3)
            # Probe cadence inside a wall-clock deadline depends on runner
            # speed, so this test owns only the bounded failure and the
            # surfaced last reason; the retry-until-ready cadence is proven
            # deterministically by
            # test_run_mode_polls_until_strict_readiness_then_validates_journey.
            self.assertGreaterEqual(
                int(status_counter.read_text(encoding="utf-8").strip()), 1
            )
            self.assertIn("exact_scope_generation_not_ready", completed.stderr)
            self.assertNotIn("Traceback", completed.stderr)
            self.assertNotIn("tracedecay_ci_dogfood outcome=complete", completed.stdout)

    def test_run_mode_retains_valid_status_when_next_probe_is_malformed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dogfood-malformed-status-") as tmp:
            tmp_path = Path(tmp)
            output = tmp_path / "output"
            output.mkdir()
            project, base_oid, head_oid = make_two_commit_project(tmp_path)

            status_counter = tmp_path / "status-counter"
            fake_binary = tmp_path / "fake-tracedecay"
            fake_binary.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env bash
                    set -euo pipefail
                    if [[ "${1:-}" == "--version" ]]; then
                      echo "tracedecay fake-test"
                    elif [[ "${1:-}" == "init" ]]; then
                      :
                    elif [[ "${1:-}" == "status" ]]; then
                      count=0
                      [[ ! -f "$FAKE_STATUS_COUNTER" ]] || count="$(cat "$FAKE_STATUS_COUNTER")"
                      count="$((count + 1))"
                      echo "$count" >"$FAKE_STATUS_COUNTER"
                      if ((count == 1)); then
                        echo '{"code_index_freshness":{"status":"current","worktree":{"coverage":"complete","staleness_state":"fresh","latest_generation_id":"generation.valid-non-ready"}},"graph_statistics":{"state":"unavailable","reason":"exact_scope_generation_not_ready"}}'
                      else
                        printf '%s' '{"code_index_freshness":{"status":"current","worktree":{"latest_generation_id":"generation.malformed"'
                      fi
                    else
                      echo "unexpected fake TraceDecay arguments: $*" >&2
                      exit 2
                    fi
                    """
                ),
                encoding="utf-8",
            )
            fake_binary.chmod(0o755)
            env = os.environ.copy()
            env["TRACEDECAY_BIN"] = str(fake_binary)
            env["FAKE_STATUS_COUNTER"] = str(status_counter)
            env["TRACEDECAY_DOGFOOD_READINESS_TIMEOUT"] = "2"
            env["TRACEDECAY_DOGFOOD_READINESS_POLL_INTERVAL"] = "0.05"
            started = time.monotonic()
            completed = subprocess.run(
                [
                    str(DOGFOOD_SCRIPT),
                    "--run",
                    str(project),
                    base_oid,
                    head_oid,
                    str(output),
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=8,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertLess(time.monotonic() - started, 3)
            self.assertGreaterEqual(
                int(status_counter.read_text(encoding="utf-8").strip()), 2
            )
            self.assertIn("last complete status output", completed.stderr)
            self.assertIn('"latest_generation_id":"generation.valid-non-ready"', completed.stderr)
            self.assertNotIn("generation.malformed", completed.stderr)
            self.assertNotIn("Traceback", completed.stderr)
            self.assertNotIn("tracedecay_ci_dogfood outcome=complete", completed.stdout)


if __name__ == "__main__":
    unittest.main()
