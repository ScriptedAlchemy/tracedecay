"""Hermetic process lifecycle and evidence capture for runtime samples."""

from __future__ import annotations

import ctypes
import json
import os
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from types import FrameType
from typing import Callable, Mapping, Sequence


class LifecycleError(RuntimeError):
    """A managed runtime process violated its lifecycle contract."""


class LifecycleInterrupted(LifecycleError):
    def __init__(self, signum: int) -> None:
        self.signum = signum
        super().__init__(f"lifecycle interrupted by signal {signum}")


class ReadinessTimeout(LifecycleError):
    def __init__(self, evidence: LifecycleEvidence) -> None:
        self.evidence = evidence
        super().__init__(f"daemon readiness timed out in {evidence.timeout_phase}")


class HostProcessError(LifecycleError):
    def __init__(self, result: HostResult, message: str | None = None) -> None:
        self.result = result
        super().__init__(message or f"host exited with status {result.evidence.exit_code}")


@dataclass(frozen=True)
class ProbeResult:
    ready: bool
    phase: str
    availability_state: str
    availability_detail: str | None
    activation_state: str = "unknown"
    restart_state: str = "not_applicable"
    status_code: int | None = None
    payload_bytes: int = 0


@dataclass
class LifecycleEvidence:
    sample_count: int = 1
    measurement_class: str = "n=1 regression sample"
    process_count: int = 0
    process_count_after_cleanup: int = 0
    activation_state: str = "unknown"
    restart_state: str = "not_applicable"
    availability_state: str = "unavailable"
    availability_detail: str | None = "not observed"
    readiness_availability_history: list[str] = field(default_factory=list)
    timeout_phase: str | None = None
    dashboard_status_code: int | None = None
    daemon_survived: bool | None = None
    end_to_end_host_wall_seconds: float = 0.0
    end_to_end_hook_wall_seconds: float = 0.0
    request_payload_bytes: int = 0
    response_payload_bytes: int = 0
    stdout_bytes: int = 0
    stderr_bytes: int = 0
    capture_id: str | None = None
    capture_id_occurrences: int = 0
    term_sent: bool = False
    kill_sent: bool = False
    exit_code: int | None = None


@dataclass(frozen=True)
class HostResult:
    evidence: LifecycleEvidence
    stdout: bytes
    stderr: bytes
    stdout_log: Path
    stderr_log: Path
    process_group_id: int


class RunWorkspace:
    """A disposable run directory with explicit failure preservation."""

    def __init__(self, parent: Path, *, preserve_on_failure: bool) -> None:
        parent = Path(parent)
        parent.mkdir(parents=True, exist_ok=True)
        self.path = Path(tempfile.mkdtemp(prefix="runtime-run-", dir=parent))
        self.preserve_on_failure = preserve_on_failure

    def __enter__(self) -> RunWorkspace:
        return self

    def __exit__(
        self,
        exception_type: type[BaseException] | None,
        exception: BaseException | None,
        traceback: object,
    ) -> bool:
        if exception_type is None or not self.preserve_on_failure:
            shutil.rmtree(self.path, ignore_errors=True)
        return False


class OwnedDaemon:
    """Exactly one daemon process group with bounded readiness and shutdown."""

    def __init__(
        self,
        command: Sequence[str],
        *,
        env: Mapping[str, str],
        log_dir: Path,
        readiness: Callable[[], ProbeResult],
        readiness_timeout: float,
        poll_interval: float,
        termination_grace: float,
    ) -> None:
        self.command = tuple(command)
        self.env = dict(env)
        self.log_dir = Path(log_dir)
        self.readiness = readiness
        self.readiness_timeout = readiness_timeout
        self.poll_interval = poll_interval
        self.termination_grace = termination_grace
        self.evidence = LifecycleEvidence()
        self.process: subprocess.Popen[bytes] | None = None
        self.process_group_id = 0
        self._started = False
        self._threads: list[threading.Thread] = []
        self._logs: list[object] = []

    @property
    def is_alive(self) -> bool:
        return self.process is not None and self.process.poll() is None

    def start(self) -> OwnedDaemon:
        if self._started:
            raise LifecycleError("daemon process group already started")
        self._started = True
        enable_process_subreaper()
        self.log_dir.mkdir(parents=True, exist_ok=True)
        stdout_log = (self.log_dir / "stdout.log").open("wb")
        stderr_log = (self.log_dir / "stderr.log").open("wb")
        self._logs = [stdout_log, stderr_log]
        self.process = subprocess.Popen(
            self.command,
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        self.process_group_id = os.getpgid(self.process.pid)
        self.evidence.process_count = max(
            1, process_group_process_count(self.process_group_id)
        )
        self._threads = [
            _start_drain(self.process.stdout, stdout_log),
            _start_drain(self.process.stderr, stderr_log),
        ]

        deadline = time.monotonic() + self.readiness_timeout
        last_probe = ProbeResult(
            ready=False,
            phase="dashboard_connect",
            availability_state="unavailable",
            availability_detail="dashboard not contacted",
        )
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.evidence.availability_state = "failed"
                self.evidence.availability_detail = "daemon exited before readiness"
                self.stop()
                raise LifecycleError("daemon exited before readiness")
            last_probe = self.readiness()
            self._record_probe(last_probe)
            if last_probe.ready:
                return self
            time.sleep(self.poll_interval)

        self.evidence.timeout_phase = last_probe.phase
        self.stop()
        raise ReadinessTimeout(self.evidence)

    def stop(self) -> None:
        if self.process is None:
            return
        terminate_process_group(
            self.process,
            self.process_group_id,
            self.termination_grace,
            self.evidence,
        )
        for thread in self._threads:
            thread.join(timeout=1.0)
        for log in self._logs:
            log.close()
        self.evidence.process_count_after_cleanup = process_group_process_count(
            self.process_group_id
        )

    def handle_signal(self, signum: int, frame: FrameType | None) -> None:
        del frame
        self.stop()
        raise LifecycleInterrupted(signum)

    def __enter__(self) -> OwnedDaemon:
        return self.start()

    def __exit__(
        self,
        exception_type: type[BaseException] | None,
        exception: BaseException | None,
        traceback: object,
    ) -> bool:
        self.stop()
        return False

    def _record_probe(self, probe: ProbeResult) -> None:
        self.evidence.availability_state = probe.availability_state
        self.evidence.availability_detail = probe.availability_detail
        self.evidence.activation_state = probe.activation_state
        self.evidence.restart_state = probe.restart_state
        self.evidence.dashboard_status_code = probe.status_code
        self.evidence.readiness_availability_history.append(
            probe.availability_state
        )


def probe_dashboard_once(url: str, *, request_timeout: float) -> ProbeResult:
    """Make one bounded dashboard probe with typed availability."""

    try:
        with urllib.request.urlopen(url, timeout=request_timeout) as response:
            status = response.status
            body = response.read()
    except urllib.error.HTTPError as error:
        status = error.code
        body = error.read()
    except (TimeoutError, socket.timeout):
        return ProbeResult(
            False,
            "dashboard_response",
            "unavailable",
            "dashboard response timed out",
        )
    except urllib.error.URLError as error:
        if isinstance(error.reason, (TimeoutError, socket.timeout)):
            return ProbeResult(
                False,
                "dashboard_response",
                "unavailable",
                "dashboard response timed out",
            )
        return ProbeResult(
            False,
            "dashboard_connect",
            "unavailable",
            "dashboard connection unavailable",
        )

    if status == 204:
        return ProbeResult(
            False,
            "dashboard_empty",
            "unavailable",
            "dashboard returned no content",
            status_code=status,
        )
    if status == 404:
        return ProbeResult(
            False,
            "dashboard_http_404",
            "unsupported",
            "dashboard route is unsupported",
            status_code=status,
            payload_bytes=len(body),
        )
    if status != 200:
        availability = "partial" if status == 503 else "unavailable"
        phase = "dashboard_warming" if status == 503 else f"dashboard_http_{status}"
        return ProbeResult(
            False,
            phase,
            availability,
            f"dashboard returned HTTP {status}",
            status_code=status,
            payload_bytes=len(body),
        )
    try:
        document = json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return ProbeResult(
            False,
            "dashboard_malformed",
            "failed",
            "dashboard response was malformed",
            status_code=status,
            payload_bytes=len(body),
        )
    if not isinstance(document, dict):
        return ProbeResult(
            False,
            "dashboard_malformed",
            "failed",
            "dashboard response was not an object",
            status_code=status,
            payload_bytes=len(body),
        )
    activated = document.get("activated") is True
    restart_required = document.get("restart_required") is True
    return ProbeResult(
        True,
        "ready",
        "available",
        None,
        activation_state="active" if activated else "inactive",
        restart_state="required" if restart_required else "not_required",
        status_code=status,
        payload_bytes=len(body),
    )


def run_host(
    command: Sequence[str],
    *,
    env: Mapping[str, str],
    log_dir: Path,
    timeout: float,
    termination_grace: float,
    input_payload: bytes = b"",
    daemon: OwnedDaemon | None = None,
    check: bool = True,
) -> HostResult:
    """Run one host group while concurrently draining both output streams."""

    enable_process_subreaper()
    log_dir = Path(log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    stdout_log = log_dir / "stdout.log"
    stderr_log = log_dir / "stderr.log"
    stdout_buffer = bytearray()
    stderr_buffer = bytearray()
    evidence = LifecycleEvidence(request_payload_bytes=len(input_payload))
    started = time.monotonic()
    with stdout_log.open("wb") as stdout_handle, stderr_log.open("wb") as stderr_handle:
        process = subprocess.Popen(
            tuple(command),
            env=dict(env),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        process_group_id = os.getpgid(process.pid)
        evidence.process_count = max(
            1, process_group_process_count(process_group_id)
        )
        threads = [
            _start_drain(process.stdout, stdout_handle, stdout_buffer),
            _start_drain(process.stderr, stderr_handle, stderr_buffer),
        ]
        if process.stdin is not None:
            try:
                process.stdin.write(input_payload)
                process.stdin.flush()
            except BrokenPipeError:
                pass
            finally:
                process.stdin.close()
        timed_out = False
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            evidence.process_count = max(
                evidence.process_count,
                process_group_process_count(process_group_id),
            )
            evidence.timeout_phase = (
                "child_io" if evidence.process_count > 1 else "host_wait"
            )
            evidence.availability_state = "failed"
            evidence.availability_detail = "host process timed out"
            terminate_process_group(
                process,
                process_group_id,
                termination_grace,
                evidence,
            )
        else:
            _reap_process_group(process_group_id)
        for thread in threads:
            thread.join(timeout=1.0)

    elapsed = time.monotonic() - started
    stdout = bytes(stdout_buffer)
    stderr = bytes(stderr_buffer)
    evidence.exit_code = process.returncode
    evidence.stdout_bytes = len(stdout)
    evidence.stderr_bytes = len(stderr)
    evidence.response_payload_bytes = len(stdout)
    evidence.end_to_end_host_wall_seconds = elapsed
    evidence.end_to_end_hook_wall_seconds = elapsed
    evidence.daemon_survived = daemon.is_alive if daemon is not None else True
    evidence.process_count_after_cleanup = process_group_process_count(
        process_group_id
    )

    capture_ids: list[str] = []
    saw_record = False
    for line in stdout.splitlines():
        try:
            record = json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        if not isinstance(record, dict):
            continue
        saw_record = True
        capture_id = record.get("capture_id")
        if isinstance(capture_id, str) and capture_id:
            capture_ids.append(capture_id)
        if record.get("activated") is True:
            evidence.activation_state = "active"
        elif evidence.activation_state == "unknown":
            evidence.activation_state = "inactive"
        evidence.restart_state = (
            "required"
            if record.get("restart_required") is True
            else "not_required"
        )

    unique_capture_ids = set(capture_ids)
    if capture_ids:
        evidence.capture_id = capture_ids[0]
        evidence.capture_id_occurrences = len(capture_ids)
    if not timed_out and process.returncode == 0 and len(unique_capture_ids) <= 1:
        evidence.availability_state = "available"
        evidence.availability_detail = None
        if not saw_record:
            evidence.activation_state = "unknown"
            evidence.restart_state = "not_applicable"
    elif evidence.availability_detail == "not observed":
        evidence.availability_state = "failed"
        evidence.availability_detail = "host process failed"

    result = HostResult(
        evidence=evidence,
        stdout=stdout,
        stderr=stderr,
        stdout_log=stdout_log,
        stderr_log=stderr_log,
        process_group_id=process_group_id,
    )
    if len(unique_capture_ids) > 1:
        evidence.availability_state = "failed"
        evidence.availability_detail = "conflicting capture IDs"
        raise HostProcessError(result, "conflicting capture IDs")
    if check and (timed_out or process.returncode != 0):
        raise HostProcessError(result)
    return result


def process_group_process_count(process_group_id: int) -> int:
    """Count live Linux processes in one process group."""

    if process_group_id <= 0:
        return 0
    count = 0
    proc = Path("/proc")
    if not proc.is_dir():
        try:
            os.killpg(process_group_id, 0)
        except ProcessLookupError:
            return 0
        return 1
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text(encoding="utf-8")
            tail = stat[stat.rfind(")") + 2 :].split()
            state = tail[0]
            group = int(tail[2])
        except (FileNotFoundError, PermissionError, ValueError, IndexError):
            continue
        if group == process_group_id and state != "Z":
            count += 1
    return count


def _start_drain(
    stream: object,
    log: object,
    buffer: bytearray | None = None,
) -> threading.Thread:
    def drain() -> None:
        try:
            while True:
                chunk = stream.read(65_536)
                if not chunk:
                    break
                log.write(chunk)
                log.flush()
                if buffer is not None:
                    buffer.extend(chunk)
        finally:
            stream.close()

    thread = threading.Thread(target=drain, daemon=True)
    thread.start()
    return thread


def terminate_process_group(
    process: subprocess.Popen[bytes],
    process_group_id: int,
    grace: float,
    evidence: LifecycleEvidence,
    *,
    kill_timeout: float | None = None,
) -> None:
    bounded_kill_timeout = max(0.01, kill_timeout or max(1.0, grace))
    if os.name == "posix" and process_group_process_count(process_group_id):
        try:
            os.killpg(process_group_id, signal.SIGTERM)
            evidence.term_sent = True
        except ProcessLookupError:
            pass
    elif process.poll() is None:
        process.terminate()
        evidence.term_sent = True
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        process.poll()
        if os.name == "posix":
            _reap_process_group(process_group_id)
        if process.poll() is not None and (
            os.name != "posix" or process_group_process_count(process_group_id) == 0
        ):
            break
        time.sleep(0.005)
    if os.name == "posix" and process_group_process_count(process_group_id):
        try:
            os.killpg(process_group_id, signal.SIGKILL)
            evidence.kill_sent = True
        except ProcessLookupError:
            pass
    elif process.poll() is None:
        process.kill()
        evidence.kill_sent = True
    try:
        process.wait(timeout=bounded_kill_timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
    deadline = time.monotonic() + bounded_kill_timeout
    while time.monotonic() < deadline:
        if os.name != "posix":
            break
        _reap_process_group(process_group_id)
        if process_group_process_count(process_group_id) == 0:
            break
        time.sleep(0.005)


def _reap_process_group(process_group_id: int) -> None:
    while True:
        try:
            child, _ = os.waitpid(-process_group_id, os.WNOHANG)
        except ChildProcessError:
            return
        if child == 0:
            return


_SUBREAPER_ENABLED = False


def enable_process_subreaper() -> None:
    global _SUBREAPER_ENABLED
    if _SUBREAPER_ENABLED or os.name != "posix":
        return
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(36, 1, 0, 0, 0) == 0:
            _SUBREAPER_ENABLED = True
    except (AttributeError, OSError):
        return
