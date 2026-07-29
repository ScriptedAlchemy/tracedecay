#!/usr/bin/env python3
"""Unit tests for the line-oriented MCP benchmark client."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest import mock

from benchmarks.runtime.mcp_client import (
    MCPClient,
    MCPProtocolError,
    MCPTimeoutError,
    MCPToolError,
    JSONRPCError,
)


FAKE_SERVER = r"""
import json
import os
import signal
import subprocess
import sys
import threading
import time

if sys.argv[-2:] != ["serve", "--timings"]:
    raise SystemExit(f"unexpected arguments: {sys.argv!r}")

write_lock = threading.Lock()
initialized = threading.Event()
owned_children = []


def reap_owned_children(_signum, _frame):
    for child in owned_children:
        if child.poll() is None:
            child.kill()
        child.wait()
    raise SystemExit(0)


signal.signal(signal.SIGTERM, reap_owned_children)


def send(message):
    payload = json.dumps(
        message,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    with write_lock:
        sys.stdout.buffer.write(payload + b"\n")
        sys.stdout.buffer.flush()


def handle_call(request):
    request_id = request["id"]
    params = request["params"]
    name = params["name"]
    arguments = params.get("arguments", {})

    if name == "timeout":
        return
    if name == "delayed":
        time.sleep(arguments["delay"])
    if name == "notify_then_echo":
        send(
            {
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {"token": arguments["token"]},
            }
        )
    if name == "malformed":
        with write_lock:
            sys.stdout.buffer.write(b"{not-json}\n")
            sys.stdout.buffer.flush()
        return
    if name == "rpc_error":
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {
                    "code": -32042,
                    "message": "fake rpc failure",
                    "data": {"retryable": False},
                },
            }
        )
        return
    if name == "tool_error":
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {
                    "isError": True,
                    "content": [{"type": "text", "text": "fake tool failure"}],
                    "_meta": {"duration_us": 17},
                },
            }
        )
        return
    if name == "stderr_flood":
        sys.stderr.buffer.write(b"x" * (512 * 1024))
        sys.stderr.buffer.flush()
    if name == "spawn_stubborn_tree":
        child = subprocess.Popen(
            [
                sys.executable,
                "-c",
                (
                    "import signal,time;"
                    "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                    "time.sleep(60)"
                ),
            ]
        )
        owned_children.append(child)
        value = {"parent_pid": os.getpid(), "child_pid": child.pid}
    elif name == "pid":
        value = os.getpid()
    else:
        value = arguments.get("value", arguments)

    send(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [{"type": "text", "text": json.dumps(value)}],
                "structuredContent": {"value": value},
                "_meta": {"duration_us": 321},
            },
        }
    )


for raw_line in sys.stdin.buffer:
    request = json.loads(raw_line)
    method = request.get("method")
    if method == "initialize":
        if os.environ.get("FAKE_INITIALIZE_HANG") == "1":
            time.sleep(60)
        send(
            {
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "protocolVersion": request["params"]["protocolVersion"],
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake-tracedecay", "version": "1"},
                },
            }
        )
    elif method == "notifications/initialized":
        initialized.set()
    elif method == "tools/list":
        if not initialized.wait(1):
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {"code": -32000, "message": "not initialized"},
                }
            )
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": {
                        "tools": [
                            {
                                "name": "echo",
                                "description": "fake echo",
                                "inputSchema": {"type": "object"},
                            }
                        ]
                    },
                }
            )
    elif method == "tools/call":
        threading.Thread(target=handle_call, args=(request,), daemon=True).start()
"""


class MCPClientTest(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._temporary_directory.cleanup)
        self.server_path = Path(self._temporary_directory.name) / "fake_server.py"
        self.server_path.write_text(textwrap.dedent(FAKE_SERVER), encoding="utf-8")

    def client(self, **overrides: object) -> MCPClient:
        options: dict[str, object] = {
            "timeout": 0.5,
            "terminate_timeout": 0.1,
            "kill_timeout": 0.5,
        }
        options.update(overrides)
        return MCPClient((sys.executable, str(self.server_path)), **options)

    def assert_process_gone(self, pid: int) -> None:
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.01)
        self.fail(f"process {pid} is still alive")

    def test_start_performs_handshake_and_lists_tools(self) -> None:
        with self.client() as client:
            response = client.list_tools()

            self.assertEqual(response.result["tools"][0]["name"], "echo")
            self.assertEqual(
                client.initialize_result["serverInfo"]["name"],
                "fake-tracedecay",
            )
            self.assertEqual(
                client.command[-2:],
                ("serve", "--timings"),
            )

    def test_initialize_timeout_is_attributed_and_process_is_reaped(self) -> None:
        client = self.client(
            timeout=0.05,
            env={"FAKE_INITIALIZE_HANG": "1"},
        )

        with self.assertRaises(MCPTimeoutError) as raised:
            client.start()

        self.assertEqual(raised.exception.phase, "initialize")
        self.assertFalse(client.process_alive)
        self.assertEqual(client.process_start_count, 1)

    def test_out_of_order_responses_match_request_ids(self) -> None:
        with self.client(timeout=1) as client:
            with ThreadPoolExecutor(max_workers=2) as executor:
                slow = executor.submit(
                    client.call_tool,
                    "delayed",
                    {"delay": 0.15, "value": "slow"},
                )
                fast = executor.submit(
                    client.call_tool,
                    "delayed",
                    {"delay": 0.01, "value": "fast"},
                )

            self.assertEqual(
                slow.result().result["structuredContent"]["value"],
                "slow",
            )
            self.assertEqual(
                fast.result().result["structuredContent"]["value"],
                "fast",
            )

    def test_notifications_are_queued_without_stealing_response(self) -> None:
        with self.client() as client:
            response = client.call_tool(
                "notify_then_echo",
                {"token": "progress-1", "value": "done"},
            )
            notification = client.next_notification(timeout=0.2)

            self.assertEqual(response.result["structuredContent"]["value"], "done")
            self.assertEqual(notification.method, "notifications/progress")
            self.assertEqual(notification.params, {"token": "progress-1"})

    def test_malformed_line_fails_the_pending_request(self) -> None:
        with self.client() as client:
            with self.assertRaisesRegex(MCPProtocolError, "invalid JSON"):
                client.call_tool("malformed")

            self.assertTrue(client.process_alive)
            recovered = client.call_tool("echo", {"value": "recovered"})
            self.assertEqual(
                recovered.result["structuredContent"]["value"],
                "recovered",
            )

    def test_request_timeout_is_bounded(self) -> None:
        with self.client() as client:
            started = time.monotonic()
            with self.assertRaises(MCPTimeoutError) as raised:
                client.call_tool("timeout", timeout=0.05)

            self.assertLess(time.monotonic() - started, 0.4)
            self.assertEqual(raised.exception.phase, "tools/call")
            self.assertTrue(client.process_alive)
            self.assertEqual(client.process_start_count, 1)
            recovered = client.call_tool("echo", {"value": "still-alive"})
            self.assertEqual(
                recovered.result["structuredContent"]["value"],
                "still-alive",
            )

    def test_json_rpc_error_preserves_error_details(self) -> None:
        with self.client() as client:
            with self.assertRaises(JSONRPCError) as raised:
                client.call_tool("rpc_error")

            self.assertEqual(raised.exception.code, -32042)
            self.assertEqual(raised.exception.data, {"retryable": False})

    def test_tool_error_is_not_reported_as_an_empty_success(self) -> None:
        with self.client() as client:
            with self.assertRaises(MCPToolError) as raised:
                client.call_tool("tool_error")

            self.assertEqual(raised.exception.result["_meta"]["duration_us"], 17)
            self.assertIn("fake tool failure", str(raised.exception))

    def test_handler_timing_and_utf8_payload_sizes_are_extracted(self) -> None:
        with self.client() as client:
            response = client.call_tool("echo", {"value": "μ"})

            expected_request = {
                "jsonrpc": "2.0",
                "id": response.request_id,
                "method": "tools/call",
                "params": {"name": "echo", "arguments": {"value": "μ"}},
            }
            expected_response = {
                "jsonrpc": "2.0",
                "id": response.request_id,
                "result": response.result,
            }
            compact = {"ensure_ascii": False, "separators": (",", ":"), "sort_keys": True}
            self.assertEqual(response.handler_duration_us, 321)
            self.assertEqual(
                response.request_bytes,
                len(json.dumps(expected_request, **compact).encode("utf-8")),
            )
            self.assertEqual(
                response.response_bytes,
                len(json.dumps(expected_response, **compact).encode("utf-8")),
            )
            self.assertEqual(
                response.payload_bytes,
                response.request_bytes + response.response_bytes,
            )
            self.assertEqual(
                response.content_bytes,
                sum(
                    len(item["text"].encode("utf-8"))
                    for item in response.result["content"]
                ),
            )
            self.assertEqual(response.handler_duration_ns, 321_000)
            self.assertGreater(response.wall_duration_ns, 0)

    def test_persistent_process_is_reused(self) -> None:
        with self.client() as client:
            first = client.call_tool("pid").result["structuredContent"]["value"]
            second = client.call_tool("pid").result["structuredContent"]["value"]

            self.assertEqual(first, second)
            self.assertEqual(first, client.pid)
            self.assertEqual(client.process_start_count, 1)
            self.assertTrue(client.process_alive)

    def test_explicit_binary_command_is_spawned_without_a_shell(self) -> None:
        with mock.patch(
            "benchmarks.runtime.mcp_client.subprocess.Popen",
            wraps=subprocess.Popen,
        ) as popen:
            with self.client() as client:
                client.call_tool("echo")

        self.assertEqual(popen.call_count, 1)
        self.assertIs(popen.call_args.kwargs["shell"], False)
        self.assertEqual(
            tuple(popen.call_args.args[0])[-2:],
            ("serve", "--timings"),
        )

    def test_string_command_is_rejected_instead_of_shell_parsed(self) -> None:
        with self.assertRaisesRegex(TypeError, "sequence"):
            MCPClient(sys.executable)

    def test_concurrent_calls_share_one_process(self) -> None:
        with self.client(timeout=1) as client:
            with ThreadPoolExecutor(max_workers=8) as executor:
                futures = [
                    executor.submit(client.call_tool, "echo", {"value": index})
                    for index in range(8)
                ]

            self.assertEqual(
                [future.result().result["structuredContent"]["value"] for future in futures],
                list(range(8)),
            )

    def test_stderr_is_drained_without_blocking_stdout(self) -> None:
        with self.client(timeout=1) as client:
            response = client.call_tool("stderr_flood", {"value": "ok"})

            self.assertEqual(response.result["structuredContent"]["value"], "ok")
            self.assertGreater(client.stderr_bytes, 0)

    @unittest.skipUnless(hasattr(os, "killpg"), "requires POSIX process groups")
    def test_context_manager_kills_stubborn_process_group(self) -> None:
        with self.client() as client:
            value = client.call_tool("spawn_stubborn_tree").result["structuredContent"][
                "value"
            ]
            parent_pid = value["parent_pid"]
            child_pid = value["child_pid"]

        self.assert_process_gone(parent_pid)
        self.assert_process_gone(child_pid)


if __name__ == "__main__":
    unittest.main()
