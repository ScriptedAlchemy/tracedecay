"""Closed/open-loop, crash, recovery, backup, and A/A phase execution."""

from __future__ import annotations

import os
import subprocess
import threading
import time
from pathlib import Path
from typing import Any

from runner_contract import ConfigError, DEFAULT_OUTCOME_MAP, ExecutionError, RunnerError
from safe_paths import assert_safe_path_components, create_fresh_directory
from process_execution import (
    _popen_group_kwargs, _posix_group_has_live_members, command_failure_detail,
    command_succeeded, kill_process_tree, map_outcome, process_tree_capability,
    require_safe_identifier, substitute_argv, terminate_process_tree,
)
from workload_model import (
    _config_int, _config_number, effective_phase_pending_reason, new_counts, summarize_latency,
)
from run_context import RunContext
from evidence_validation import evaluate_compares, record_evidence, relative_to_output

def fresh_run_dir(ctx: RunContext, phase: dict, family: str, label: str | None) -> Path:
    phase_name = require_safe_identifier(phase["name"], "phase name")
    family = require_safe_identifier(family, "store family")
    parts = [phase_name, family]
    if label:
        parts.append(require_safe_identifier(label, "run label"))
    else:
        parts.append("run")
    phase_dir = ctx._owned_directory(ctx.work_root / parts[0], "phase work directory")
    family_dir = ctx._owned_directory(phase_dir / parts[1], "family work directory")
    run_dir = create_fresh_directory(family_dir / parts[2], "run directory")
    ctx.prepare_run(run_dir, phase, family)
    return run_dir


def execute_setup(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> None:
    setup = phase.get("setup")
    if not isinstance(setup, dict):
        return
    result = ctx.command(setup, family, run_dir)
    if not command_succeeded(result, setup.get("expect_exit_code", 0)):
        raise ExecutionError(
            f"setup failed for phase {phase['name']!r} family {family!r}: "
            f"{command_failure_detail(result)}"
        )


def execute_closed_loop(
    ctx: RunContext, phase: dict, family: str, run_dir: Path
) -> dict:
    work = phase["work"]
    defaults = ctx.workload.get("defaults", {})
    warmup = _config_int(phase.get("warmup", defaults.get("warmup", 0)), "warmup", 0)
    repetitions = _config_int(
        phase.get("repetitions", defaults.get("repetitions", 1)), "repetitions", 1
    )
    outcome_map = {**DEFAULT_OUTCOME_MAP, **(phase.get("outcome_map") or {})}

    counts = new_counts()
    samples: list[dict] = []
    latencies: list[int] = []

    for index in range(warmup + repetitions):
        measured = index >= warmup
        issue = time.monotonic_ns()
        result = ctx.command(work, family, run_dir, index)
        finished = time.monotonic_ns()
        if not measured:
            continue
        outcome = map_outcome(result["exit_code"], result["timed_out"], outcome_map)
        if result["process_tree"].get("clean") == "false":
            outcome = "failed"
        counts["offered"] += 1
        counts["admitted"] += 1
        if outcome == "completed":
            counts["completed"] += 1
        else:
            counts["failed"] += 1
        latency = finished - issue
        latencies.append(latency)
        samples.append(
            {
                "operation": index - warmup,
                "latency_ns": latency,
                "exit_code": result["exit_code"],
                "timed_out": result["timed_out"],
                "outcome": outcome,
                "process_tree": result["process_tree"],
            }
        )

    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    wall_ns = sum(latencies)
    return {
        "counts": counts,
        "latency": {
            # Closed loop: each operation is issued only after the previous
            # one completed, so issue time equals scheduled time and the
            # distribution carries no coordinated omission.
            "response_ns": summarize_latency(latencies),
        },
        "throughput_ops_per_second": (
            counts["completed"] / (wall_ns / 1e9) if wall_ns > 0 else None
        ),
        "samples": samples,
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_open_loop(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> dict:
    work = phase["work"]
    rate = _config_number(
        phase["offered_rate_per_second"], "open_loop offered_rate_per_second", 0, strict=True
    )
    operation_count = _config_int(phase["operation_count"], "open_loop operation_count", 1)
    max_in_flight = _config_int(phase["max_in_flight"], "open_loop max_in_flight", 1)
    outcome_map = {**DEFAULT_OUTCOME_MAP, **(phase.get("outcome_map") or {})}
    retryable = set(phase.get("retryable_outcomes") or ["shed"])
    max_retries = _config_int(phase.get("max_retries", 0), "open_loop max_retries", 0)

    counts = new_counts()
    counts_lock = threading.Lock()
    in_flight = {"value": 0}
    requests: list[dict[str, Any] | None] = [None] * operation_count
    requests_lock = threading.Lock()
    latencies: list[int] = []
    schedule_lags: list[int] = []
    start_ns = time.monotonic_ns()

    def offset(timestamp_ns: int) -> int:
        return timestamp_ns - start_ns

    def worker(op_index: int, scheduled_ns: int, request: dict[str, Any]) -> None:
        attempts = 0
        final_outcome = "failed"
        final_exit = None
        final_timed_out = False
        finished_ns = scheduled_ns
        request["started_at_ns"] = offset(time.monotonic_ns())
        try:
            while True:
                result = ctx.command(work, family, run_dir, op_index)
                finished_ns = time.monotonic_ns()
                outcome = map_outcome(result["exit_code"], result["timed_out"], outcome_map)
                if result["process_tree"].get("clean") == "false":
                    outcome = "failed"
                final_outcome = outcome
                final_exit = result["exit_code"]
                final_timed_out = result["timed_out"]
                if outcome in retryable and attempts < max_retries:
                    attempts += 1
                    continue
                break
        except RunnerError as exc:
            finished_ns = time.monotonic_ns()
            final_outcome = "failed"
            request["error_class"] = type(exc).__name__
        with counts_lock:
            counts["retried"] += attempts
            if final_outcome == "completed":
                counts["completed"] += 1
            elif final_outcome == "shed":
                counts["shed"]["command_saturation"] += 1
            else:
                counts["failed"] += 1
            in_flight["value"] -= 1
        latency = finished_ns - scheduled_ns
        with requests_lock:
            latencies.append(latency)
            request.update(
                {
                    # Latency is measured from the scheduled issue time, not
                    # service start, so queueing delay is retained.
                    "terminal_at_ns": offset(finished_ns),
                    "latency_ns": latency,
                    "attempts": attempts + 1,
                    "exit_code": final_exit,
                    "timed_out": final_timed_out,
                    "outcome": (
                        "shed_command_saturation"
                        if final_outcome == "shed"
                        else final_outcome
                    ),
                    "terminal": True,
                }
            )

    threads: list[threading.Thread] = []
    for op_index in range(operation_count):
        scheduled_ns = start_ns + int(op_index * 1e9 / rate)
        now = time.monotonic_ns()
        if scheduled_ns > now:
            time.sleep((scheduled_ns - now) / 1e9)
        issue_ns = time.monotonic_ns()
        request: dict[str, Any] = {
            "request_id": op_index,
            "scheduled_at_ns": offset(scheduled_ns),
            "admitted_at_ns": None,
            "started_at_ns": None,
            "terminal_at_ns": None,
            "terminal": False,
        }
        with requests_lock:
            requests[op_index] = request
            schedule_lags.append(issue_ns - scheduled_ns)
        with counts_lock:
            counts["offered"] += 1
            if in_flight["value"] >= max_in_flight:
                counts["shed"]["runner_in_flight_cap"] += 1
                request.update(
                    {
                        "terminal_at_ns": offset(issue_ns),
                        "latency_ns": issue_ns - scheduled_ns,
                        "attempts": 0,
                        "exit_code": None,
                        "timed_out": False,
                        "outcome": "shed_runner_in_flight_cap",
                        "terminal": True,
                    }
                )
                with requests_lock:
                    latencies.append(issue_ns - scheduled_ns)
                continue
            in_flight["value"] += 1
            counts["admitted"] += 1
            request["admitted_at_ns"] = offset(issue_ns)
        thread = threading.Thread(
            target=worker, args=(op_index, scheduled_ns, request), daemon=True
        )
        thread.start()
        threads.append(thread)
    for thread in threads:
        thread.join()
    workload_finished_ns = time.monotonic_ns()

    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    terminal_requests = [request for request in requests if request is not None]
    if len(terminal_requests) != operation_count or any(
        not request.get("terminal") or request.get("terminal_at_ns") is None
        for request in terminal_requests
    ):
        raise ExecutionError("open-loop request ledger is missing a terminal record")
    return {
        "counts": counts,
        "latency": {
            "response_ns": summarize_latency(latencies),
            "schedule_lag_ns": summarize_latency(schedule_lags),
        },
        "throughput_ops_per_second": (
            counts["completed"]
            / ((workload_finished_ns - start_ns) / 1e9)
            if operation_count
            else None
        ),
        "requests": terminal_requests,
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_crash(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> dict:
    capability = process_tree_capability()
    if capability["state"] != "supported_best_effort":
        raise ExecutionError(
            "crash phase is unsupported without safe stdlib process-tree control "
            f"({capability['state']})"
        )
    work = phase["work"]
    wait_for_file = phase.get("wait_for_file")
    wait_timeout = _config_number(
        phase.get("wait_timeout_seconds", 30.0), "crash wait_timeout_seconds", 0, strict=True
    )
    after_seconds = _config_number(
        phase.get("after_seconds", 1.0), "crash after_seconds", 0, strict=True
    )

    started = time.monotonic_ns()
    argv = substitute_argv(
        work["argv"], ctx.mapping(family, run_dir), ctx.path_roots(run_dir)
    )
    try:
        proc = subprocess.Popen(
            argv,
            env=ctx.child_env(run_dir),
            cwd=str(ctx.state(run_dir)["cwd"]),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **_popen_group_kwargs(),
        )
    except OSError as exc:
        raise ExecutionError(
            f"failed to execute crash command {Path(argv[0]).name!r}: {type(exc).__name__}"
        ) from exc
    tree_result: dict[str, str] | None = None
    try:
        if wait_for_file:
            target = ctx.expand_path(wait_for_file, family, run_dir, "crash wait_for_file")
            deadline = started + int(wait_timeout * 1e9)
            while True:
                assert_safe_path_components(target, "crash wait_for_file", allow_missing=True)
                if target.exists():
                    break
                if time.monotonic_ns() > deadline:
                    tree_result = terminate_process_tree(proc)
                    raise ExecutionError(
                        f"crash phase {phase['name']!r} family {family!r}: "
                        f"wait trigger did not appear within {wait_timeout}s"
                    )
                if proc.poll() is not None:
                    raise ExecutionError(
                        f"crash phase {phase['name']!r} family {family!r}: work "
                        f"process exited {proc.returncode} before the kill trigger"
                    )
                time.sleep(0.01)
        else:
            time.sleep(after_seconds)
        killed_at = time.monotonic_ns()
        tree_result = kill_process_tree(proc)
        if tree_result["clean"] != "true":
            raise ExecutionError("crash process group termination could not be verified")
    finally:
        if proc.poll() is None or _posix_group_has_live_members(proc.pid):
            tree_result = terminate_process_tree(proc)

    evidence = record_evidence(
        ctx, phase, family, run_dir, phase.get("post_crash_evidence") or []
    )
    ctx.phase_run_dirs[(phase["name"], family)] = run_dir
    return {
        "counts": new_counts(),
        "crash": {
            "mechanism": "sigkill" if os.name == "posix" else "terminate_process",
            "uptime_before_kill_ns": killed_at - started,
            "work_exit_code": proc.returncode,
            "process_tree": tree_result,
        },
        "evidence": evidence,
        "comparisons": [],
    }


def execute_recovery(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> dict:
    recover = phase.get("recover")
    counts = new_counts()
    if isinstance(recover, dict):
        result = ctx.command(recover, family, run_dir)
        counts["offered"] += 1
        counts["admitted"] += 1
        ok = command_succeeded(result, recover.get("expect_exit_code", 0))
        counts["completed" if ok else "failed"] += 1
        if not ok:
            raise ExecutionError(
                f"recovery command failed for phase {phase['name']!r} family "
                f"{family!r}: {command_failure_detail(result)}"
            )
    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    failures = [item for item in compares if not item["pass"]]
    if failures:
        raise ExecutionError(
            f"recovery phase {phase['name']!r} family {family!r} compare "
            f"failures: {failures}"
        )
    return {
        "counts": counts,
        "recovered_against": relative_to_output(ctx, run_dir),
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_backup_restore(
    ctx: RunContext, phase: dict, family: str, run_dir: Path
) -> dict:
    steps = phase.get("steps") or []
    if not steps:
        raise ConfigError(f"backup_restore phase {phase['name']!r} has no steps")
    step_results = []
    for step in steps:
        require_safe_identifier(step.get("name", "step"), "backup_restore step name")
        result = ctx.command(step, family, run_dir)
        ok = command_succeeded(result, step.get("expect_exit_code", 0))
        step_results.append(
            {
                "name": step.get("name"),
                "exit_code": result["exit_code"],
                "timed_out": result["timed_out"],
                "wall_ns": result["wall_ns"],
                "pass": ok,
                "process_tree": result["process_tree"],
            }
        )
        if not ok:
            raise ExecutionError(
                f"backup_restore step {step.get('name')!r} failed in phase "
                f"{phase['name']!r} family {family!r}: {command_failure_detail(result)}"
            )
    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    failures = [item for item in compares if not item["pass"]]
    if failures:
        raise ExecutionError(
            f"backup_restore phase {phase['name']!r} family {family!r} compare "
            f"failures: {failures}"
        )
    return {
        "counts": new_counts(),
        "steps": step_results,
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_aa_pairs(ctx: RunContext, phase: dict, family: str) -> dict:
    target_name = phase["target_phase"]
    target = next(
        (item for item in ctx.workload["phases"] if item["name"] == target_name),
        None,
    )
    if target is None:
        raise ConfigError(
            f"aa_pairs phase {phase['name']!r} targets unknown phase {target_name!r}"
        )
    if target.get("kind") != "closed_loop":
        raise ConfigError(
            f"aa_pairs target {target_name!r} must be a closed_loop phase"
        )
    pairs = _config_int(phase.get("pairs", 5), "aa_pairs pairs", 1)
    margin_multiplier = _config_number(
        phase.get("margin_multiplier", 2.0), "aa_pairs margin_multiplier", 0, strict=True
    )

    observations: list[dict] = []
    for pair_index in range(pairs):
        for member in ("A", "B"):
            label = f"pair{pair_index}_{member}"
            run_dir = fresh_run_dir(ctx, phase, family, label)
            execute_setup(ctx, target, family, run_dir)
            body = execute_closed_loop(ctx, target, family, run_dir)
            latency = body["latency"]["response_ns"]
            throughput = body["throughput_ops_per_second"]
            observations.append(
                {
                    "pair": pair_index,
                    "member": member,
                    "run_dir": relative_to_output(ctx, run_dir),
                    "p50_response_ns": latency["p50_ns"],
                    "throughput_ops_per_second": throughput,
                    "completed": body["counts"]["completed"],
                }
            )
            ctx.runs.append(
                {
                    "phase": phase["name"],
                    "family": family,
                    "kind": "closed_loop",
                    "repetition_label": label,
                    "status": "completed",
                    **body,
                }
            )

    deltas: list[dict] = []
    for pair_index in range(pairs):
        member_a = observations[2 * pair_index]
        member_b = observations[2 * pair_index + 1]
        pair_delta: dict[str, object] = {"pair": pair_index}
        for metric in ("p50_response_ns", "throughput_ops_per_second"):
            value_a = member_a[metric]
            value_b = member_b[metric]
            if value_a is None or value_b is None:
                pair_delta[f"{metric}_relative_delta"] = None
                continue
            midpoint = (value_a + value_b) / 2.0
            pair_delta[f"{metric}_relative_delta"] = (
                abs(value_a - value_b) / midpoint if midpoint > 0 else 0.0
            )
        deltas.append(pair_delta)

    noise_floor = {}
    for metric in ("p50_response_ns", "throughput_ops_per_second"):
        values = [
            item[f"{metric}_relative_delta"]
            for item in deltas
            if item[f"{metric}_relative_delta"] is not None
        ]
        floor = max(values) if values else None
        noise_floor[metric] = {
            "aa_noise_floor_relative": floor,
            "regression_margin_relative": (
                floor * margin_multiplier if floor is not None else None
            ),
        }

    return {
        "counts": new_counts(),
        "aa": {
            "target_phase": target_name,
            "pairs": pairs,
            "margin_multiplier": margin_multiplier,
            "observations": observations,
            "pair_relative_deltas": deltas,
            "noise_floor": noise_floor,
            "note": (
                "A/A margins are per-machine noise floors; regression gates must "
                "be re-baselined per platform (Linux/Windows/macOS)."
            ),
        },
    }


def execute_phase_for_family(
    ctx: RunContext, phase: dict, family: str, allow_pending: bool
) -> None:
    reason = effective_phase_pending_reason(ctx.workload, phase)
    if reason is not None:
        if not allow_pending:
            raise ConfigError(
                f"phase {phase['name']!r} is pending ({reason}); refusing to "
                f"execute. Re-run with --allow-pending to record it as not run."
            )
        ctx.runs.append(
            {
                "phase": phase["name"],
                "family": family,
                "kind": phase["kind"],
                "status": "pending",
                "pending_reason": reason,
            }
        )
        return

    if phase["kind"] == "aa_pairs":
        body = execute_aa_pairs(ctx, phase, family)
        ctx.runs.append(
            {
                "phase": phase["name"],
                "family": family,
                "kind": phase["kind"],
                "status": "completed",
                **body,
            }
        )
        return

    if phase["kind"] == "recovery":
        source_dir = ctx.phase_run_dirs.get((phase["depends_on"], family))
        if source_dir is None:
            raise ExecutionError(
                f"recovery phase {phase['name']!r} has no crashed runner-owned store copy"
            )
        run_dir = source_dir
        body = execute_recovery(ctx, phase, family, run_dir)
    else:
        run_dir = fresh_run_dir(ctx, phase, family, None)
        execute_setup(ctx, phase, family, run_dir)
        if phase["kind"] == "closed_loop":
            body = execute_closed_loop(ctx, phase, family, run_dir)
        elif phase["kind"] == "open_loop":
            body = execute_open_loop(ctx, phase, family, run_dir)
        elif phase["kind"] == "crash":
            body = execute_crash(ctx, phase, family, run_dir)
        elif phase["kind"] == "backup_restore":
            body = execute_backup_restore(ctx, phase, family, run_dir)
        else:  # pragma: no cover - guarded by load_workload
            raise ConfigError(f"unknown phase kind {phase['kind']!r}")
    ctx.runs.append(
        {
            "phase": phase["name"],
            "family": family,
            "kind": phase["kind"],
            "run_dir": relative_to_output(ctx, run_dir),
            "status": "completed",
            **body,
        }
    )
