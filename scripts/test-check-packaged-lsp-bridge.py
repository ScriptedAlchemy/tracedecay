#!/usr/bin/env python3
import importlib.util
import os
from pathlib import Path


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

    response = {"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {}}}
    assert MODULE.read_frame(MODULE.framed(response)) == response
    print(f"portable packaged LSP bridge acceptance passed on {os.name}")


if __name__ == "__main__":
    main()
