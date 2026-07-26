#!/usr/bin/env python3
import json
import os
import select
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import BinaryIO


TIMEOUT_SECONDS = 30


def fail(message: str) -> None:
    raise RuntimeError(f"distribution acceptance: {message}")


def framed(value: dict[str, object]) -> bytes:
    body = json.dumps(value, separators=(",", ":")).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body


def read_available(stream: BinaryIO, deadline: float) -> bytes:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        fail("packaged LSP bridge initialize response timed out")
    ready, _, _ = select.select([stream], [], [], remaining)
    if not ready:
        fail("packaged LSP bridge initialize response timed out")
    chunk = os.read(stream.fileno(), 8192)
    if not chunk:
        fail("packaged LSP bridge closed before initialize response")
    return chunk


def read_frame(stream: BinaryIO) -> dict[str, object]:
    deadline = time.monotonic() + TIMEOUT_SECONDS
    buffered = b""
    while b"\r\n\r\n" not in buffered:
        buffered += read_available(stream, deadline)
    header, buffered = buffered.split(b"\r\n\r\n", 1)
    lengths = [
        line.split(b":", 1)[1].strip()
        for line in header.split(b"\r\n")
        if line.lower().startswith(b"content-length:")
    ]
    if len(lengths) != 1 or not lengths[0].isdigit():
        fail("packaged LSP bridge returned invalid Content-Length framing")
    body_length = int(lengths[0])
    while len(buffered) < body_length:
        buffered += read_available(stream, deadline)
    try:
        value = json.loads(buffered[:body_length])
    except json.JSONDecodeError as error:
        fail(f"packaged LSP bridge returned invalid JSON: {error}")
    if not isinstance(value, dict):
        fail("packaged LSP bridge returned a non-object JSON-RPC response")
    return value


def terminate(process: subprocess.Popen[bytes] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def wait_for_daemon(socket_path: Path, daemon: subprocess.Popen[bytes]) -> None:
    deadline = time.monotonic() + TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if daemon.poll() is not None:
            fail(f"packaged daemon exited before bridge startup with {daemon.returncode}")
        try:
            client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            client.settimeout(0.1)
            client.connect(str(socket_path))
            client.close()
            return
        except OSError:
            time.sleep(0.05)
    fail("packaged daemon socket did not become ready")


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: check-packaged-lsp-bridge.py <binary> <work-directory>")
    binary = Path(sys.argv[1]).resolve()
    work = Path(sys.argv[2]).resolve()
    home = work / "home"
    project = work / "project"
    socket_path = home / ".tracedecay/daemon.sock"
    daemon_log = work / "daemon.log"
    bridge_log = work / "bridge.log"
    (project / "src").mkdir(parents=True)
    home.mkdir(parents=True)
    (project / "Cargo.toml").write_text(
        '[package]\nname = "distribution-lsp-smoke"\nversion = "0.0.0"\nedition = "2024"\n',
        encoding="utf-8",
    )
    (project / "src/lib.rs").write_text("pub fn answer() -> u8 { 42 }\n", encoding="utf-8")

    environment = os.environ.copy()
    environment["HOME"] = str(home)
    environment["USERPROFILE"] = str(home)
    environment["TRACEDECAY_DAEMON_SOCKET"] = str(socket_path)
    initialized = subprocess.run(
        [binary, "init", project],
        cwd=project,
        env=environment,
        capture_output=True,
        timeout=TIMEOUT_SECONDS,
        check=False,
    )
    if initialized.returncode != 0:
        fail(
            "packaged binary could not initialize bridge fixture:\n"
            + initialized.stderr.decode(errors="replace")
        )

    daemon: subprocess.Popen[bytes] | None = None
    bridge: subprocess.Popen[bytes] | None = None
    try:
        with daemon_log.open("wb") as daemon_stderr:
            daemon = subprocess.Popen(
                [binary, "daemon", "run", "--socket", socket_path],
                cwd=project,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=daemon_stderr,
            )
            wait_for_daemon(socket_path, daemon)
            with bridge_log.open("wb") as bridge_stderr:
                bridge = subprocess.Popen(
                    [binary, "lsp", "bridge", "--stdio", "--project", project],
                    cwd=project,
                    env=environment,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=bridge_stderr,
                )
                if bridge.stdin is None or bridge.stdout is None:
                    fail("packaged LSP bridge stdio was not connected")
                request = {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "processId": None,
                        "rootUri": project.as_uri(),
                        "capabilities": {
                            "general": {"positionEncodings": ["utf-16"]}
                        },
                    },
                }
                bridge.stdin.write(framed(request))
                bridge.stdin.flush()
                response = read_frame(bridge.stdout)
                if response.get("id") != 1 or not isinstance(response.get("result"), dict):
                    fail(f"packaged LSP bridge initialize failed: {response}")
                capabilities = response["result"].get("capabilities")
                if (
                    not isinstance(capabilities, dict)
                    or capabilities.get("positionEncoding") != "utf-16"
                ):
                    fail(
                        "packaged LSP bridge initialize omitted negotiated utf-16 capability"
                    )
    except Exception as error:
        daemon_output = (
            daemon_log.read_text(encoding="utf-8", errors="replace")
            if daemon_log.exists()
            else ""
        )
        bridge_output = (
            bridge_log.read_text(encoding="utf-8", errors="replace")
            if bridge_log.exists()
            else ""
        )
        raise SystemExit(
            f"{error}\ndaemon stderr:\n{daemon_output}\nbridge stderr:\n{bridge_output}"
        ) from error
    finally:
        terminate(bridge)
        terminate(daemon)

    print("distribution acceptance: packaged LSP bridge completed initialize handshake")


if __name__ == "__main__":
    main()
