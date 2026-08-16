#!/usr/bin/env python3
"""Cross-platform installed-binary MCP stdio transport acceptance."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


INSPECTOR_VERSION = "0.22.0"
REQUIRED_TOOLS = {
    "tracedecay_search",
    "tracedecay_diagnostics",
    "tracedecay_impact",
    "tracedecay_affected",
    "tracedecay_test_map",
}


def run(
    arguments: list[str],
    *,
    cwd: Path,
    environment: dict[str, str],
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        arguments,
        cwd=cwd,
        env=environment,
        check=check,
        capture_output=True,
        text=True,
        timeout=60,
    )


def inspect(
    npx: str,
    binary: Path,
    fixture: Path,
    environment: dict[str, str],
    *arguments: str,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    return run(
        [
            npx,
            "-y",
            f"@modelcontextprotocol/inspector@{INSPECTOR_VERSION}",
            "--cli",
            str(binary),
            "serve",
            "-p",
            str(fixture),
            *arguments,
        ],
        cwd=fixture,
        environment=environment,
        check=check,
    )


def terminate(process: subprocess.Popen[bytes] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        # SIGKILL cannot be refused; wait without a timeout so a slow reap
        # here can never mask the real failure being propagated.
        process.wait()


def wait_for_daemon_socket(
    daemon: subprocess.Popen[bytes], socket_path: Path
) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if daemon.poll() is not None:
            raise SystemExit(
                f"packaged daemon exited before init with {daemon.returncode}"
            )
        if socket_path.exists():
            return
        time.sleep(0.05)
    raise SystemExit("packaged daemon socket did not become ready")


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit(
            "usage: check-packaged-mcp-stdio.py TRACEDecay_BINARY WORK_DIRECTORY"
        )
    binary = Path(sys.argv[1]).resolve()
    work = Path(sys.argv[2]).resolve()
    npx = shutil.which("npx")
    if not binary.is_file():
        raise SystemExit(f"installed tracedecay binary is missing: {binary}")
    if npx is None:
        raise SystemExit("npx is required for MCP stdio acceptance")

    fixture = work / "project"
    home = work / "home"
    fixture.joinpath("src").mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    fixture.joinpath("src", "main.rs").write_text(
        'fn main() { println!("hello"); }\n', encoding="utf-8"
    )
    run(["git", "init", "--quiet"], cwd=fixture, environment=os.environ.copy())
    run(["git", "add", "src/main.rs"], cwd=fixture, environment=os.environ.copy())
    run(
        [
            "git",
            "-c",
            "user.name=TraceDecay MCP Smoke",
            "-c",
            "user.email=tracedecay-mcp-smoke@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed MCP smoke fixture",
        ],
        cwd=fixture,
        environment=os.environ.copy(),
    )

    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "XDG_DATA_HOME": str(home / ".local" / "share"),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "APPDATA": str(home / "AppData" / "Roaming"),
            "LOCALAPPDATA": str(home / "AppData" / "Local"),
        }
    )
    socket_path = home / ".tracedecay" / "daemon.sock"
    socket_path.parent.mkdir(parents=True, exist_ok=True)
    environment["TRACEDECAY_DAEMON_SOCKET"] = str(socket_path)
    daemon = subprocess.Popen(
        [str(binary), "daemon", "run", "--socket", str(socket_path)],
        cwd=fixture,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_for_daemon_socket(daemon, socket_path)
        run([str(binary), "init"], cwd=fixture, environment=environment)

        first = inspect(
            npx,
            binary,
            fixture,
            environment,
            "--method",
            "tools/list",
        )
        second = inspect(
            npx,
            binary,
            fixture,
            environment,
            "--method",
            "tools/list",
        )
        if first.stdout != second.stdout:
            raise SystemExit(
                "MCP tools/list output changed across identical invocations"
            )
        payload = json.loads(first.stdout)
        tools = payload.get("tools")
        if not isinstance(tools, list) or not tools:
            raise SystemExit("MCP tools/list returned no tools")
        names = {
            tool.get("name")
            for tool in tools
            if isinstance(tool, dict)
            and isinstance(tool.get("inputSchema"), dict)
            and tool["inputSchema"].get("type") == "object"
        }
        missing = sorted(REQUIRED_TOOLS - names)
        if missing:
            raise SystemExit(
                "MCP tools/list omitted required tools: " + ", ".join(missing)
            )

        diagnostics = inspect(
            npx,
            binary,
            fixture,
            environment,
            "--method",
            "tools/call",
            "--tool-name",
            "tracedecay_diagnostics",
            check=False,
        )
        try:
            diagnostic_payload = json.loads(diagnostics.stdout)
        except json.JSONDecodeError as error:
            raise SystemExit(
                "advertised tracedecay_diagnostics dispatch returned no typed payload: "
                + diagnostics.stderr.strip()
            ) from error
        content = diagnostic_payload.get("content")
        if not isinstance(content, list):
            raise SystemExit("tracedecay_diagnostics omitted typed MCP content")
        diagnostic_text = "\n".join(
            item.get("text", "")
            for item in content
            if isinstance(item, dict)
            and item.get("type") == "text"
            and isinstance(item.get("text"), str)
        )
        if not diagnostic_text:
            raise SystemExit("tracedecay_diagnostics returned no typed text result")
        lowered = diagnostic_text.lower()
        if diagnostic_payload.get("isError") is True:
            if "unavailable" not in lowered or not any(
                marker in lowered for marker in ("daemon", "authority", "service")
            ):
                raise SystemExit(
                    "tracedecay_diagnostics error was not typed as unavailable"
                )
        else:
            if diagnostics.returncode != 0:
                raise SystemExit(
                    "tracedecay_diagnostics successful payload exited nonzero"
                )
            if not any(
                marker in lowered
                for marker in ("diagnostic", "generation", "status", "unavailable")
            ):
                raise SystemExit(
                    "tracedecay_diagnostics success omitted concrete diagnostic state"
                )

        resources = inspect(
            npx,
            binary,
            fixture,
            environment,
            "--method",
            "resources/list",
        )
        resource_payload = json.loads(resources.stdout)
        if not any(
            resource.get("uri") == "tracedecay://status"
            for resource in resource_payload.get("resources", [])
            if isinstance(resource, dict)
        ):
            raise SystemExit("MCP resources/list omitted tracedecay://status")

        unknown = inspect(
            npx,
            binary,
            fixture,
            environment,
            "--method",
            "tools/call",
            "--tool-name",
            "definitely_not_a_tool",
            check=False,
        )
        if unknown.returncode == 0:
            raise SystemExit("unknown MCP tool unexpectedly succeeded")
    finally:
        terminate(daemon)

    print("packaged MCP stdio acceptance passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
