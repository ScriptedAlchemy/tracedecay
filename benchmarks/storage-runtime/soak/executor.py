"""Library-backed executor for frozen, allowlisted storage soak plans."""

from __future__ import annotations

import asyncio
import json
import os
import platform
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import psutil

from process_execution import (
    PROCESS_TREE_SAMPLE_INTERVAL_SECONDS,
    ProcessTreeTracker,
    _posix_group_has_live_members,
    binary_identity,
    current_process_tree_metrics,
    process_tree_capability,
    terminate_tracked_process_tree,
)
from profile_safety import build_child_env, create_child_sandbox
from runner_contract import ConfigError, ExecutionError
from safe_paths import (
    canonical_compact_json,
    fingerprint_tree,
    sha256_bytes,
    sha256_file,
    validate_safe_tree,
)
from soak.schemas import (
    RECEIPT_SCHEMA_ID,
    RESULT_SCHEMA_ID,
    product_adapter_output_valid,
    validate_plan,
    validate_result,
)
from soak.trends import RESOURCE_NAMES

MAX_CAPTURE_BYTES = 1 << 20
MAX_IN_FLIGHT = 64
EXECUTOR_ID = "tracedecay-storage-runtime-soak-executor"
EXECUTOR_VERSION = 1


async def _bounded_read(stream: asyncio.StreamReader | None) -> tuple[bytes, bool]:
    if stream is None:
        return b"", False
    captured = bytearray()
    truncated = False
    while True:
        chunk = await stream.read(64 * 1024)
        if not chunk:
            break
        remaining = MAX_CAPTURE_BYTES - len(captured)
        if remaining > 0:
            captured.extend(chunk[:remaining])
        if len(chunk) > remaining:
            truncated = True
    return bytes(captured), truncated


async def execute_fixed_argv(
    argv: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout_seconds: float,
) -> dict[str, Any]:
    """Execute code-owned argv with asyncio cancellation and psutil cleanup."""
    if not argv or not Path(argv[0]).is_absolute():
        raise ConfigError("allowlisted workload executable must be an absolute path")
    validate_safe_tree(cwd, "soak child cwd")
    started = time.monotonic_ns()
    process = await asyncio.create_subprocess_exec(
        *argv,
        cwd=str(cwd),
        env=env,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        start_new_session=os.name == "posix",
    )
    stdout_task = asyncio.create_task(_bounded_read(process.stdout))
    stderr_task = asyncio.create_task(_bounded_read(process.stderr))
    wait_task = asyncio.create_task(process.wait())
    tracker = ProcessTreeTracker(process.pid)
    timed_out = False
    try:
        deadline = time.monotonic() + timeout_seconds
        while not wait_task.done():
            tracker.sample()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            await asyncio.wait(
                {wait_task},
                timeout=min(PROCESS_TREE_SAMPLE_INTERVAL_SECONDS, remaining),
            )

        if timed_out:
            process_tree = await asyncio.to_thread(
                terminate_tracked_process_tree,
                process.pid,
                tracker=tracker,
                use_process_group=os.name == "posix",
            )
            await asyncio.shield(wait_task)
        else:
            tracker.sample()
            leaked_descendant = any(
                candidate.pid != process.pid for candidate in tracker.live_processes()
            )
            group_leak = (
                os.name == "posix"
                and _posix_group_has_live_members(process.pid)
            )
            if leaked_descendant or group_leak:
                process_tree = await asyncio.to_thread(
                    terminate_tracked_process_tree,
                    process.pid,
                    tracker=tracker,
                    use_process_group=os.name == "posix",
                )
                process_tree["termination"] = "descendant_leak_terminated"
                process_tree["clean"] = "false"
            else:
                process_tree = {
                    **process_tree_capability(),
                    **tracker.metrics(),
                    "termination": "not_required",
                    "clean": "true",
                }
    except BaseException:
        await asyncio.to_thread(
            terminate_tracked_process_tree,
            process.pid,
            tracker=tracker,
            use_process_group=os.name == "posix",
        )
        await asyncio.shield(wait_task)
        await asyncio.shield(asyncio.gather(stdout_task, stderr_task))
        raise
    stdout, stdout_truncated = await stdout_task
    stderr, stderr_truncated = await stderr_task
    return {
        "exit_code": process.returncode,
        "timed_out": timed_out,
        "process_tree_clean": process_tree["clean"] == "true",
        "process_tree": process_tree,
        "wall_ns": time.monotonic_ns() - started,
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
    }


def _product_fts_argv(
    *,
    product_binary: Path,
    evidence_binary: Path,
    fixture: Path,
    sandbox: Path,
    family: str,
    crash_count: int,
    restore_rehearsals: int,
    fixture_sha256: str,
    product_commit_sha: str,
    product_binary_sha256: str,
    evidence_binary_sha256: str,
) -> list[str]:
    del (
        crash_count,
        restore_rehearsals,
        fixture_sha256,
        product_commit_sha,
        product_binary_sha256,
        evidence_binary_sha256,
        evidence_binary,
    )
    if family not in {"graph", "session"}:
        raise ConfigError("product FTS workload family must be graph or session")
    adapter = Path(__file__).resolve().parent.parent / "product_adapter.py"
    return [
        sys.executable,
        str(adapter),
        "fts",
        "--product-binary",
        str(product_binary),
        "--fixture",
        str(fixture),
        "--sandbox",
        str(sandbox),
        "--family",
        family,
    ]


def _product_s11_argv(
    *,
    product_binary: Path,
    evidence_binary: Path,
    fixture: Path,
    sandbox: Path,
    family: str,
    crash_count: int,
    restore_rehearsals: int,
    fixture_sha256: str,
    product_commit_sha: str,
    product_binary_sha256: str,
    evidence_binary_sha256: str,
) -> list[str]:
    del family
    adapter = Path(__file__).resolve().parent.parent / "product_adapter.py"
    return [
        sys.executable,
        str(adapter),
        "s11",
        "--product-binary",
        str(product_binary),
        "--evidence-binary",
        str(evidence_binary),
        "--fixture",
        str(fixture),
        "--sandbox",
        str(sandbox),
        "--fixture-sha256",
        fixture_sha256,
        "--product-commit-sha",
        product_commit_sha,
        "--product-binary-sha256",
        product_binary_sha256,
        "--evidence-binary-sha256",
        evidence_binary_sha256,
        "--crash-count",
        str(crash_count),
        "--restore-rehearsals",
        str(restore_rehearsals),
    ]


WORKLOAD_ALLOWLIST: dict[str, Callable[..., list[str]]] = {
    "storage-runtime-product-fts-v1": _product_fts_argv,
    "storage-runtime-s11-product-gates-v1": _product_s11_argv,
}


def resolve_workload_argv(workload_id: str, **arguments: Any) -> list[str]:
    resolver = WORKLOAD_ALLOWLIST.get(workload_id)
    if resolver is None:
        raise ConfigError(f"workload_id {workload_id!r} is not in the code allowlist")
    return resolver(**arguments)


def _process_metrics(queue_depth: int) -> dict[str, float]:
    process_tree = current_process_tree_metrics()
    values = {
        "queue_depth": float(queue_depth),
        "wal_bytes": 0.0,
        "readers": float(process_tree["peak_thread_count"]),
        "rss_bytes": float(process_tree["peak_rss_bytes"]),
        "fd_count": float(process_tree["peak_fd_count"]),
        "cpu_seconds": float(process_tree["cpu_seconds"]),
        "io_write_bytes": float(process_tree["io_write_bytes"] or 0),
    }
    return {name: values[name] for name in RESOURCE_NAMES}


async def _sample_resources(
    duration_seconds: float,
    interval_seconds: float,
    queue_depth: Callable[[], int],
    stop: asyncio.Event,
) -> list[dict[str, float]]:
    start = time.monotonic()
    samples: list[dict[str, float]] = []
    next_sample = start
    while True:
        now = time.monotonic()
        if now < next_sample:
            await asyncio.sleep(next_sample - now)
            now = time.monotonic()
        elapsed = now - start
        samples.append(
            {"elapsed_seconds": elapsed, **_process_metrics(queue_depth())}
        )
        if elapsed >= duration_seconds and stop.is_set():
            break
        next_sample += interval_seconds
    return samples


async def execute_soak(
    plan: dict,
    *,
    product_binary: Path,
    evidence_binary: Path,
    frozen_binary_identities: dict[str, dict[str, Any]],
    product_commit_sha: str,
    fixture: Path,
    frozen_identity: Path,
    family: str,
    run_root: Path,
    forbidden: list[tuple[str, Path]],
) -> dict:
    """Execute one frozen plan; all process argv comes from the code allowlist."""
    validate_plan(plan)
    if (
        len(product_commit_sha) not in {40, 64}
        or any(character not in "0123456789abcdef" for character in product_commit_sha)
    ):
        raise ExecutionError("frozen product commit identity is invalid")
    plan_sha = plan["plan_sha256"]
    fixture_identity = fingerprint_tree(fixture, "soak fixture")["aggregate_sha256"]
    product_identity = binary_identity(product_binary)
    evidence_identity = binary_identity(evidence_binary)
    for role, actual in (
        ("product_binary", product_identity),
        ("evidence_binary", evidence_identity),
    ):
        expected = frozen_binary_identities.get(role, {})
        if (
            actual.get("sha256") != expected.get("sha256")
            or actual.get("size_bytes") != expected.get("size_bytes")
        ):
            raise ExecutionError(f"{role} changed after frozen identity binding")
    frozen_sha = sha256_file(frozen_identity, "frozen identity")
    implementation_sha = sha256_file(Path(__file__), "soak executor implementation")
    environment = {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "psutil": psutil.__version__,
    }
    environment_sha = sha256_bytes(
        canonical_compact_json(environment).encode("utf-8")
    )
    environment["sha256"] = environment_sha

    child = create_child_sandbox(run_root, "soak executor")
    env = build_child_env(dict(os.environ), {}, [], forbidden, child)
    argv = resolve_workload_argv(
        plan["workload_id"],
        product_binary=product_binary,
        evidence_binary=evidence_binary,
        fixture=fixture,
        sandbox=child["output"],
        family=family,
        crash_count=plan["crash_count"],
        restore_rehearsals=plan["restore_rehearsal_count"],
        fixture_sha256=fixture_identity,
        product_commit_sha=product_commit_sha,
        product_binary_sha256=product_identity["sha256"],
        evidence_binary_sha256=evidence_identity["sha256"],
    )
    outstanding = {"value": 0}
    sampling_complete = asyncio.Event()
    sampler = asyncio.create_task(
        _sample_resources(
            float(plan["duration_seconds"]),
            float(plan["sample_interval_seconds"]),
            lambda: outstanding["value"],
            sampling_complete,
        )
    )
    sustained: list[dict[str, Any]] = []
    logical_evidence: list[dict[str, Any]] = []
    latest_gates: list[dict[str, Any]] = []
    adapter_valid = True
    adapter_successes = 0
    any_timeout = False
    any_failure = False
    for scale in plan["sustained"]:
        counts = {
            "scale": scale["scale"],
            "offered": scale["offered_count"],
            "admitted": 0,
            "completed": 0,
            "failed": 0,
            "shed_runner_in_flight": 0,
            "shed_command_saturation": 0,
            "terminal": 0,
            "latency_origin": "scheduled_issue_time",
        }
        scale_start = time.monotonic()

        async def run_operation() -> None:
            nonlocal adapter_valid, adapter_successes, any_timeout, any_failure
            nonlocal latest_gates, logical_evidence
            outstanding["value"] += 1
            counts["admitted"] += 1
            try:
                result = await execute_fixed_argv(
                    argv,
                    cwd=child["cwd"],
                    env=env,
                    timeout_seconds=float(plan["operation_timeout_seconds"]),
                )
            except OSError:
                counts["terminal"] += 1
                counts["failed"] += 1
                any_failure = True
                return
            finally:
                outstanding["value"] -= 1
            counts["terminal"] += 1
            if result["timed_out"]:
                any_timeout = True
            if (
                result["exit_code"] != 0
                or result["timed_out"]
                or not result["process_tree_clean"]
                or result["stdout_truncated"]
                or result["stderr_truncated"]
            ):
                counts["failed"] += 1
                any_failure = True
                return
            try:
                adapter = json.loads(result["stdout"].decode("utf-8"))
            except (UnicodeError, json.JSONDecodeError):
                adapter_valid = False
                counts["failed"] += 1
                any_failure = True
                return
            if not product_adapter_output_valid(adapter):
                adapter_valid = False
                counts["failed"] += 1
                any_failure = True
                return
            if adapter.get("schema") == "tracedecay-storage-runtime-product-probe-v2":
                if any(
                    gate["status"] == "completed"
                    and (
                        gate["fixture_sha256"] != fixture_identity
                        or gate["product_commit_sha"] != product_commit_sha
                        or gate["product_binary_sha256"] != product_identity["sha256"]
                        or gate["evidence_binary_sha256"] != evidence_identity["sha256"]
                    )
                    for gate in adapter["gates"]
                ):
                    adapter_valid = False
                    counts["failed"] += 1
                    any_failure = True
                    return
            counts["completed"] += 1
            adapter_successes += 1
            if adapter.get("schema") == "tracedecay-storage-runtime-product-probe-v2":
                latest_gates = list(adapter["gates"])
                logical_evidence = [
                    evidence
                    for gate in latest_gates
                    if gate["evidence_status"]["state"] == "evidence"
                    for evidence in gate["logical_evidence"]
                ]

        active: set[asyncio.Task[None]] = set()
        all_tasks: list[asyncio.Task[None]] = []
        for request_id in range(scale["offered_count"]):
            scheduled = scale_start + request_id / scale["rate_per_second"]
            delay = scheduled - time.monotonic()
            if delay > 0:
                await asyncio.sleep(delay)
            active = {task for task in active if not task.done()}
            if len(active) >= MAX_IN_FLIGHT:
                counts["shed_runner_in_flight"] += 1
                counts["terminal"] += 1
                continue
            task = asyncio.create_task(run_operation())
            active.add(task)
            all_tasks.append(task)
        if all_tasks:
            await asyncio.gather(*all_tasks)
        sustained.append(counts)
    sampling_complete.set()
    samples = await sampler
    post_eviction = _process_metrics(0)
    validate_safe_tree(run_root, "soak executor output")
    for role, path, expected in (
        ("product binary", product_binary, product_identity),
        ("evidence binary", evidence_binary, evidence_identity),
    ):
        current = binary_identity(path)
        if (
            current["sha256"] != expected["sha256"]
            or current["size_bytes"] != expected["size_bytes"]
        ):
            raise ExecutionError(f"{role} changed during soak execution")

    trend_limits = {name: 10**18 for name in RESOURCE_NAMES}
    trend_policy = {
        "maximum_slope_per_second": trend_limits,
        "maximum_end_to_baseline_ratio": trend_limits,
        "maximum_post_eviction_ratio": trend_limits,
        "minimum_samples": 2,
        "maximum_samples": 100_000,
        "maximum_cadence_gap_seconds": max(
            2.0 * float(plan["sample_interval_seconds"]), 0.001
        ),
    }
    payload = {
        "schema": RESULT_SCHEMA_ID,
        "plan_identity": {"sha256": plan_sha},
        "workload_identity": {
            "id": plan["workload_id"],
            "implementation_sha256": implementation_sha,
        },
        "commit_identity": {"sha": product_commit_sha},
        "binary_identity": {
            "product_sha256": product_identity["sha256"],
            "evidence_sha256": evidence_identity["sha256"],
        },
        "environment_identity": environment,
        "resource_samples": samples,
        "post_eviction": post_eviction,
        "trend_policy": trend_policy,
    }
    gates_by_id = {
        gate["gate_id"]: gate for gate in latest_gates if isinstance(gate, dict)
    }
    crash_outcome = gates_by_id.get(
        "storage-runtime-crash-recovery-repair-v1", {}
    ).get("outcome", {})
    backup_outcome = gates_by_id.get(
        "storage-runtime-backup-restore-v1", {}
    ).get("outcome", {})
    product_commits = {
        gate.get("product_commit_sha")
        for gate in latest_gates
        if gate.get("status") == "completed"
    }
    receipt = {
        "schema": RECEIPT_SCHEMA_ID,
        "executor_id": EXECUTOR_ID,
        "executor_version": EXECUTOR_VERSION,
        "artifact_schema": RESULT_SCHEMA_ID,
        "status": "timed_out" if any_timeout else "failed" if any_failure else "completed",
        "plan_sha256": plan_sha,
        "workload_id": plan["workload_id"],
        "workload_implementation_sha256": implementation_sha,
        "commit_sha": product_commit_sha,
        "environment_sha256": environment_sha,
        "fixture_sha256": fixture_identity,
        "product_binary_sha256": product_identity["sha256"],
        "evidence_binary_sha256": evidence_identity["sha256"],
        "frozen_identity_sha256": frozen_sha,
        "payload_sha256": sha256_bytes(
            canonical_compact_json(payload).encode("utf-8")
        ),
        "coordinated_omission": False,
        "artifacts_bounded": True,
        "fixture_source": "explicit",
        "fixture_schema": "storage-runtime-fixture-v1",
        "fixture_verified": True,
        "product_adapter_validated": adapter_valid and adapter_successes > 0,
        "logical_evidence": logical_evidence,
        "product_gate_evidence": latest_gates,
        "product_commit_sha": (
            next(iter(product_commits)) if len(product_commits) == 1 else None
        ),
        "sustained": sustained,
        "crash_count_completed": crash_outcome.get("crashes_completed", 0),
        "crash_recovery_count": crash_outcome.get("recoveries_completed", 0),
        "restore_rehearsal_count": backup_outcome.get("backups_completed", 0),
        "restore_verified_count": backup_outcome.get("restores_completed", 0),
    }
    receipt["receipt_sha256"] = sha256_bytes(
        canonical_compact_json(receipt).encode("utf-8")
    )
    result = {**payload, "execution_receipt": receipt}
    validate_result(result)
    return result
