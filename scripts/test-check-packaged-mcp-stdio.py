#!/usr/bin/env python3
"""Fixture-driven Windows MCP stdio acceptance regression."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


SMOKE = Path(__file__).with_name("check-packaged-mcp-stdio.py")
SPEC = importlib.util.spec_from_file_location("check_packaged_mcp_stdio", SMOKE)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {SMOKE}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
REQUIRED_TOOLS = [
    "tracedecay_search",
    "tracedecay_diagnostics",
    "tracedecay_impact",
    "tracedecay_affected",
    "tracedecay_test_map",
]


def write_executable(path: Path, source: str) -> None:
    path.write_text(source, encoding="utf-8")
    path.chmod(path.stat().st_mode | 0o111)


def write_python_launcher(path: Path, source: str) -> Path:
    if os.name != "nt":
        write_executable(path, f"#!/usr/bin/env python3\n{source}")
        return path

    python_source = path.with_suffix(".py")
    python_source.write_text(source, encoding="utf-8")
    launcher = path.with_suffix(".cmd")
    command = subprocess.list2cmdline([sys.executable, str(python_source)])
    launcher.write_text(f"@echo off\r\n{command} %*\r\n", encoding="utf-8")
    return launcher


def main() -> int:
    binary_path = Path("tracedecay.exe")
    socket_path = Path("home/.tracedecay/daemon.sock")
    assert MODULE.daemon_command(binary_path, socket_path, platform_name="nt") == [
        str(binary_path),
        "daemon",
        "run",
    ]
    assert "TRACEDECAY_DAEMON_SOCKET" not in MODULE.daemon_environment(
        {"TRACEDECAY_DAEMON_SOCKET": "fake"}, socket_path, platform_name="nt"
    )
    assert MODULE.daemon_command(binary_path, socket_path, platform_name="posix") == [
        str(binary_path),
        "daemon",
        "run",
        "--socket",
        str(socket_path),
    ]
    assert MODULE.daemon_environment(
        {"HOME": "home"}, socket_path, platform_name="posix"
    )["TRACEDECAY_DAEMON_SOCKET"] == str(socket_path)

    assert not MODULE.daemon_endpoint_is_ready(0, f"socket: {socket_path} (missing)")
    assert not MODULE.daemon_endpoint_is_ready(0, f"socket: {socket_path} (stale)")
    assert not MODULE.daemon_endpoint_is_ready(
        1, "endpoint: tcp://127.0.0.1:9 (connectable)"
    )
    assert MODULE.daemon_endpoint_is_ready(0, f"socket: {socket_path} (connectable)")
    assert MODULE.daemon_endpoint_is_ready(
        0, "endpoint: tcp://127.0.0.1:39181 (connectable)"
    )

    with tempfile.TemporaryDirectory() as temporary_directory:
        root = Path(temporary_directory)
        bin_directory = root / "bin"
        bin_directory.mkdir()
        binary = write_python_launcher(
            bin_directory / "tracedecay",
            """import os
import sys
import time
from pathlib import Path

args = sys.argv[1:]
ready = Path(os.environ["HOME"]) / ".tracedecay" / "fixture-daemon-ready"
if args[:2] == ["daemon", "run"]:
    ready.parent.mkdir(parents=True, exist_ok=True)
    ready.write_text("", encoding="utf-8")
    while True:
        time.sleep(60)
if args == ["daemon", "status"]:
    state = "connectable" if ready.exists() else "missing"
    print(f"endpoint: fixture ({state})")
    raise SystemExit(0)
if args == ["init"]:
    raise SystemExit(0)
if args == ["fixture-probe-daemon"]:
    raise SystemExit(0 if ready.exists() else 1)
raise SystemExit(1)
""",
        )

        tools = [
            {"name": name, "inputSchema": {"type": "object"}} for name in REQUIRED_TOOLS
        ]
        write_python_launcher(
            bin_directory / "npx",
            f"""import json
import subprocess
import sys

arguments = sys.argv[1:]
binary = arguments[arguments.index("--cli") + 1]
subprocess.run([binary, "fixture-probe-daemon"], check=True)
method = arguments[arguments.index("--method") + 1]
if method == "tools/list":
    print(json.dumps({{"tools": {json.dumps(tools)}}}))
elif method == "resources/list":
    print(json.dumps({{"resources": [{{"uri": "tracedecay://status"}}]}}))
elif method == "tools/call":
    tool = arguments[arguments.index("--tool-name") + 1]
    if tool == "tracedecay_diagnostics":
        print(json.dumps({{
            "isError": True,
            "content": [{{
                "type": "text",
                "text": "daemon diagnostic authority is unavailable",
            }}],
        }}))
        raise SystemExit(2)
    else:
        raise SystemExit(1)
else:
    raise SystemExit(1)
""",
        )

        environment = os.environ.copy()
        environment["PATH"] = str(bin_directory) + os.pathsep + environment["PATH"]
        completed = subprocess.run(
            [sys.executable, str(SMOKE), str(binary), str(root / "work")],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise SystemExit(completed.stderr or completed.stdout)
        if "packaged MCP stdio acceptance passed" not in completed.stdout:
            raise SystemExit("Windows MCP fixture did not complete the real-tool smoke")

    print("Windows packaged MCP stdio fixture passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
