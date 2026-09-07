#!/usr/bin/env python3
"""Fixture-driven Windows MCP stdio acceptance regression."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


SMOKE = Path(__file__).with_name("check-packaged-mcp-stdio.py")
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
    with tempfile.TemporaryDirectory() as temporary_directory:
        root = Path(temporary_directory)
        bin_directory = root / "bin"
        bin_directory.mkdir()
        binary = write_python_launcher(
            bin_directory / "tracedecay",
            """import sys
import time
from pathlib import Path

args = sys.argv[1:]
if args[:2] == ["daemon", "run"]:
    if "--socket" in args:
        socket = Path(args[args.index("--socket") + 1])
        socket.parent.mkdir(parents=True, exist_ok=True)
        socket.write_text("", encoding="utf-8")
    while True:
        time.sleep(60)
if args == ["init"]:
    raise SystemExit(0)
raise SystemExit(1)
""",
        )

        tools = [
            {"name": name, "inputSchema": {"type": "object"}} for name in REQUIRED_TOOLS
        ]
        write_python_launcher(
            bin_directory / "npx",
            f"""import json
import sys

arguments = sys.argv[1:]
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
