from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = PACKAGE_ROOT.parents[1]


def run(
    args: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout: int = 120,
) -> str:
    completed = subprocess.run(
        args,
        cwd=cwd,
        env=env,
        check=False,
        text=True,
        capture_output=True,
        timeout=timeout,
    )
    if completed.returncode != 0:
        raise AssertionError(
            f"{args[0]} exited {completed.returncode}:\n{completed.stderr}"
        )
    return completed.stdout


class InstalledPackageConformance(unittest.TestCase):
    def test_wheel_against_production_daemon(self) -> None:
        if os.name == "nt":
            self.fail("installed-package conformance requires Unix daemon sockets")
        binary = Path(
            os.environ.get(
                "TRACEDECAY_TEST_BIN",
                str(REPOSITORY_ROOT / "target/debug/tracedecay"),
            )
        ).resolve()
        self.assertTrue(binary.is_file(), f"missing production daemon binary: {binary}")

        with tempfile.TemporaryDirectory(prefix="tracedecay-python-sdk-") as raw:
            scratch = Path(raw)
            home = scratch / "home"
            profile = home / ".tracedecay"
            project = scratch / "project"
            wheelhouse = scratch / "wheelhouse"
            venv = scratch / "venv"
            socket = profile / "daemon.sock"
            authority_path = profile / "daemon-authority.json"
            for path in (home, project, wheelhouse):
                path.mkdir(parents=True)
            (project / "pyproject.toml").write_text(
                '[project]\nname="sdk-conformance-fixture"\nversion="0.0.0"\n',
                encoding="utf-8",
            )
            (project / "fixture.py").write_text("VALUE = True\n", encoding="utf-8")

            env = {
                **os.environ,
                "HOME": str(home),
                "USERPROFILE": str(home),
                "XDG_CONFIG_HOME": str(home / ".config"),
                "TRACEDECAY_DATA_DIR": str(profile),
                "TRACEDECAY_GLOBAL_DB": str(profile / "global.db"),
                "TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN": "1",
            }
            run(["git", "init", "--quiet"], cwd=project, env=env)
            run([str(binary), "init"], cwd=project, env=env)
            daemon = subprocess.Popen(
                [str(binary), "daemon", "run", "--socket", str(socket)],
                cwd=project,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                authority = self._wait_for_authority(daemon, authority_path)
                context = json.loads(
                    run(
                        [str(binary), "projects", "context", str(project), "--json"],
                        cwd=project,
                        env=env,
                    )
                )
                project_id = context["project"]["project_id"]

                external_wheel = os.environ.get("TRACEDECAY_SDK_WHEEL")
                if external_wheel:
                    # CI callers may build the wheel once and pass it in so
                    # conformance exercises that exact local artifact rather
                    # than a fresh rebuild.
                    wheels = [Path(external_wheel).resolve()]
                    self.assertTrue(
                        wheels[0].is_file(), f"missing prebuilt wheel: {wheels[0]}"
                    )
                else:
                    run(
                        [
                            sys.executable,
                            "-m",
                            "pip",
                            "wheel",
                            "--no-deps",
                            "--wheel-dir",
                            str(wheelhouse),
                            str(PACKAGE_ROOT),
                        ],
                        cwd=scratch,
                        env=os.environ.copy(),
                    )
                    wheels = list(wheelhouse.glob("tracedecay_sdk-*.whl"))
                    self.assertEqual(len(wheels), 1)
                run(
                    [sys.executable, "-m", "venv", str(venv)],
                    cwd=scratch,
                    env=os.environ.copy(),
                )
                python = venv / "bin/python"
                run(
                    [
                        str(python),
                        "-m",
                        "pip",
                        "install",
                        str(wheels[0]),
                    ],
                    cwd=scratch,
                    env=os.environ.copy(),
                )
                consumer = scratch / "consumer.py"
                consumer.write_text(self._consumer_script(), encoding="utf-8")
                output = run(
                    [str(python), str(consumer)],
                    cwd=scratch,
                    env={
                        **os.environ,
                        "TRACEDECAY_SDK_BASE_URL": (
                            f"http://{authority['http_application_endpoint']}"
                        ),
                        "TRACEDECAY_SDK_PROJECT_ID": project_id,
                        "TRACEDECAY_SDK_TOKEN": authority["auth_token"],
                    },
                )
                evidence = [json.loads(line) for line in output.splitlines()]
                self.assertEqual([item["mode"] for item in evidence], ["local"])
                self.assertTrue(all(item["terminal"] for item in evidence))
                self.assertTrue(
                    all(
                        item["cancellation"]
                        in {
                            "requested",
                            "already_requested",
                            "already_terminal",
                            "unavailable",
                        }
                        for item in evidence
                    )
                )
            finally:
                if daemon.poll() is None:
                    daemon.send_signal(signal.SIGINT)
                    try:
                        daemon.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=5)
                stderr = daemon.stderr.read() if daemon.stderr else ""
                if daemon.stderr is not None:
                    daemon.stderr.close()
                if daemon.returncode not in {0, -signal.SIGINT}:
                    self.fail(
                        f"production daemon exited {daemon.returncode}: {stderr}"
                    )

    def _wait_for_authority(
        self, daemon: subprocess.Popen[str], path: Path
    ) -> dict[str, Any]:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if daemon.poll() is not None:
                self.fail(f"production daemon exited during startup: {daemon.returncode}")
            if path.exists():
                try:
                    value = json.loads(path.read_text(encoding="utf-8"))
                    if (
                        isinstance(value.get("auth_token"), str)
                        and len(value["auth_token"]) == 64
                        and isinstance(value.get("http_application_endpoint"), str)
                    ):
                        return value
                except (OSError, json.JSONDecodeError):
                    pass
            time.sleep(0.025)
        self.fail(f"timed out waiting for daemon authority: {path}")

    def _consumer_script(self) -> str:
        return """
import json
import os
from tracedecay_sdk import (
    PageOptions,
    SERVER_OPERATIONS,
    StreamOptions,
    StreamResume,
    TraceDecayClient,
    TraceDecayProblemError,
    UNAVAILABLE_OPERATIONS,
    WORK_OPERATIONS,
)

base_url = os.environ["TRACEDECAY_SDK_BASE_URL"]
project_id = os.environ["TRACEDECAY_SDK_PROJECT_ID"]
token = os.environ["TRACEDECAY_SDK_TOKEN"]

for mode in ("local",):
    client = TraceDecayClient.local(base_url, project_id=project_id, token=token)
    server_operations = set(SERVER_OPERATIONS)
    available_operations = set(WORK_OPERATIONS)
    unavailable_operations = set(UNAVAILABLE_OPERATIONS)
    schema_unavailable_operations = {
        operation
        for operation, reason in UNAVAILABLE_OPERATIONS.items()
        if reason == "schema_unavailable"
    }
    route_unavailable_operations = {
        operation
        for operation, reason in UNAVAILABLE_OPERATIONS.items()
        if reason == "route_unavailable"
    }
    if (
        not SERVER_OPERATIONS
        or not UNAVAILABLE_OPERATIONS
        or server_operations
        != available_operations | schema_unavailable_operations
        or available_operations & unavailable_operations
    ):
        raise AssertionError("installed operation availability inventory drifted")
    expected_route_unavailable_operations = {
        "workflow_activate_definition",
        "workflow_handoff_issue",
        "workflow_handoff_redeem",
        "workflow_register_definition",
    }
    if (
        route_unavailable_operations != expected_route_unavailable_operations
        or server_operations & route_unavailable_operations
        or unavailable_operations
        != schema_unavailable_operations | route_unavailable_operations
    ):
        raise AssertionError("installed unavailable operation inventory drifted")
    if hasattr(client, "call") or not hasattr(client.operations, "work_snapshot"):
        raise AssertionError("only typed Work operations may be callable")
    attempt_finish = WORK_OPERATIONS.get("work_attempt_finish")
    if (
        attempt_finish is None
        or attempt_finish.operation_id != "operation.work.attempt_finish"
        or attempt_finish.route != "/application/work/attempt/finish"
        or attempt_finish.binding_id != "binding.http.work.attempt_finish"
        or attempt_finish.result_schema_id != "schema.work.attempt_finish.result"
        or attempt_finish.result_schema_revision != 1
        or attempt_finish.request_schema.get("title") != "WorkAttemptFinishRequestV1"
        or not hasattr(client.operations, "work_attempt_finish")
    ):
        raise AssertionError("installed package work_attempt_finish descriptor identity drifted")
    try:
        response = client.operations.work_snapshot(
            {"page_size": 1}, page=PageOptions(size=1)
        )
        request_id = response.request_id
    except TraceDecayProblemError as error:
        request_id = error.envelope["request_id"]
    try:
        initial = client.stream_operation(request_id)
        opened = next(initial)
        frontier = opened.data["data"]["frontier"]
        initial.close()
        resumed = list(
            client.stream_operation(
                request_id,
                StreamOptions(
                    resume=StreamResume(
                        token=frontier["resume_token"],
                        next_sequence=frontier["next_sequence"],
                    )
                )
            )
        )
        terminal = resumed[-1].terminal
    except TraceDecayProblemError as error:
        if error.code != "operation_event.unavailable":
            raise
        try:
            next(
                client.stream_operation(
                    request_id,
                    StreamOptions(
                        resume=StreamResume(
                            token="resume.unavailable", next_sequence=1
                        )
                    ),
                )
            )
            raise AssertionError("unavailable resume unexpectedly opened")
        except TraceDecayProblemError as resume_error:
            if resume_error.code != "operation_event.resume_expired":
                raise
        terminal = "unavailable"
    try:
        cancellation = client.cancel_operation(request_id)["status"]
    except TraceDecayProblemError as error:
        if error.code != "operation_event.unavailable":
            raise
        cancellation = "unavailable"
    print(
        json.dumps(
            {
                "mode": mode,
                "terminal": terminal,
                "cancellation": cancellation,
            }
        )
    )
"""


if __name__ == "__main__":
    unittest.main()
