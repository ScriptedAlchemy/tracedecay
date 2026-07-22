"""Contained argv expansion, process-tree execution, and host identity capture."""

from __future__ import annotations

import hashlib
import os
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import psutil

from runner_contract import ConfigError, ExecutionError, RUNNER_VERSION, RunnerError, SAFE_IDENTIFIER, SafetyError
from safe_paths import _absolute, _path_is_within, assert_safe_path_components, sha256_file, validate_safe_tree
from profile_safety import (
    _env_key, build_child_env, create_child_sandbox, normalized_platform_name,
)

PATH_PLACEHOLDERS = {
    "BINARY",
    "PRODUCT_BINARY",
    "EVIDENCE_BINARY",
    "INPUT",
    "OUTPUT",
    "RUN_DIR",
    "HOME",
    "CONFIG",
    "CACHE",
    "TRACEDECAY_DATA_DIR",
    "TRACEDECAY_GLOBAL_DB",
}
# Exact executable placeholders: templates may not append path segments.
EXECUTABLE_PATH_PLACEHOLDERS = frozenset(
    {"BINARY", "PRODUCT_BINARY", "EVIDENCE_BINARY"}
)
PROCESS_TREE_SAMPLE_INTERVAL_SECONDS = 0.05


def _process_tree_limitation(platform_name: str) -> str:
    if platform_name == "windows":
        return (
            "best-effort psutil recursive snapshots without a Windows Job Object; "
            "descendants that detach between process-tree samples can escape"
        )
    return (
        "best-effort psutil recursive snapshots; descendants that spawn and detach "
        "between process-tree samples can escape"
    )


class ProcessTreeTracker:
    """Track one root and every descendant identity observed through psutil."""

    def __init__(self, root_pid: int):
        self.root_pid = root_pid
        self.samples = 0
        self.observed_pids: set[int] = set()
        self.peak_process_count = 0
        self.peak_rss_bytes = 0
        self.peak_pss_bytes: int | None = None
        self.peak_thread_count = 0
        self.peak_fd_count = 0
        self._identities: dict[tuple[int, float], psutil.Process] = {}
        self._cpu_seconds: dict[tuple[int, float], float] = {}
        self._read_bytes: dict[tuple[int, float], int] = {}
        self._write_bytes: dict[tuple[int, float], int] = {}
        self._errors: set[str] = set()
        self._cleanup_unverifiable = False

    def _record_error(self, error: BaseException) -> None:
        self._errors.add(type(error).__name__)
        if isinstance(error, psutil.AccessDenied):
            self._cleanup_unverifiable = True

    def _remember(self, process: psutil.Process) -> tuple[int, float] | None:
        try:
            identity = (process.pid, process.create_time())
        except (psutil.AccessDenied, psutil.NoSuchProcess, psutil.ZombieProcess) as error:
            self._record_error(error)
            return None
        self._identities[identity] = process
        self.observed_pids.add(process.pid)
        return identity

    def snapshot(self) -> list[psutil.Process]:
        try:
            root = psutil.Process(self.root_pid)
            processes = [root, *root.children(recursive=True)]
        except psutil.NoSuchProcess:
            processes = []
        except (psutil.AccessDenied, psutil.ZombieProcess) as error:
            self._record_error(error)
            processes = []

        unique: dict[int, psutil.Process] = {}
        for process in processes:
            if self._remember(process) is not None:
                unique[process.pid] = process
        return list(unique.values())

    def sample(self) -> None:
        processes = self.snapshot()
        rss_bytes = 0
        pss_bytes = 0
        pss_available = bool(processes)
        thread_count = 0
        fd_count = 0

        for process in processes:
            identity = self._remember(process)
            if identity is None:
                continue
            try:
                with process.oneshot():
                    memory = process.memory_info()
                    cpu = process.cpu_times()
                    rss_bytes += memory.rss
                    thread_count += process.num_threads()
                    self._cpu_seconds[identity] = max(
                        self._cpu_seconds.get(identity, 0.0),
                        float(cpu.user + cpu.system),
                    )
                    try:
                        full_memory = process.memory_full_info()
                        process_pss = getattr(full_memory, "pss", None)
                        if process_pss is None:
                            pss_available = False
                        else:
                            pss_bytes += int(process_pss)
                    except (AttributeError, NotImplementedError, psutil.AccessDenied):
                        pss_available = False
                    try:
                        io = process.io_counters()
                        self._read_bytes[identity] = max(
                            self._read_bytes.get(identity, 0), int(io.read_bytes)
                        )
                        self._write_bytes[identity] = max(
                            self._write_bytes.get(identity, 0), int(io.write_bytes)
                        )
                    except (AttributeError, NotImplementedError, psutil.AccessDenied):
                        pass
                    if hasattr(process, "num_fds"):
                        fd_count += process.num_fds()
                    elif hasattr(process, "num_handles"):
                        fd_count += process.num_handles()
            except (psutil.AccessDenied, psutil.NoSuchProcess, psutil.ZombieProcess) as error:
                self._record_error(error)

        self.samples += 1
        self.peak_process_count = max(self.peak_process_count, len(processes))
        self.peak_rss_bytes = max(self.peak_rss_bytes, rss_bytes)
        self.peak_thread_count = max(self.peak_thread_count, thread_count)
        self.peak_fd_count = max(self.peak_fd_count, fd_count)
        if pss_available:
            self.peak_pss_bytes = max(self.peak_pss_bytes or 0, pss_bytes)

    def live_processes(self) -> list[psutil.Process]:
        candidates = {process.pid: process for process in self.snapshot()}
        for identity, process in self._identities.items():
            try:
                if (
                    process.create_time() == identity[1]
                    and process.is_running()
                    and process.status() != psutil.STATUS_ZOMBIE
                ):
                    candidates[process.pid] = process
            except (psutil.AccessDenied, psutil.NoSuchProcess, psutil.ZombieProcess) as error:
                if isinstance(error, psutil.AccessDenied):
                    self._record_error(error)
                continue
        return list(candidates.values())

    def descendants_first(self, processes: list[psutil.Process]) -> list[psutil.Process]:
        by_pid = {process.pid: process for process in processes}

        def depth(process: psutil.Process) -> int:
            current = process
            seen: set[int] = set()
            value = 0
            while current.pid != self.root_pid and current.pid not in seen:
                seen.add(current.pid)
                try:
                    parent = by_pid.get(current.ppid())
                except (psutil.AccessDenied, psutil.NoSuchProcess, psutil.ZombieProcess):
                    break
                if parent is None:
                    break
                value += 1
                current = parent
            return value

        return sorted(
            processes,
            key=lambda process: (process.pid == self.root_pid, -depth(process)),
        )

    def metrics(self) -> dict[str, Any]:
        return {
            "samples": self.samples,
            "observed_pids": sorted(self.observed_pids),
            "peak_process_count": self.peak_process_count,
            "peak_rss_bytes": self.peak_rss_bytes,
            "peak_pss_bytes": self.peak_pss_bytes,
            "peak_thread_count": self.peak_thread_count,
            "peak_fd_count": self.peak_fd_count,
            "cpu_seconds": sum(self._cpu_seconds.values()),
            "io_read_bytes": sum(self._read_bytes.values()) if self._read_bytes else None,
            "io_write_bytes": (
                sum(self._write_bytes.values()) if self._write_bytes else None
            ),
            "observation_errors": sorted(self._errors),
            "child_process_coverage_complete": False,
        }


def current_process_tree_metrics(root_pid: int | None = None) -> dict[str, Any]:
    """Return one psutil snapshot using the same accounting as child runners."""
    tracker = ProcessTreeTracker(root_pid or os.getpid())
    tracker.sample()
    return tracker.metrics()


def require_safe_identifier(value: object, role: str) -> str:
    if not isinstance(value, str):
        raise ConfigError(f"{role} must be a string safe identifier")
    text = value
    stem = text.split(".", 1)[0].casefold()
    windows_reserved = {"con", "prn", "aux", "nul", "clock$"} | {
        f"{prefix}{number}" for prefix in ("com", "lpt") for number in range(1, 10)
    }
    if (
        text in {".", ".."}
        or text.endswith(".")
        or stem in windows_reserved
        or not SAFE_IDENTIFIER.fullmatch(text)
    ):
        raise ConfigError(
            f"{role} must be a safe identifier ([A-Za-z0-9][A-Za-z0-9_.-]{{0,63}})"
        )
    return text


def substitute(arg: str, mapping: dict[str, str]) -> str:
    for token, value in mapping.items():
        arg = arg.replace(f"__{token}__", value)
    if re.search(r"__[A-Z0-9_]+__", arg):
        raise ConfigError(f"unresolved command placeholder in argument")
    return arg


def safe_expanded_path(
    value: str,
    root: Path,
    role: str,
    *,
    allow_missing: bool = True,
    require_directory: bool | None = None,
) -> Path:
    """Require an expanded path to remain inside its runner-owned root."""
    candidate = _absolute(value)
    if not _path_is_within(candidate, root):
        raise SafetyError(f"{role} escapes its allowed runner-owned root")
    assert_safe_path_components(root, role, require_directory=True)
    return assert_safe_path_components(
        candidate, role, allow_missing=allow_missing, require_directory=require_directory
    )


def _path_portion(argument: str) -> str:
    """Extract an option's value for --flag=/path style argv entries."""
    if argument.startswith("-") and "=" in argument:
        return argument.split("=", 1)[1]
    return argument


def substitute_argv(
    argv: list[str], mapping: dict[str, str], path_roots: dict[str, Path] | None = None
) -> list[str]:
    expanded: list[str] = []
    for raw_arg in argv:
        template = str(raw_arg)
        value = substitute(template, mapping)
        if "__PYTHON__" in template and _path_portion(value) != mapping.get("PYTHON"):
            raise SafetyError("expanded Python executable path may not be modified by a template")
        if path_roots is not None:
            for token in PATH_PLACEHOLDERS:
                marker = f"__{token}__"
                if marker in template:
                    root = path_roots.get(token)
                    if root is None:
                        raise ConfigError(f"no containment root declared for {marker}")
                    if token in EXECUTABLE_PATH_PLACEHOLDERS:
                        expected = mapping.get(token)
                        if expected is None:
                            raise ConfigError(f"no executable mapping declared for {marker}")
                        if _path_portion(value) != expected:
                            raise SafetyError(
                                f"expanded {token.lower().replace('_', ' ')} path "
                                "may not be modified by a template"
                            )
                        assert_safe_path_components(
                            Path(expected),
                            f"expanded {token.lower().replace('_', ' ')}",
                            require_directory=False,
                        )
                        continue
                    safe_expanded_path(
                        _path_portion(value), root, f"expanded command argument {token}"
                    )
        expanded.append(value)
    if not expanded:
        raise ConfigError("command argv must not be empty")
    return expanded


def process_tree_capability(platform_name: str | None = None) -> dict[str, str]:
    """Describe the cross-platform best-effort psutil containment boundary."""
    target = (
        normalized_platform_name(platform_name)
        if platform_name
        else normalized_platform_name()
    )
    if target == "windows":
        return {
            "state": "supported_best_effort",
            "mechanism": "psutil_recursive_no_job_object",
            "descendant_verification": "observed_process_identity_liveness",
            "child_process_coverage_complete": "false",
            "limitation": _process_tree_limitation(target),
        }
    if target in {"linux", "macos"}:
        return {
            "state": "supported_best_effort",
            "mechanism": "psutil_recursive_with_process_group",
            "descendant_verification": "observed_process_identity_and_process_group_liveness",
            "child_process_coverage_complete": "false",
            "limitation": _process_tree_limitation(target),
        }
    return {
        "state": "unsupported_platform",
        "mechanism": "none",
        "descendant_verification": "unsupported",
        "child_process_coverage_complete": "false",
        "limitation": "platform is outside the supported Windows/Linux/macOS soak matrix",
    }


def _popen_group_kwargs() -> dict[str, Any]:
    if os.name == "posix":
        return {"start_new_session": True}
    if os.name == "nt":
        return {"creationflags": getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0x00000200)}
    return {}


def _posix_group_alive(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Existence without permission cannot be safely verified as gone.
        return True
    return True


def _posix_group_has_live_members(process_group: int) -> bool:
    """Best-effort distinguish a live descendant from an unreapable zombie.

    A killed grandchild can briefly (or on a broken PID 1, persistently) remain
    as a zombie after its parent is killed.  It cannot execute or mutate a
    store, so Linux /proc lets us verify that no *live* descendant remains.
    Other POSIX hosts retain the conservative process-group liveness fallback.
    """
    proc_root = Path("/proc")
    if not proc_root.is_dir():
        return _posix_group_alive(process_group)
    seen = False
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return _posix_group_alive(process_group)
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            text = (entry / "stat").read_text(encoding="utf-8")
            tail = text[text.rfind(")") + 1 :].split()
            # tail = state, ppid, pgrp, ...
            if len(tail) < 3 or int(tail[2]) != process_group:
                continue
            seen = True
            if tail[0] != "Z":
                return True
        except (OSError, ValueError):
            continue
    return False if seen else _posix_group_alive(process_group)


def _wait_for_no_live_posix_group(process_group: int, timeout_seconds: float) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while _posix_group_has_live_members(process_group):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.01)
    return True


def terminate_tracked_process_tree(
    root_pid: int,
    *,
    tracker: ProcessTreeTracker | None = None,
    grace_seconds: float = 0.5,
    force: bool = False,
    use_process_group: bool = False,
) -> dict[str, Any]:
    """Stop every observed descendant before the root, escalating to kill."""
    active_tracker = tracker or ProcessTreeTracker(root_pid)
    active_tracker.sample()
    processes = active_tracker.descendants_first(active_tracker.live_processes())
    if not force:
        for process in processes:
            try:
                process.terminate()
            except (psutil.AccessDenied, psutil.NoSuchProcess, psutil.ZombieProcess) as error:
                active_tracker._record_error(error)
        _, alive = psutil.wait_procs(processes, timeout=grace_seconds)
    else:
        alive = processes

    # Re-snapshot after the grace period so children created during termination
    # are included before the root is force-killed.
    survivors = {process.pid: process for process in alive}
    for process in active_tracker.live_processes():
        survivors[process.pid] = process
    for process in active_tracker.descendants_first(list(survivors.values())):
        try:
            process.kill()
        except (psutil.AccessDenied, psutil.NoSuchProcess, psutil.ZombieProcess) as error:
            active_tracker._record_error(error)
    psutil.wait_procs(list(survivors.values()), timeout=max(grace_seconds, 0.1))
    group_clean = True
    if use_process_group and os.name == "posix":
        if _posix_group_alive(root_pid):
            try:
                os.killpg(root_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        group_clean = _wait_for_no_live_posix_group(root_pid, grace_seconds)
    remaining = active_tracker.live_processes()
    clean = (
        not remaining
        and group_clean
        and not active_tracker._cleanup_unverifiable
    )
    return {
        **process_tree_capability(),
        **active_tracker.metrics(),
        "termination": (
            "tree_killed" if force and clean
            else "tree_terminated" if clean
            else "tree_termination_unverified"
        ),
        "clean": "true" if clean else "false",
    }


def terminate_process_tree(
    proc: subprocess.Popen,
    *,
    grace_seconds: float = 0.5,
    tracker: ProcessTreeTracker | None = None,
) -> dict[str, Any]:
    """Terminate a tracked tree, retaining the POSIX process group as backup."""
    result = terminate_tracked_process_tree(
        proc.pid,
        tracker=tracker,
        grace_seconds=grace_seconds,
        use_process_group=os.name == "posix",
    )
    try:
        proc.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        result["clean"] = "false"
        result["termination"] = "tree_termination_unverified"
    return result


def kill_process_tree(
    proc: subprocess.Popen,
    *,
    grace_seconds: float = 0.5,
    tracker: ProcessTreeTracker | None = None,
) -> dict[str, Any]:
    """Abruptly kill a tracked tree for crash-recovery workloads."""
    result = terminate_tracked_process_tree(
        proc.pid,
        tracker=tracker,
        grace_seconds=grace_seconds,
        force=True,
        use_process_group=os.name == "posix",
    )
    try:
        proc.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        result["clean"] = "false"
        result["termination"] = "tree_kill_unverified"
    return result


def _redacted_stream_summary(stream) -> dict[str, Any]:
    stream.flush()
    stream.seek(0)
    digest = hashlib.sha256()
    byte_count = 0
    newline_count = 0
    for chunk in iter(lambda: stream.read(1 << 20), b""):
        digest.update(chunk)
        byte_count += len(chunk)
        newline_count += chunk.count(b"\n")
    return {
        "redacted": True,
        "sha256": digest.hexdigest(),
        "byte_count": byte_count,
        "line_count": newline_count + (1 if byte_count else 0),
    }


def command_failure_detail(result: dict) -> str:
    """Diagnostics with hashes only: never echo command stdout/stderr evidence."""
    return (
        f"exit={result.get('exit_code')} timed_out={result.get('timed_out')} "
        f"stdout={result.get('stdout')} stderr={result.get('stderr')}"
    )


def preferred_output_summary(result: dict) -> dict[str, Any]:
    return result["stdout"] if result["stdout"]["byte_count"] else result["stderr"]


def run_command(
    argv: list[str],
    env: dict,
    timeout_seconds: float,
    cwd: Path | None = None,
    cancel_event=None,
) -> dict:
    if isinstance(argv, (str, bytes)) or not argv:
        raise ConfigError("command argv must be a non-empty argument vector")
    if not all(isinstance(argument, str) for argument in argv):
        raise ConfigError("command argument-vector entries must be strings")
    if cwd is not None:
        assert_safe_path_components(cwd, "child cwd", require_directory=True)
    started = time.monotonic_ns()
    temp_dir = str(cwd) if cwd else None
    with tempfile.TemporaryFile(dir=temp_dir) as stdout_file, tempfile.TemporaryFile(
        dir=temp_dir
    ) as stderr_file:
        try:
            proc = subprocess.Popen(
                argv,
                env=env,
                cwd=str(cwd) if cwd else None,
                stdout=stdout_file,
                stderr=stderr_file,
                **_popen_group_kwargs(),
            )
        except OSError as exc:
            raise ExecutionError(
                f"failed to execute command {Path(argv[0]).name!r}: {type(exc).__name__}"
            ) from exc

        tracker = ProcessTreeTracker(proc.pid)
        timed_out = False
        cancelled = False
        deadline = time.monotonic() + timeout_seconds
        while proc.poll() is None:
            tracker.sample()
            if cancel_event is not None and cancel_event.is_set():
                cancelled = True
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            time.sleep(min(PROCESS_TREE_SAMPLE_INTERVAL_SECONDS, remaining))

        if timed_out or cancelled:
            process_tree = terminate_process_tree(proc, tracker=tracker)
        else:
            tracker.sample()
            leaked_descendant = any(
                process.pid != proc.pid for process in tracker.live_processes()
            )
            group_leak = os.name == "posix" and _posix_group_has_live_members(proc.pid)
            if leaked_descendant or group_leak:
                process_tree = terminate_process_tree(proc, tracker=tracker)
                process_tree["termination"] = "descendant_leak_terminated"
                process_tree["clean"] = "false"
            else:
                process_tree = {
                    **process_tree_capability(),
                    **tracker.metrics(),
                    "termination": "not_required",
                    "clean": "true",
                }

        try:
            proc.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            process_tree["clean"] = "false"
            process_tree["termination"] = "tree_termination_unverified"
        finished = time.monotonic_ns()
        return {
            "exit_code": proc.returncode,
            "wall_ns": finished - started,
            "stdout": _redacted_stream_summary(stdout_file),
            "stderr": _redacted_stream_summary(stderr_file),
            "timed_out": timed_out,
            "cancelled": cancelled,
            "process_tree": process_tree,
        }


def map_outcome(exit_code, timed_out: bool, outcome_map: dict) -> str:
    if timed_out:
        return "failed"
    mapped = outcome_map.get(str(exit_code))
    if mapped is not None:
        return mapped
    return "failed"


def command_succeeded(result: dict, expected_exit_code: int = 0) -> bool:
    """A leaked POSIX process group invalidates an otherwise-zero command."""
    return (
        not result["timed_out"]
        and not result.get("cancelled", False)
        and result["exit_code"] == expected_exit_code
        and result.get("process_tree", {}).get("clean") != "false"
    )


# ---------------------------------------------------------------------------
# Environment / identity capture (no machine-specific absolute paths)
# ---------------------------------------------------------------------------


def safe_probe_base_env(base_env: dict) -> dict[str, str]:
    """Minimal inherited environment for version probes, not the operator profile."""
    allowed = {
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "COMSPEC",
        "LANG",
        "LC_ALL",
        "TZ",
        "TMP",
        "TEMP",
    }
    normalized_allowed = {_env_key(item) for item in allowed}
    return {
        key: str(value)
        for key, value in base_env.items()
        if _env_key(key) in normalized_allowed
    }


def capture_environment(
    workload: dict,
    host_label: str | None,
    record_hostname: bool,
    probe_root: Path,
    forbidden: list[tuple[str, Path]],
) -> dict:
    probe_sandbox = create_child_sandbox(probe_root, "version probe")
    probe_env = build_child_env(
        safe_probe_base_env(dict(os.environ)), {}, [], forbidden, probe_sandbox
    )
    path_roots = {
        "HOME": probe_sandbox["home"],
        "CONFIG": probe_sandbox["config"],
        "CACHE": probe_sandbox["cache"],
        "TRACEDECAY_DATA_DIR": probe_sandbox["data"],
        "TRACEDECAY_GLOBAL_DB": probe_sandbox["data"],
    }
    env_block = {
        "os": normalized_platform_name(),
        "platform_id": normalized_platform_name(),
        "os_release": platform.release(),
        "os_version": platform.version(),
        "machine": platform.machine(),
        "python_version": platform.python_version(),
        "cpu_count_logical": os.cpu_count(),
        "hostname": platform.node() if record_hostname else "redacted",
        "host_label": host_label or "unspecified",
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "runner_version": RUNNER_VERSION,
        "tool_versions": {},
    }
    for name, argv in (workload.get("environment", {}).get("version_commands") or {}).items():
        require_safe_identifier(name, "version command name")
        try:
            command = substitute_argv(
                [str(a) for a in argv], {"PYTHON": sys.executable}, path_roots
            )
            result = run_command(command, probe_env, 15.0, cwd=probe_sandbox["cwd"])
            env_block["tool_versions"][name] = {
                "status": "available" if command_succeeded(result) else "unavailable",
                "exit_code": result["exit_code"],
                "output": preferred_output_summary(result),
                "process_tree": result["process_tree"],
            }
        except RunnerError:
            env_block["tool_versions"][name] = {"status": "unavailable"}
    validate_safe_tree(probe_root, "version probe output")
    return env_block


def binary_identity(binary_path) -> dict:
    path = assert_safe_path_components(binary_path, "binary", require_directory=False)
    info = os.lstat(path)
    return {
        "basename": path.name,
        "sha256": sha256_file(path, "binary"),
        "size_bytes": info.st_size,
    }
