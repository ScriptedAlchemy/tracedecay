#!/usr/bin/env python3
"""Capture graph Criterion and fixture-binary measurements without Cargo.

Criterion is the latency authority. Optional fixture binaries are the authority
for fixture identity, logical writes, and reopen samples; the harness verifies
their retained store bytes directly. Missing fixture binaries remain typed
unavailable measurements instead of synthetic zeroes.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NoReturn

try:
    import fcntl
    import resource
except ModuleNotFoundError:  # Windows does not expose wait4 resource usage.
    fcntl = None  # type: ignore[assignment]
    resource = None  # type: ignore[assignment]

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
if os.fspath(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, os.fspath(REPOSITORY_ROOT))

from benchmarks.runtime.schema import write_jsonl
from benchmarks.runtime.statistics import summarize_distribution


SCHEMA_VERSION = 1
FIXTURE_RECEIPT_ENV = "TRACEDECAY_GRAPH_MEASUREMENT_RECEIPT"
FIXTURE_STORE_ENV = "TRACEDECAY_GRAPH_MEASUREMENT_STORE"
FIXTURE_UNAVAILABLE_DETAIL = (
    "no graph fixture measurement binary was supplied; exact store bytes, "
    "logical write amplification, and reopen time are unavailable"
)
NOFOLLOW_TREE_MEASUREMENT_SUPPORTED = all(
    (
        hasattr(os, "O_DIRECTORY"),
        hasattr(os, "O_NOFOLLOW"),
        os.listdir in os.supports_fd,
        os.open in os.supports_dir_fd,
        os.stat in os.supports_dir_fd,
        os.stat in os.supports_follow_symlinks,
    )
)
EXECUTABLE_SNAPSHOT_SUPPORTED = all(
    (
        hasattr(os, "O_NOFOLLOW"),
        hasattr(os, "memfd_create"),
        hasattr(os, "MFD_ALLOW_SEALING"),
        os.execve in os.supports_fd,
        fcntl is not None,
        all(
            hasattr(fcntl, name)
            for name in (
                "F_ADD_SEALS",
                "F_GET_SEALS",
                "F_SEAL_GROW",
                "F_SEAL_SEAL",
                "F_SEAL_SHRINK",
                "F_SEAL_WRITE",
            )
        ),
    )
)


def _required_capability(module: Any, name: str) -> Any:
    value = getattr(module, name, None)
    if value is None:
        fail("graph measurement execution requires sealed-memory snapshots")
    return value


class GraphMeasurementError(RuntimeError):
    """A malformed input, invalid binary, or failed measurement command."""


def fail(message: str) -> NoReturn:
    raise GraphMeasurementError(message)


@dataclass(frozen=True)
class ExecutableSnapshot:
    source: Path
    file_fd: int
    sha256: str
    identity: tuple[int, ...]


def _sha256_fd(file_fd: int, *, label: Path) -> str:
    digest = hashlib.sha256()
    offset = 0
    try:
        while chunk := os.pread(file_fd, 1024 * 1024, offset):
            digest.update(chunk)
            offset += len(chunk)
    except OSError as exc:
        fail(f"cannot hash measurement file {label}: {exc}")
    return digest.hexdigest()


def _require_executable(value: str | os.PathLike[str]) -> Path:
    path = Path(value).expanduser()
    try:
        is_file = path.is_file()
        is_executable = os.access(path, os.X_OK)
        resolved = path.resolve(strict=True)
    except OSError as exc:
        fail(f"cannot inspect measurement binary {path}: {exc}")
    if not is_file:
        fail(f"measurement binary is not a regular file: {path}")
    if not is_executable:
        fail(f"measurement binary is not executable: {path}")
    return resolved


def _copy_executable_snapshot(source: Path, destination: Path) -> ExecutableSnapshot:
    if not EXECUTABLE_SNAPSHOT_SUPPORTED:
        fail("graph measurement execution requires sealed-memory descriptor snapshots")
    flags = os.O_RDONLY | os.O_NOFOLLOW
    try:
        source_fd = os.open(source, flags)
    except OSError as exc:
        fail(f"cannot open measurement binary {source}: {exc}")
    try:
        before = os.fstat(source_fd)
        if not stat.S_ISREG(before.st_mode):
            fail(f"measurement binary is not a regular file: {source}")
        if before.st_mode & 0o111 == 0:
            fail(f"measurement binary is not executable: {source}")
        try:
            memfd_create = _required_capability(os, "memfd_create")
            allow_sealing = _required_capability(os, "MFD_ALLOW_SEALING")
            snapshot_fd = memfd_create(
                f"tracedecay-{destination.name}",
                allow_sealing,
            )
        except OSError as exc:
            fail(f"cannot create sealed measurement binary {destination.name}: {exc}")
        digest = hashlib.sha256()
        try:
            while chunk := os.read(source_fd, 1024 * 1024):
                digest.update(chunk)
                view = memoryview(chunk)
                while view:
                    written = os.write(snapshot_fd, view)
                    view = view[written:]
            os.fchmod(snapshot_fd, 0o500)
            add_seals = _required_capability(fcntl, "F_ADD_SEALS")
            get_seals = _required_capability(fcntl, "F_GET_SEALS")
            seals = sum(
                _required_capability(fcntl, name)
                for name in (
                    "F_SEAL_GROW",
                    "F_SEAL_SEAL",
                    "F_SEAL_SHRINK",
                    "F_SEAL_WRITE",
                )
            )
            fcntl_function = _required_capability(fcntl, "fcntl")
            fcntl_function(snapshot_fd, add_seals, seals)
            if fcntl_function(snapshot_fd, get_seals) != seals:
                fail(f"measurement binary seals were not retained: {source}")
        except OSError as exc:
            os.close(snapshot_fd)
            fail(f"cannot snapshot measurement binary {source}: {exc}")
        after = os.fstat(source_fd)
    finally:
        os.close(source_fd)
    if _measurement_identity(before) != _measurement_identity(after):
        os.close(snapshot_fd)
        fail(f"measurement binary changed while it was snapshotted: {source}")
    copied = os.fstat(snapshot_fd)
    return ExecutableSnapshot(
        source=source,
        file_fd=snapshot_fd,
        sha256=digest.hexdigest(),
        identity=_measurement_identity(copied),
    )


def _open_current_snapshot(snapshot: ExecutableSnapshot) -> int:
    try:
        file_fd = os.dup(snapshot.file_fd)
    except OSError as exc:
        fail(f"cannot duplicate sealed measurement binary {snapshot.source}: {exc}")
    try:
        before = os.fstat(file_fd)
        if _measurement_identity(before) != snapshot.identity:
            fail(
                "private measurement binary changed before or during execution: "
                f"{snapshot.source}"
            )
        if _sha256_fd(file_fd, label=snapshot.source) != snapshot.sha256:
            fail(f"private measurement binary content changed: {snapshot.source}")
        after = os.fstat(file_fd)
        if _measurement_identity(before) != _measurement_identity(after):
            fail(
                "private measurement binary changed before or during execution: "
                f"{snapshot.source}"
            )
        return file_fd
    except BaseException:
        os.close(file_fd)
        raise


def _finite_positive(value: Any, field: str) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or value <= 0
    ):
        fail(f"{field} must be a finite positive number")
    return float(value)


def _non_negative_integer(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        fail(f"{field} must be a non-negative integer")
    return value


def _digest(value: Any, field: str) -> str:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        fail(f"{field} must be a lowercase SHA-256 digest")
    return value


def _availability(
    value: int | float | None, detail: str | None = None
) -> dict[str, Any]:
    return {
        "available": value is not None,
        "value": value,
        "detail": detail if value is None else None,
    }


def _eligible_distribution(values: Sequence[int | float]) -> dict[str, Any]:
    distribution = summarize_distribution(values)

    def eligible(field: str, minimum_samples: int) -> dict[str, Any]:
        available = len(values) >= minimum_samples
        return {
            "available": available,
            "value": distribution[field] if available else None,
            "minimum_samples": minimum_samples,
        }

    return {
        "sample_count": distribution["sample_count"],
        "min": distribution["min"],
        "p50": eligible("p50", 2),
        "p95": eligible("p95", 40),
        "p99": eligible("p99", 100),
        "max": distribution["max"],
        "mean": distribution["mean"],
        "percentile_method": distribution["percentile_method"],
    }


def summarize_criterion_capture(
    criterion_root: Path,
    *,
    variant: str,
    capture_id: str,
    binary_sha256: str,
    round_index: int,
    abba_position: int,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Read canonical Criterion raw samples and summarize ns per iteration."""

    _digest(binary_sha256, "binary_sha256")
    samples: list[dict[str, Any]] = []
    benchmark_values: dict[str, list[float]] = {}
    sample_paths = sorted(Path(criterion_root).rglob("new/sample.json"))
    if not sample_paths:
        fail(f"Criterion capture produced no new/sample.json files: {criterion_root}")

    for sample_path in sample_paths:
        benchmark_path = sample_path.with_name("benchmark.json")
        try:
            sample_document = json.loads(sample_path.read_text(encoding="utf-8"))
            benchmark_document = json.loads(benchmark_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            fail(f"cannot read Criterion capture {sample_path}: {exc}")
        if not isinstance(sample_document, Mapping) or not isinstance(
            benchmark_document, Mapping
        ):
            fail(f"Criterion capture must contain JSON objects: {sample_path}")
        benchmark_id = benchmark_document.get("full_id")
        if not isinstance(benchmark_id, str) or not benchmark_id:
            fail(f"Criterion benchmark full_id is missing: {benchmark_path}")
        iterations = sample_document.get("iters")
        elapsed = sample_document.get("times")
        if not isinstance(iterations, list) or not isinstance(elapsed, list):
            fail(f"Criterion sample arrays are missing: {sample_path}")
        if len(iterations) != len(elapsed):
            fail(f"Criterion iters and times must have the same length: {sample_path}")
        if not iterations:
            fail(f"Criterion sample arrays must not be empty: {sample_path}")

        benchmark_samples = benchmark_values.setdefault(benchmark_id, [])
        for sample_index, (iteration_count, elapsed_ns) in enumerate(
            zip(iterations, elapsed, strict=True)
        ):
            iteration_value = _finite_positive(
                iteration_count,
                f"{sample_path}.iters[{sample_index}]",
            )
            elapsed_value = _finite_positive(
                elapsed_ns,
                f"{sample_path}.times[{sample_index}]",
            )
            elapsed_per_iteration = elapsed_value / iteration_value
            benchmark_samples.append(elapsed_per_iteration)
            samples.append(
                {
                    "schema_version": SCHEMA_VERSION,
                    "capture_id": capture_id,
                    "variant": variant,
                    "round_index": round_index,
                    "abba_position": abba_position,
                    "binary_sha256": binary_sha256,
                    "benchmark_id": benchmark_id,
                    "sample_index": sample_index,
                    "iterations": iteration_value,
                    "elapsed_ns": elapsed_value,
                    "elapsed_ns_per_iteration": elapsed_per_iteration,
                }
            )

    benchmarks = {
        benchmark_id: {"latency_ns": _eligible_distribution(values)}
        for benchmark_id, values in sorted(benchmark_values.items())
    }
    return samples, benchmarks


def summarize_fixture_receipt(document: Mapping[str, Any]) -> dict[str, Any]:
    """Validate a fixture-binary receipt and preserve measurable fields."""

    if not isinstance(document, Mapping):
        fail("fixture receipt must be a JSON object")
    if document.get("schema_version") != SCHEMA_VERSION:
        fail("fixture receipt schema_version is unsupported")
    fixture = document.get("fixture")
    if not isinstance(fixture, Mapping):
        fail("fixture receipt fixture identity is missing")
    fixture_id = fixture.get("id")
    if not isinstance(fixture_id, str) or not fixture_id:
        fail("fixture receipt fixture.id must be a non-empty string")
    fixture_digest = _digest(fixture.get("sha256"), "fixture.sha256")
    exact_store_bytes = _non_negative_integer(
        document.get("exact_store_bytes"), "exact_store_bytes"
    )
    logical_write_bytes = _non_negative_integer(
        document.get("logical_write_bytes"), "logical_write_bytes"
    )
    process_write_bytes = _non_negative_integer(
        document.get("process_write_bytes"), "process_write_bytes"
    )
    reopen = document.get("reopen_elapsed_ns")
    if not isinstance(reopen, list) or not reopen:
        fail("reopen_elapsed_ns must contain at least one sample")
    reopen_values = [
        _non_negative_integer(value, f"reopen_elapsed_ns[{index}]")
        for index, value in enumerate(reopen)
    ]
    if any(value == 0 for value in reopen_values):
        fail("reopen_elapsed_ns samples must be positive")

    amplification_available = logical_write_bytes > 0
    amplification_detail = (
        None
        if amplification_available
        else "logical_write_bytes is zero; write amplification has no denominator"
    )
    return {
        "fixture": {"id": fixture_id, "sha256": fixture_digest},
        "exact_store_bytes": _availability(exact_store_bytes),
        "write_amplification": {
            "available": amplification_available,
            "logical_write_bytes": logical_write_bytes,
            "process_write_bytes": process_write_bytes,
            "parts_per_million": (
                process_write_bytes * 1_000_000 // logical_write_bytes
                if amplification_available
                else None
            ),
            "detail": amplification_detail,
        },
        "reopen_time_ns": {
            "available": True,
            **_eligible_distribution(reopen_values),
            "samples": reopen_values,
            "detail": None,
        },
    }


def unavailable_fixture_measurements(
    detail: str = FIXTURE_UNAVAILABLE_DETAIL,
) -> dict[str, Any]:
    """Represent unavailable fixture-only fields without a fabricated zero."""

    return {
        "fixture": None,
        "exact_store_bytes": _availability(None, detail),
        "write_amplification": {
            "available": False,
            "logical_write_bytes": None,
            "process_write_bytes": None,
            "parts_per_million": None,
            "detail": detail,
        },
        "reopen_time_ns": {
            "available": False,
            "sample_count": 0,
            "min": None,
            "p50": {"available": False, "value": None, "minimum_samples": 2},
            "p95": {"available": False, "value": None, "minimum_samples": 40},
            "p99": {"available": False, "value": None, "minimum_samples": 100},
            "max": None,
            "mean": None,
            "percentile_method": "nearest_rank",
            "samples": [],
            "detail": detail,
        },
    }


def require_matching_fixture(
    baseline: Mapping[str, Any], candidate: Mapping[str, Any]
) -> dict[str, str]:
    """Require a byte-identical fixture before calling evidence comparable."""

    baseline_fixture = baseline.get("fixture")
    candidate_fixture = candidate.get("fixture")
    if not isinstance(baseline_fixture, Mapping) or not isinstance(
        candidate_fixture, Mapping
    ):
        fail("same-fixture comparison requires both fixture receipts")
    identity = {
        "id": baseline_fixture.get("id"),
        "sha256": baseline_fixture.get("sha256"),
    }
    if identity != {
        "id": candidate_fixture.get("id"),
        "sha256": candidate_fixture.get("sha256"),
    }:
        fail("baseline and candidate do not prove the same fixture identity")
    return {"id": str(identity["id"]), "sha256": str(identity["sha256"])}


def _measurement_identity(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def _inode_identity(metadata: os.stat_result) -> tuple[int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        stat.S_IFMT(metadata.st_mode),
    )


def _require_unchanged_entry(
    before: os.stat_result,
    after: os.stat_result,
    *,
    path: Path,
) -> None:
    if _measurement_identity(before) != _measurement_identity(after):
        fail(f"retained store entry changed during measurement: {path}")


def _retained_directory_bytes(directory_fd: int, *, path: Path) -> int:
    total = 0
    try:
        names = sorted(os.listdir(directory_fd))
    except OSError as exc:
        fail(f"cannot list retained store directory {path}: {exc}")

    for name in names:
        entry_path = path / name
        try:
            before = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        except OSError as exc:
            fail(f"cannot inspect retained store entry {entry_path}: {exc}")

        if stat.S_ISLNK(before.st_mode):
            fail(f"retained store contains a symbolic link: {entry_path}")
        if stat.S_ISDIR(before.st_mode):
            flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
            try:
                child_fd = os.open(name, flags, dir_fd=directory_fd)
            except OSError as exc:
                fail(f"cannot open retained store directory {entry_path}: {exc}")
            try:
                opened = os.fstat(child_fd)
                _require_unchanged_entry(before, opened, path=entry_path)
                total += _retained_directory_bytes(child_fd, path=entry_path)
                _require_unchanged_entry(opened, os.fstat(child_fd), path=entry_path)
            finally:
                os.close(child_fd)
        elif stat.S_ISREG(before.st_mode):
            flags = os.O_RDONLY | os.O_NOFOLLOW
            try:
                file_fd = os.open(name, flags, dir_fd=directory_fd)
            except OSError as exc:
                fail(f"cannot open retained store file {entry_path}: {exc}")
            try:
                opened = os.fstat(file_fd)
                _require_unchanged_entry(before, opened, path=entry_path)
                total += opened.st_size
                _require_unchanged_entry(opened, os.fstat(file_fd), path=entry_path)
            finally:
                os.close(file_fd)
        else:
            fail(f"retained store entry is not a regular file: {entry_path}")

        try:
            after = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        except OSError as exc:
            fail(f"cannot recheck retained store entry {entry_path}: {exc}")
        _require_unchanged_entry(before, after, path=entry_path)
    return total


def _open_directory_path_nofollow(path: Path) -> int:
    if not NOFOLLOW_TREE_MEASUREMENT_SUPPORTED:
        fail("retained store measurement requires no-follow filesystem support")
    try:
        absolute = path.resolve(strict=True)
    except OSError as exc:
        fail(f"cannot resolve retained store directory {path}: {exc}")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    try:
        directory_fd = os.open(absolute.anchor, flags)
    except OSError as exc:
        fail(f"cannot open retained store path anchor {absolute.anchor}: {exc}")
    try:
        for component in absolute.parts[1:]:
            try:
                child_fd = os.open(component, flags, dir_fd=directory_fd)
            except OSError as exc:
                fail(f"cannot open retained store directory {absolute}: {exc}")
            os.close(directory_fd)
            directory_fd = child_fd
    except BaseException:
        os.close(directory_fd)
        raise
    return directory_fd


def _exact_tree_bytes_fd(root_fd: int, *, root: Path) -> int:
    try:
        before = os.fstat(root_fd)
    except OSError as exc:
        fail(f"cannot inspect retained store root {root}: {exc}")
    if not stat.S_ISDIR(before.st_mode):
        fail(f"retained store root is not a directory: {root}")
    total = _retained_directory_bytes(root_fd, path=root)
    try:
        after = os.fstat(root_fd)
    except OSError as exc:
        fail(f"cannot recheck retained store root {root}: {exc}")
    _require_unchanged_entry(before, after, path=root)
    return total


def _exact_tree_bytes(root: Path) -> int:
    root_fd = _open_directory_path_nofollow(root)
    try:
        return _exact_tree_bytes_fd(root_fd, root=root)
    finally:
        os.close(root_fd)


def _maximum_rss_bytes(usage: Any) -> int:
    # Linux reports KiB while Darwin reports bytes.
    if platform.system() == "Darwin":
        return int(usage.ru_maxrss)
    return int(usage.ru_maxrss) * 1024


def _run_process(
    command: Sequence[str],
    *,
    executable: ExecutableSnapshot,
    environment: Mapping[str, str],
    log_path: Path,
    timeout_seconds: int,
) -> dict[str, Any]:
    """Run one owned process and capture its direct wait4 resource receipt."""

    if resource is None or not hasattr(os, "wait4"):
        fail("graph measurement execution requires wait4 resource accounting")
    executable_fd = _open_current_snapshot(executable)
    started_ns = time.monotonic_ns()
    try:
        log = log_path.open("w+b")
    except OSError as exc:
        os.close(executable_fd)
        fail(f"cannot create measurement log {log_path}: {exc}")
    with log:
        launch_status_read, launch_status_write = os.pipe()

        def execute_snapshot() -> None:
            os.close(launch_status_read)
            try:
                os.set_inheritable(launch_status_write, False)
                os.execve(
                    executable_fd,
                    list(command),
                    dict(environment),
                )
            except OSError as exc:
                try:
                    os.write(
                        launch_status_write,
                        (
                            f"cannot execute measurement binary {executable.source}: {exc}"
                        ).encode("utf-8", errors="replace"),
                    )
                finally:
                    os.close(launch_status_write)
                    os._exit(126)

        try:
            process = subprocess.Popen(
                list(command),
                executable=sys.executable,
                cwd=REPOSITORY_ROOT,
                env=dict(environment),
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                pass_fds=(executable_fd, launch_status_write),
                preexec_fn=execute_snapshot,
            )
        except OSError as exc:
            os.close(launch_status_read)
            os.close(launch_status_write)
            fail(f"cannot launch measurement binary {executable.source}: {exc}")
        finally:
            os.close(executable_fd)
        os.close(launch_status_write)
        launch_error = os.read(launch_status_read, 64 * 1024).decode(
            "utf-8",
            errors="replace",
        )
        os.close(launch_status_read)
        if launch_error:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
            fail(launch_error)
        deadline = time.monotonic() + timeout_seconds
        status: int | None = None
        usage: Any | None = None
        try:
            while status is None:
                waited_pid, waited_status, waited_usage = os.wait4(
                    process.pid, os.WNOHANG
                )
                if waited_pid == process.pid:
                    status = waited_status
                    usage = waited_usage
                    break
                if time.monotonic() >= deadline:
                    os.killpg(process.pid, signal.SIGTERM)
                    grace = time.monotonic() + 5
                    while time.monotonic() < grace:
                        waited_pid, waited_status, waited_usage = os.wait4(
                            process.pid, os.WNOHANG
                        )
                        if waited_pid == process.pid:
                            status = waited_status
                            usage = waited_usage
                            break
                        time.sleep(0.05)
                    if status is None:
                        os.killpg(process.pid, signal.SIGKILL)
                        _waited_pid, status, usage = os.wait4(process.pid, 0)
                    process.returncode = os.waitstatus_to_exitcode(status)
                    fail(
                        f"measurement command exceeded {timeout_seconds}s: "
                        + " ".join(command)
                    )
                time.sleep(0.05)
        except BaseException:
            if status is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    _waited_pid, status, usage = os.wait4(process.pid, 0)
                except ChildProcessError:
                    pass
            raise
        process.returncode = os.waitstatus_to_exitcode(status)
        try:
            log.flush()
            os.fsync(log.fileno())
        except OSError as exc:
            fail(f"cannot flush measurement log {log_path}: {exc}")
        log_sha256 = _sha256_fd(log.fileno(), label=log_path)
    if usage is None:
        fail("measurement process exited without a wait4 resource receipt")
    elapsed_ns = time.monotonic_ns() - started_ns
    verification_fd = _open_current_snapshot(executable)
    os.close(verification_fd)
    if process.returncode != 0:
        fail(
            f"measurement command exited {process.returncode}; see {log_path}: "
            + " ".join(command)
        )
    return {
        "elapsed_ns": elapsed_ns,
        "peak_rss_bytes": _maximum_rss_bytes(usage),
        "user_cpu_seconds": usage.ru_utime,
        "system_cpu_seconds": usage.ru_stime,
        "log_sha256": log_sha256,
    }


def _criterion_binary_spec(value: str) -> tuple[str, Path]:
    name, separator, path = value.partition("=")
    if not separator or not name or not path:
        fail("criterion binary must use NAME=/path/to/executable")
    if any(
        character not in "abcdefghijklmnopqrstuvwxyz0123456789-_" for character in name
    ):
        fail(f"criterion binary name is invalid: {name}")
    return name, _require_executable(path)


def _snapshot_named_binaries(
    binaries: Sequence[tuple[str, Path]],
    *,
    root: Path,
    prefix: str,
) -> list[tuple[str, ExecutableSnapshot]]:
    return [
        (
            name,
            _copy_executable_snapshot(source, root / f"{prefix}-{index}-{name}"),
        )
        for index, (name, source) in enumerate(binaries)
    ]


def _fixture_capture(
    binary: ExecutableSnapshot | None,
    *,
    root: Path,
    timeout_seconds: int,
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    if binary is None:
        return unavailable_fixture_measurements(), None
    store_root = root / "store"
    receipt_path = root / "fixture-receipt.json"
    try:
        root.mkdir(parents=True)
    except OSError as exc:
        fail(f"cannot create fixture measurement root {root}: {exc}")
    root_fd = _open_directory_path_nofollow(root)
    root_identity = _inode_identity(os.fstat(root_fd))
    store_fd: int | None = None
    try:
        try:
            os.mkdir("store", dir_fd=root_fd)
            store_fd = os.open(
                "store",
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=root_fd,
            )
            store_identity = _inode_identity(os.fstat(store_fd))
        except OSError as exc:
            fail(f"cannot create retained store {store_root}: {exc}")
        environment = dict(os.environ)
        environment[FIXTURE_STORE_ENV] = os.fspath(store_root)
        environment[FIXTURE_RECEIPT_ENV] = os.fspath(receipt_path)
        process = _run_process(
            (os.fspath(binary.source),),
            executable=binary,
            environment=environment,
            log_path=root / "fixture.log",
            timeout_seconds=timeout_seconds,
        )
        current_root_fd = _open_directory_path_nofollow(root)
        try:
            if _inode_identity(os.fstat(current_root_fd)) != root_identity:
                fail(f"fixture measurement root changed during execution: {root}")
        finally:
            os.close(current_root_fd)
        try:
            named_store = os.stat("store", dir_fd=root_fd, follow_symlinks=False)
        except OSError as exc:
            fail(f"cannot recheck retained store path {store_root}: {exc}")
        if _inode_identity(named_store) != store_identity:
            fail(f"retained store path changed during execution: {store_root}")
        try:
            receipt_fd = os.open(
                receipt_path.name,
                os.O_RDONLY | os.O_NOFOLLOW,
                dir_fd=root_fd,
            )
        except OSError as exc:
            fail(
                f"fixture binary did not write {FIXTURE_RECEIPT_ENV} "
                f"as a regular file: {receipt_path}: {exc}"
            )
        try:
            receipt_before = os.fstat(receipt_fd)
            if not stat.S_ISREG(receipt_before.st_mode):
                fail(
                    f"fixture measurement receipt is not a regular file: {receipt_path}"
                )
            with os.fdopen(receipt_fd, encoding="utf-8") as stream:
                receipt_fd = -1
                document = json.load(stream)
                receipt_after = os.fstat(stream.fileno())
                _require_unchanged_entry(
                    receipt_before,
                    receipt_after,
                    path=receipt_path,
                )
        except (OSError, json.JSONDecodeError) as exc:
            fail(f"cannot read fixture measurement receipt {receipt_path}: {exc}")
        finally:
            if receipt_fd >= 0:
                os.close(receipt_fd)
        retained_store_bytes = _exact_tree_bytes_fd(store_fd, root=store_root)
    finally:
        if store_fd is not None:
            os.close(store_fd)
        os.close(root_fd)
    if not isinstance(document, dict):
        fail("fixture measurement receipt must be a JSON object")
    stated_store_bytes = document.get("exact_store_bytes")
    if stated_store_bytes != retained_store_bytes:
        fail(
            "fixture receipt exact_store_bytes does not match the retained store: "
            f"receipt={stated_store_bytes!r}, retained={retained_store_bytes}"
        )
    return summarize_fixture_receipt(document), process


def _capture_variant(
    *,
    variant: str,
    binaries: Sequence[tuple[str, ExecutableSnapshot]],
    fixture_binary: ExecutableSnapshot | None,
    root: Path,
    capture_id: str,
    round_index: int,
    abba_position: int,
    timeout_seconds: int,
) -> dict[str, Any]:
    criterion_samples: list[dict[str, Any]] = []
    benchmarks: dict[str, Any] = {}
    processes: list[dict[str, Any]] = []
    for name, binary in binaries:
        binary_root = root / "criterion" / name
        criterion_root = binary_root / "output"
        criterion_root.mkdir(parents=True)
        environment = dict(os.environ)
        environment["CRITERION_HOME"] = os.fspath(criterion_root)
        process = _run_process(
            (
                os.fspath(binary.source),
                "--noplot",
                "--save-baseline",
                "capture",
            ),
            executable=binary,
            environment=environment,
            log_path=binary_root / "criterion.log",
            timeout_seconds=timeout_seconds,
        )
        process.update(
            {
                "kind": "criterion",
                "name": name,
                "binary": os.fspath(binary.source),
                "binary_sha256": binary.sha256,
            }
        )
        processes.append(process)
        samples, binary_benchmarks = summarize_criterion_capture(
            criterion_root,
            variant=variant,
            capture_id=capture_id,
            binary_sha256=process["binary_sha256"],
            round_index=round_index,
            abba_position=abba_position,
        )
        duplicate_ids = set(benchmarks) & set(binary_benchmarks)
        if duplicate_ids:
            fail(
                "Criterion benchmark IDs must be unique across binaries: "
                + ", ".join(sorted(duplicate_ids))
            )
        criterion_samples.extend(samples)
        benchmarks.update(binary_benchmarks)

    fixture, fixture_process = _fixture_capture(
        fixture_binary,
        root=root / "fixture",
        timeout_seconds=timeout_seconds,
    )
    if fixture_process is not None and fixture_binary is not None:
        fixture_process.update(
            {
                "kind": "fixture",
                "binary": os.fspath(fixture_binary.source),
                "binary_sha256": fixture_binary.sha256,
            }
        )
        processes.append(fixture_process)
    return {
        "variant": variant,
        "criterion_samples": criterion_samples,
        "benchmarks": benchmarks,
        "process_peak_rss_bytes": _availability(
            max(process["peak_rss_bytes"] for process in processes)
            if processes
            else None,
            "no benchmark or fixture process ran" if not processes else None,
        ),
        "fixture_measurements": fixture,
        "processes": processes,
    }


def _write_json(path: Path, document: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_text(
        json.dumps(document, indent=2, sort_keys=True, allow_nan=False) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def _capture_command(args: argparse.Namespace) -> int:
    if args.timeout_seconds <= 0:
        fail("timeout-seconds must be positive")
    if not isinstance(args.variant, str) or not args.variant:
        fail("variant must be a non-empty string")
    output = Path(args.output)
    samples_path = output.with_suffix(".samples.jsonl")
    for path in (output, samples_path):
        if path.exists():
            fail(f"capture output already exists: {path}")
    binaries = [_criterion_binary_spec(value) for value in args.criterion_binary]
    if not binaries:
        fail("capture requires at least one --criterion-binary")
    fixture_binary = (
        _require_executable(args.fixture_binary) if args.fixture_binary else None
    )
    capture_id = args.capture_id or f"graph-{uuid.uuid4().hex}"
    with tempfile.TemporaryDirectory(
        prefix="tracedecay-graph-measurement-"
    ) as directory:
        temporary_root = Path(directory)
        executable_root = temporary_root / "executables"
        prepared_binaries = _snapshot_named_binaries(
            binaries,
            root=executable_root,
            prefix="criterion",
        )
        prepared_fixture = (
            _copy_executable_snapshot(
                fixture_binary,
                executable_root / "fixture",
            )
            if fixture_binary is not None
            else None
        )
        try:
            result = _capture_variant(
                variant=args.variant,
                binaries=prepared_binaries,
                fixture_binary=prepared_fixture,
                root=temporary_root / "capture",
                capture_id=capture_id,
                round_index=0,
                abba_position=0,
                timeout_seconds=args.timeout_seconds,
            )
        finally:
            for _name, snapshot in prepared_binaries:
                os.close(snapshot.file_fd)
            if prepared_fixture is not None:
                os.close(prepared_fixture.file_fd)
    samples = result.pop("criterion_samples")
    samples_path.parent.mkdir(parents=True, exist_ok=True)
    samples_sha256 = write_jsonl(
        samples_path,
        samples,
        validator=lambda sample: dict(sample),
    )
    _write_json(
        output,
        {
            "schema_version": SCHEMA_VERSION,
            "capture_id": capture_id,
            "machine": {
                "system": platform.system(),
                "machine": platform.machine(),
            },
            "capture": result,
            "raw_samples": {
                "path": samples_path.name,
                "sample_count": len(samples),
                "sha256": samples_sha256,
            },
            "baseline_comparison": {
                "available": False,
                "detail": "no pre-Grafeo baseline binaries were supplied",
            },
        },
    )
    return 0


def _paired_command(args: argparse.Namespace) -> int:
    if args.timeout_seconds <= 0:
        fail("timeout-seconds must be positive")
    if args.rounds <= 0:
        fail("rounds must be positive")
    output = Path(args.output)
    samples_path = output.with_suffix(".samples.jsonl")
    for path in (output, samples_path):
        if path.exists():
            fail(f"paired output already exists: {path}")
    baseline = [_criterion_binary_spec(value) for value in args.baseline_criterion]
    candidate = [_criterion_binary_spec(value) for value in args.candidate_criterion]
    if not baseline or not candidate:
        fail("paired capture requires baseline and candidate Criterion binaries")
    if [name for name, _path in baseline] != [name for name, _path in candidate]:
        fail("paired baseline and candidate Criterion binary names must match in order")
    baseline_fixture = (
        _require_executable(args.baseline_fixture_binary)
        if args.baseline_fixture_binary
        else None
    )
    candidate_fixture = (
        _require_executable(args.candidate_fixture_binary)
        if args.candidate_fixture_binary
        else None
    )
    if (baseline_fixture is None) != (candidate_fixture is None):
        fail("paired fixture measurements require both baseline and candidate binaries")
    capture_id = args.capture_id or f"graph-paired-{uuid.uuid4().hex}"
    captures: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="tracedecay-graph-paired-") as directory:
        root = Path(directory)
        executable_root = root / "executables"
        prepared_baseline = _snapshot_named_binaries(
            baseline,
            root=executable_root,
            prefix="baseline",
        )
        prepared_candidate = _snapshot_named_binaries(
            candidate,
            root=executable_root,
            prefix="candidate",
        )
        for (name, baseline_binary), (_candidate_name, candidate_binary) in zip(
            prepared_baseline,
            prepared_candidate,
            strict=True,
        ):
            if baseline_binary.sha256 == candidate_binary.sha256:
                fail(
                    f"paired baseline and candidate binary content is identical: {name}"
                )
        prepared_baseline_fixture = (
            _copy_executable_snapshot(
                baseline_fixture,
                executable_root / "baseline-fixture",
            )
            if baseline_fixture is not None
            else None
        )
        prepared_candidate_fixture = (
            _copy_executable_snapshot(
                candidate_fixture,
                executable_root / "candidate-fixture",
            )
            if candidate_fixture is not None
            else None
        )
        schedule = (
            ("baseline", prepared_baseline, prepared_baseline_fixture),
            ("candidate", prepared_candidate, prepared_candidate_fixture),
            ("candidate", prepared_candidate, prepared_candidate_fixture),
            ("baseline", prepared_baseline, prepared_baseline_fixture),
        )
        try:
            for round_index in range(args.rounds):
                for position, (variant, binaries, fixture_binary) in enumerate(
                    schedule
                ):
                    captures.append(
                        _capture_variant(
                            variant=variant,
                            binaries=binaries,
                            fixture_binary=fixture_binary,
                            root=root / f"round-{round_index}-position-{position}",
                            capture_id=capture_id,
                            round_index=round_index,
                            abba_position=position,
                            timeout_seconds=args.timeout_seconds,
                        )
                    )
        finally:
            snapshots = [
                snapshot
                for binaries in (prepared_baseline, prepared_candidate)
                for _name, snapshot in binaries
            ]
            for snapshot in (
                prepared_baseline_fixture,
                prepared_candidate_fixture,
            ):
                if snapshot is not None:
                    snapshots.append(snapshot)
            for snapshot in snapshots:
                os.close(snapshot.file_fd)
    benchmark_id_sets = [set(capture["benchmarks"]) for capture in captures]
    if any(ids != benchmark_id_sets[0] for ids in benchmark_id_sets[1:]):
        fail("paired captures did not produce the same Criterion benchmark IDs")

    fixture_comparison: dict[str, Any]
    if baseline_fixture is None:
        fixture_comparison = {
            "available": False,
            "detail": "no same-fixture pre-Grafeo fixture binary was supplied",
        }
    else:
        fixture_measurements = [capture["fixture_measurements"] for capture in captures]
        fixture_identity = fixture_measurements[0]
        for measurement in fixture_measurements[1:]:
            require_matching_fixture(fixture_identity, measurement)
        fixture_comparison = {
            "available": True,
            "fixture": require_matching_fixture(
                fixture_measurements[0], fixture_measurements[1]
            ),
            "detail": None,
        }
    samples = [
        sample for capture in captures for sample in capture.pop("criterion_samples")
    ]
    latency_distributions: dict[str, dict[str, Any]] = {}
    for variant in ("baseline", "candidate"):
        variant_samples = [sample for sample in samples if sample["variant"] == variant]
        latency_distributions[variant] = {
            benchmark_id: _eligible_distribution(
                [
                    sample["elapsed_ns_per_iteration"]
                    for sample in variant_samples
                    if sample["benchmark_id"] == benchmark_id
                ]
            )
            for benchmark_id in sorted(benchmark_id_sets[0])
        }
    samples_path.parent.mkdir(parents=True, exist_ok=True)
    samples_sha256 = write_jsonl(
        samples_path,
        samples,
        validator=lambda sample: dict(sample),
    )
    _write_json(
        output,
        {
            "schema_version": SCHEMA_VERSION,
            "capture_id": capture_id,
            "schedule": "ABBA",
            "rounds": args.rounds,
            "machine": {
                "system": platform.system(),
                "machine": platform.machine(),
            },
            "captures": captures,
            "latency_ns": latency_distributions,
            "raw_samples": {
                "path": samples_path.name,
                "sample_count": len(samples),
                "sha256": samples_sha256,
            },
            "same_fixture_comparison": fixture_comparison,
            "decision": "descriptive_only",
        },
    )
    return 0


def register_subcommands(
    subparsers: Any,
    *,
    capture_name: str = "capture",
    paired_name: str = "paired",
) -> None:
    """Mount graph measurement commands into this or the shared runtime CLI."""

    capture = subparsers.add_parser(capture_name)
    capture.add_argument(
        "--criterion-binary",
        action="append",
        default=[],
        metavar="NAME=PATH",
    )
    capture.add_argument("--fixture-binary")
    capture.add_argument("--variant", default="candidate")
    capture.add_argument("--capture-id")
    capture.add_argument("--timeout-seconds", type=int, default=3_600)
    capture.add_argument("--output", required=True)
    capture.set_defaults(handler=_capture_command)

    paired = subparsers.add_parser(paired_name)
    paired.add_argument(
        "--baseline-criterion",
        action="append",
        default=[],
        metavar="NAME=PATH",
    )
    paired.add_argument(
        "--candidate-criterion",
        action="append",
        default=[],
        metavar="NAME=PATH",
    )
    paired.add_argument("--baseline-fixture-binary")
    paired.add_argument("--candidate-fixture-binary")
    paired.add_argument("--rounds", type=int, default=1)
    paired.add_argument("--capture-id")
    paired.add_argument("--timeout-seconds", type=int, default=3_600)
    paired.add_argument("--output", required=True)
    paired.set_defaults(handler=_paired_command)


def parser() -> argparse.ArgumentParser:
    argument_parser = argparse.ArgumentParser(
        description=(
            "Run explicit prebuilt graph Criterion/fixture binaries and retain "
            "truthful measurement evidence. This command never invokes Cargo."
        )
    )
    subparsers = argument_parser.add_subparsers(dest="command", required=True)
    register_subcommands(subparsers)
    return argument_parser


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.timeout_seconds <= 0:
        print("error: timeout-seconds must be positive", file=sys.stderr)
        return 2
    if getattr(args, "rounds", 1) <= 0:
        print("error: rounds must be positive", file=sys.stderr)
        return 2
    try:
        return int(args.handler(args))
    except GraphMeasurementError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except OSError as exc:
        print(
            f"error: graph measurement filesystem operation failed: {exc}",
            file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
