#!/usr/bin/env python3
import importlib.util
import os
from pathlib import Path
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).with_name("check-packaged-lsp-bridge.py")
SPEC = importlib.util.spec_from_file_location("check_packaged_lsp_bridge", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def main() -> None:
    binary = Path("tracedecay.exe")
    socket_path = Path("home/.tracedecay/daemon.sock")
    base_environment = {"HOME": "home"}

    windows_command = MODULE.daemon_command(binary, socket_path, platform_name="nt")
    windows_environment = MODULE.daemon_environment(
        base_environment, socket_path, platform_name="nt"
    )
    assert windows_command == [str(binary), "daemon", "run"]
    assert "TRACEDECAY_DAEMON_SOCKET" not in windows_environment

    unix_command = MODULE.daemon_command(binary, socket_path, platform_name="posix")
    unix_environment = MODULE.daemon_environment(
        base_environment, socket_path, platform_name="posix"
    )
    assert unix_command == [
        str(binary),
        "daemon",
        "run",
        "--socket",
        str(socket_path),
    ]
    assert unix_environment["TRACEDECAY_DAEMON_SOCKET"] == str(socket_path)

    project = Path("project")
    daemon = Mock()
    daemon.poll.return_value = None
    with (
        patch.object(MODULE.subprocess, "run") as run,
        patch.object(MODULE.time, "sleep"),
    ):
        run.side_effect = [Mock(returncode=1), Mock(returncode=0)]
        MODULE.wait_for_daemon(daemon, binary, project, windows_environment)
        assert run.call_count == 2, "an unready daemon must not pass readiness"
        for invocation in run.call_args_list:
            assert invocation.args == ([binary, "daemon", "status"],)
            assert invocation.kwargs["env"] == windows_environment
            assert invocation.kwargs["cwd"] == project
            assert 0 < invocation.kwargs["timeout"] <= MODULE.TIMEOUT_SECONDS

    with patch.object(MODULE.subprocess, "run") as run:
        run.side_effect = MODULE.subprocess.TimeoutExpired("daemon status", 30)
        try:
            MODULE.wait_for_daemon(daemon, binary, project, windows_environment)
        except RuntimeError as error:
            assert "endpoint did not become ready" in str(error)
        else:
            raise AssertionError("a stalled readiness probe must fail")

    daemon.poll.return_value = 7
    daemon.returncode = 7
    with patch.object(MODULE.subprocess, "run") as run:
        try:
            MODULE.wait_for_daemon(daemon, binary, project, windows_environment)
        except RuntimeError as error:
            assert "exited before bridge startup with 7" in str(error)
        else:
            raise AssertionError("an exited daemon must fail readiness")
        run.assert_not_called()

    response = {"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {}}}
    assert MODULE.read_frame(MODULE.framed(response)) == response
    print(f"portable packaged LSP bridge acceptance passed on {os.name}")


if __name__ == "__main__":
    main()
