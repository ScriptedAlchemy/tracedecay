"""Persistent line-oriented JSON-RPC client for MCP runtime benchmarks."""

from __future__ import annotations

import itertools
import json
import os
import queue
import subprocess
import threading
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, BinaryIO

from benchmarks.runtime.lifecycle import (
    LifecycleEvidence,
    enable_process_subreaper,
    terminate_process_group,
)


JsonObject = dict[str, Any]


class MCPProtocolError(RuntimeError):
    """The MCP peer emitted an invalid or unusable protocol message."""


class MCPTimeoutError(TimeoutError):
    """An MCP operation exceeded its bounded wall-time phase."""

    def __init__(
        self,
        phase: str,
        timeout: float,
        *,
        request_id: int | None = None,
    ) -> None:
        self.phase = phase
        self.timeout = timeout
        self.request_id = request_id
        request = "" if request_id is None else f" for request {request_id}"
        super().__init__(f"MCP timeout during {phase}{request} after {timeout:.3f}s")


class JSONRPCError(RuntimeError):
    """A JSON-RPC error response with its structured details preserved."""

    def __init__(
        self,
        *,
        request_id: int,
        code: int,
        message: str,
        data: Any = None,
    ) -> None:
        self.request_id = request_id
        self.code = code
        self.data = data
        super().__init__(f"JSON-RPC error {code}: {message}")


class MCPToolError(RuntimeError):
    """A tools/call result explicitly marked as an error."""

    def __init__(self, result: JsonObject) -> None:
        self.result = result
        messages = [
            item["text"]
            for item in result.get("content", ())
            if isinstance(item, dict) and isinstance(item.get("text"), str)
        ]
        detail = "\n".join(messages) if messages else "MCP tool returned isError"
        super().__init__(detail)


@dataclass(frozen=True)
class MCPNotification:
    """One server notification, kept separate from request responses."""

    method: str
    params: JsonObject


@dataclass(frozen=True)
class MCPResponse:
    """One matched response plus benchmark measurement evidence."""

    request_id: int
    result: JsonObject
    request_bytes: int
    response_bytes: int
    content_bytes: int
    wall_duration_ns: int
    handler_duration_us: int | float | None

    @property
    def payload_bytes(self) -> int:
        return self.request_bytes + self.response_bytes

    @property
    def handler_duration_ns(self) -> int | None:
        if self.handler_duration_us is None:
            return None
        return int(self.handler_duration_us * 1_000)


@dataclass
class _PendingRequest:
    request_bytes: int
    started_ns: int
    event: threading.Event = field(default_factory=threading.Event)
    response: tuple[JsonObject, int, int] | None = None
    error: BaseException | None = None


def _encode_message(message: Mapping[str, Any]) -> bytes:
    return json.dumps(
        message,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def _content_bytes(content: Any) -> int:
    if isinstance(content, list):
        return sum(_content_bytes(item) for item in content)
    if not isinstance(content, dict):
        return 0

    size = 0
    for key in ("text", "data", "blob"):
        value = content.get(key)
        if isinstance(value, str):
            size += len(value.encode("utf-8"))
    resource = content.get("resource")
    if isinstance(resource, (dict, list)):
        size += _content_bytes(resource)
    return size


class MCPClient:
    """Run one explicit prebuilt binary as a persistent MCP server."""

    def __init__(
        self,
        binary_command: Sequence[str | os.PathLike[str]],
        *,
        timeout: float = 10.0,
        terminate_timeout: float = 1.0,
        kill_timeout: float = 1.0,
        protocol_version: str = "2025-06-18",
        env: Mapping[str, str] | None = None,
    ) -> None:
        if isinstance(binary_command, (str, bytes, os.PathLike)):
            raise TypeError("binary_command must be a sequence, not a shell string")
        command = tuple(os.fspath(argument) for argument in binary_command)
        if not command:
            raise ValueError("binary_command must not be empty")
        if timeout <= 0 or terminate_timeout < 0 or kill_timeout <= 0:
            raise ValueError("timeouts must be positive")

        self.command = command + ("serve", "--timings")
        self.timeout = timeout
        self.terminate_timeout = terminate_timeout
        self.kill_timeout = kill_timeout
        self.protocol_version = protocol_version
        self._env = None if env is None else {**os.environ, **env}

        self.initialize_result: JsonObject = {}
        self._process: subprocess.Popen[bytes] | None = None
        self._process_start_count = 0
        self._ids = itertools.count(1)
        self._pending: dict[int, _PendingRequest] = {}
        self._pending_lock = threading.Lock()
        self._write_lock = threading.Lock()
        self._lifecycle_lock = threading.RLock()
        self._stderr_lock = threading.Lock()
        self._stderr_bytes = 0
        self._notifications: queue.Queue[MCPNotification] = queue.Queue()
        self._stdout_thread: threading.Thread | None = None
        self._stderr_thread: threading.Thread | None = None
        self._transport_error: MCPProtocolError | None = None
        self._closing = False
        self._closed = False

    def __enter__(self) -> MCPClient:
        return self.start()

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: object | None,
    ) -> None:
        self.close()

    @property
    def pid(self) -> int:
        if self._process is None:
            raise RuntimeError("MCP client has not been started")
        return self._process.pid

    @property
    def process_alive(self) -> bool:
        return self._process is not None and self._process.poll() is None

    @property
    def process_start_count(self) -> int:
        return self._process_start_count

    @property
    def stderr_bytes(self) -> int:
        with self._stderr_lock:
            return self._stderr_bytes

    def start(self) -> MCPClient:
        with self._lifecycle_lock:
            if self.process_alive:
                return self
            if self._closed:
                raise RuntimeError("MCP client is closed")
            if self._process is not None:
                raise MCPProtocolError("MCP server process exited before restart")

            enable_process_subreaper()
            self._process = subprocess.Popen(
                self.command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
                env=self._env,
                shell=False,
                start_new_session=os.name == "posix",
            )
            self._process_start_count += 1
            self._stdout_thread = threading.Thread(
                target=self._read_stdout,
                name=f"mcp-stdout-{self._process.pid}",
                daemon=True,
            )
            self._stderr_thread = threading.Thread(
                target=self._drain_stderr,
                name=f"mcp-stderr-{self._process.pid}",
                daemon=True,
            )
            self._stdout_thread.start()
            self._stderr_thread.start()

            try:
                initialized = self._request(
                    "initialize",
                    {
                        "protocolVersion": self.protocol_version,
                        "capabilities": {},
                        "clientInfo": {
                            "name": "tracedecay-runtime-benchmark",
                            "version": "1",
                        },
                    },
                    timeout=self.timeout,
                    phase="initialize",
                    ensure_started=False,
                )
                self.initialize_result = initialized.result
                self._send_notification("notifications/initialized", {})
            except BaseException:
                self.close()
                raise
        return self

    def list_tools(self, *, timeout: float | None = None) -> MCPResponse:
        return self._request(
            "tools/list",
            {},
            timeout=self.timeout if timeout is None else timeout,
            phase="tools/list",
        )

    def call_tool(
        self,
        name: str,
        arguments: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> MCPResponse:
        response = self._request(
            "tools/call",
            {"name": name, "arguments": dict(arguments or {})},
            timeout=self.timeout if timeout is None else timeout,
            phase="tools/call",
        )
        if response.result.get("isError") is True:
            raise MCPToolError(response.result)
        return response

    def next_notification(self, *, timeout: float | None = None) -> MCPNotification:
        wait = self.timeout if timeout is None else timeout
        try:
            return self._notifications.get(timeout=wait)
        except queue.Empty as exc:
            raise MCPTimeoutError("notification", wait) from exc

    def close(self) -> None:
        with self._lifecycle_lock:
            if self._closed:
                return
            self._closed = True
            self._closing = True
            self._fail_pending(MCPProtocolError("MCP client closed"))
            process = self._process
            if process is None:
                return

            self._terminate_process_group(process)
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None:
                    try:
                        stream.close()
                    except OSError:
                        pass
            for thread in (self._stdout_thread, self._stderr_thread):
                if thread is not None and thread is not threading.current_thread():
                    thread.join(timeout=self.kill_timeout)

    def _ensure_started(self) -> None:
        if not self.process_alive:
            self.start()
        if self._transport_error is not None:
            raise self._transport_error

    def _request(
        self,
        method: str,
        params: Mapping[str, Any],
        *,
        timeout: float,
        phase: str,
        ensure_started: bool = True,
    ) -> MCPResponse:
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        if ensure_started:
            self._ensure_started()

        request_id = next(self._ids)
        message = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": dict(params),
        }
        encoded = _encode_message(message)
        pending = _PendingRequest(
            request_bytes=len(encoded),
            started_ns=time.perf_counter_ns(),
        )
        with self._pending_lock:
            if self._transport_error is not None:
                raise self._transport_error
            self._pending[request_id] = pending

        try:
            self._write_line(encoded)
        except BaseException:
            with self._pending_lock:
                self._pending.pop(request_id, None)
            raise

        if not pending.event.wait(timeout):
            with self._pending_lock:
                self._pending.pop(request_id, None)
            raise MCPTimeoutError(phase, timeout, request_id=request_id)
        if pending.error is not None:
            raise pending.error
        if pending.response is None:
            raise MCPProtocolError(f"request {request_id} completed without a response")

        document, response_bytes, finished_ns = pending.response
        result = self._parse_response(document, request_id)
        handler_duration = self._handler_duration(result)
        return MCPResponse(
            request_id=request_id,
            result=result,
            request_bytes=pending.request_bytes,
            response_bytes=response_bytes,
            content_bytes=_content_bytes(result.get("content")),
            wall_duration_ns=max(1, finished_ns - pending.started_ns),
            handler_duration_us=handler_duration,
        )

    def _send_notification(self, method: str, params: Mapping[str, Any]) -> None:
        self._write_line(
            _encode_message(
                {
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": dict(params),
                }
            )
        )

    def _write_line(self, encoded: bytes) -> None:
        process = self._process
        if process is None or process.stdin is None or process.poll() is not None:
            raise MCPProtocolError("MCP server process is not writable")
        try:
            with self._write_lock:
                process.stdin.write(encoded + b"\n")
                process.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            error = MCPProtocolError(f"failed to write MCP request: {exc}")
            self._set_transport_error(error)
            raise error from exc

    def _read_stdout(self) -> None:
        process = self._process
        stream = None if process is None else process.stdout
        if stream is None:
            return
        try:
            for raw_line in stream:
                payload = raw_line.removesuffix(b"\n").removesuffix(b"\r")
                try:
                    decoded = payload.decode("utf-8")
                    document = json.loads(decoded)
                except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                    self._fail_pending(MCPProtocolError(f"invalid JSON response: {exc}"))
                    continue
                if not isinstance(document, dict):
                    self._fail_pending(
                        MCPProtocolError("JSON-RPC message must be an object")
                    )
                    continue

                if "id" not in document and "method" in document:
                    self._queue_notification(document)
                    continue
                request_id = document.get("id")
                if not isinstance(request_id, int) or isinstance(request_id, bool):
                    self._fail_pending(
                        MCPProtocolError("JSON-RPC response id must be an integer")
                    )
                    continue
                with self._pending_lock:
                    pending = self._pending.pop(request_id, None)
                if pending is None:
                    continue
                pending.response = (
                    document,
                    len(payload),
                    time.perf_counter_ns(),
                )
                pending.event.set()
        except (OSError, ValueError) as exc:
            if not self._closing:
                self._set_transport_error(
                    MCPProtocolError(f"failed to read MCP stdout: {exc}")
                )
        finally:
            if not self._closing:
                self._set_transport_error(
                    MCPProtocolError("MCP server stdout closed unexpectedly")
                )

    def _queue_notification(self, document: JsonObject) -> None:
        method = document.get("method")
        params = document.get("params", {})
        if not isinstance(method, str) or not isinstance(params, dict):
            self._fail_pending(MCPProtocolError("invalid JSON-RPC notification"))
            return
        self._notifications.put(MCPNotification(method=method, params=params))

    def _drain_stderr(self) -> None:
        process = self._process
        stream: BinaryIO | None = None if process is None else process.stderr
        if stream is None:
            return
        try:
            while chunk := stream.read(64 * 1024):
                with self._stderr_lock:
                    self._stderr_bytes += len(chunk)
        except (OSError, ValueError):
            return

    def _parse_response(self, document: JsonObject, request_id: int) -> JsonObject:
        if document.get("jsonrpc") != "2.0":
            raise MCPProtocolError(
                f"request {request_id} response has invalid jsonrpc version"
            )
        has_result = "result" in document
        has_error = "error" in document
        if has_result == has_error:
            raise MCPProtocolError(
                f"request {request_id} response must contain result or error"
            )
        if has_error:
            error = document["error"]
            if (
                not isinstance(error, dict)
                or not isinstance(error.get("code"), int)
                or isinstance(error.get("code"), bool)
                or not isinstance(error.get("message"), str)
            ):
                raise MCPProtocolError(
                    f"request {request_id} has an invalid JSON-RPC error"
                )
            raise JSONRPCError(
                request_id=request_id,
                code=error["code"],
                message=error["message"],
                data=error.get("data"),
            )
        result = document["result"]
        if not isinstance(result, dict):
            raise MCPProtocolError(f"request {request_id} result must be an object")
        return result

    def _handler_duration(self, result: JsonObject) -> int | float | None:
        metadata = result.get("_meta")
        if metadata is None:
            return None
        if not isinstance(metadata, dict):
            raise MCPProtocolError("tool result _meta must be an object")
        duration = metadata.get("duration_us")
        if duration is None:
            return None
        if (
            not isinstance(duration, (int, float))
            or isinstance(duration, bool)
            or duration < 0
        ):
            raise MCPProtocolError("tool result _meta.duration_us must be non-negative")
        return duration

    def _set_transport_error(self, error: MCPProtocolError) -> None:
        self._transport_error = error
        self._fail_pending(error)

    def _fail_pending(self, error: BaseException) -> None:
        with self._pending_lock:
            pending = tuple(self._pending.values())
            self._pending.clear()
        for request in pending:
            request.error = error
            request.event.set()

    def _terminate_process_group(self, process: subprocess.Popen[bytes]) -> None:
        try:
            terminate_process_group(
                process,
                process.pid,
                self.terminate_timeout,
                LifecycleEvidence(),
                kill_timeout=self.kill_timeout,
            )
        except subprocess.TimeoutExpired as exc:
            raise MCPProtocolError(
                f"MCP server process group did not exit after {self.kill_timeout:.3f}s"
            ) from exc
