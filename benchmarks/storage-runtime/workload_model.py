"""Workload schema validation, count invariants, and latency summaries."""

from __future__ import annotations

import json
import math
import os
import statistics
from pathlib import Path
from typing import Any

from runner_contract import ConfigError, WORKLOAD_SCHEMA_VERSION
from safe_paths import _open_read_no_follow, assert_safe_path_components
from profile_safety import normalized_platform_name
from process_execution import require_safe_identifier

def nearest_rank(sorted_samples: list[int], percentile: float) -> int | None:
    if not sorted_samples:
        return None
    rank = max(1, math.ceil(percentile / 100.0 * len(sorted_samples)))
    return sorted_samples[rank - 1]


def summarize_latency(samples_ns: list[int]) -> dict:
    ordered = sorted(samples_ns)
    return {
        "count": len(ordered),
        "min_ns": ordered[0] if ordered else None,
        "p50_ns": nearest_rank(ordered, 50),
        "p95_ns": nearest_rank(ordered, 95),
        "p99_ns": nearest_rank(ordered, 99),
        "max_ns": ordered[-1] if ordered else None,
        "sample_stddev_ns": (
            statistics.stdev(ordered) if len(ordered) >= 2 else 0.0 if ordered else None
        ),
        "percentile_method": "nearest_rank",
    }


# ---------------------------------------------------------------------------
# Counts
# ---------------------------------------------------------------------------


def new_counts() -> dict:
    return {
        "offered": 0,
        "admitted": 0,
        "completed": 0,
        "failed": 0,
        "retried": 0,
        "shed": {"runner_in_flight_cap": 0, "command_saturation": 0},
    }


def counts_invariants_ok(counts: dict) -> list[str]:
    """Return a list of violated invariants (empty when consistent)."""
    if not isinstance(counts, dict):
        return ["counts must be an object"]
    problems: list[str] = []
    scalar_keys = ("offered", "admitted", "completed", "failed", "retried")
    for key in scalar_keys:
        if not isinstance(counts.get(key), int) or isinstance(counts.get(key), bool):
            problems.append(f"{key} must be an integer")
    shed = counts.get("shed")
    if not isinstance(shed, dict):
        problems.append("shed must be an object")
    else:
        for key in ("runner_in_flight_cap", "command_saturation"):
            if not isinstance(shed.get(key), int) or isinstance(shed.get(key), bool):
                problems.append(f"shed.{key} must be an integer")
    if problems:
        return problems
    shed_runner = counts["shed"]["runner_in_flight_cap"]
    shed_command = counts["shed"]["command_saturation"]
    if counts["offered"] != counts["admitted"] + shed_runner:
        problems.append("offered != admitted + shed.runner_in_flight_cap")
    if counts["admitted"] != counts["completed"] + counts["failed"] + shed_command:
        problems.append("admitted != completed + failed + shed.command_saturation")
    for key in scalar_keys:
        if counts[key] < 0:
            problems.append(f"{key} is negative")
    for key in ("runner_in_flight_cap", "command_saturation"):
        value = counts["shed"][key]
        if value < 0:
            problems.append(f"shed.{key} is negative")
    return problems


REQUIRED_PHASE_KINDS = {
    "closed_loop",
    "open_loop",
    "crash",
    "recovery",
    "backup_restore",
    "aa_pairs",
}


def _require_unique_identifiers(values: object, role: str) -> list[str]:
    if not isinstance(values, list) or not values:
        raise ConfigError(f"{role} must be a non-empty list")
    validated = [require_safe_identifier(value, role) for value in values]
    folded = [value.casefold() for value in validated]
    if len(set(folded)) != len(folded):
        raise ConfigError(f"{role} must be unique, including case-insensitively")
    return validated


def _validate_step(step: object, role: str) -> None:
    if not isinstance(step, dict):
        raise ConfigError(f"{role} must be an object")
    argv = step.get("argv")
    if argv is None:
        return
    if not isinstance(argv, list) or not argv or not all(isinstance(arg, str) for arg in argv):
        raise ConfigError(f"{role} argv must be null or a non-empty string list")


def _config_int(value: object, role: str, minimum: int) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise ConfigError(f"{role} must be an integer >= {minimum}")
    return value


def _config_number(value: object, role: str, minimum: float, *, strict: bool = False) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise ConfigError(f"{role} must be a finite number")
    number = float(value)
    if not math.isfinite(number) or number < minimum or (strict and number == minimum):
        comparator = ">" if strict else ">="
        raise ConfigError(f"{role} must be finite and {comparator} {minimum}")
    return number


def load_workload(path: Path) -> dict:
    path = assert_safe_path_components(path, "workload", require_directory=False)
    try:
        with os.fdopen(_open_read_no_follow(path, "workload"), "r", encoding="utf-8") as handle:
            workload = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise ConfigError(f"cannot load workload {path}: {exc}") from exc
    if not isinstance(workload, dict):
        raise ConfigError(f"workload {path} must contain a JSON object")
    if workload.get("schema_version") != WORKLOAD_SCHEMA_VERSION:
        raise ConfigError(
            f"workload {path} schema_version must be {WORKLOAD_SCHEMA_VERSION}, "
            f"got {workload.get('schema_version')!r}"
        )
    for key in ("workload_id", "store_families", "phases"):
        if key not in workload:
            raise ConfigError(f"workload {path} is missing required key {key!r}")
    require_safe_identifier(workload["workload_id"], "workload_id")
    families = _require_unique_identifiers(workload["store_families"], "store families")
    if not isinstance(workload["phases"], list) or not workload["phases"]:
        raise ConfigError("workload phases must be a non-empty list")
    evidence_eligible = workload.get("evidence_eligible", False)
    if not isinstance(evidence_eligible, bool):
        raise ConfigError("workload evidence_eligible must be boolean")
    workload["evidence_eligible"] = evidence_eligible
    if "binary" in workload:
        raise ConfigError(
            "workload binary is ambiguous; use product_binary and evidence_binary"
        )
    for role in ("product_binary", "evidence_binary"):
        value = workload.get(role)
        if value is not None and (not isinstance(value, str) or not value):
            raise ConfigError(f"workload {role} must be null or a non-empty string")
    safety = workload.get("safety") or {}
    if not isinstance(safety, dict) or not isinstance(safety.get("env") or {}, dict):
        raise ConfigError("workload safety and safety.env must be objects")
    env_path_keys = safety.get("env_path_keys") or []
    if not isinstance(env_path_keys, list) or not all(
        isinstance(key, str) for key in env_path_keys
    ):
        raise ConfigError("workload safety.env_path_keys must be a string list")
    environment = workload.get("environment") or {}
    if not isinstance(environment, dict):
        raise ConfigError("workload environment must be an object")
    version_commands = environment.get("version_commands") or {}
    if not isinstance(version_commands, dict):
        raise ConfigError("workload environment.version_commands must be an object")
    for name, argv in version_commands.items():
        require_safe_identifier(name, "version command name")
        _validate_step({"argv": argv}, f"version command {name!r}")
    frozen_ref = workload.get("frozen_identity") or {}
    if not isinstance(frozen_ref, dict):
        raise ConfigError("workload frozen_identity must be an object")
    defaults = workload.get("defaults") or {}
    if not isinstance(defaults, dict):
        raise ConfigError("workload defaults must be an object")
    if "warmup" in defaults:
        _config_int(defaults["warmup"], "defaults warmup", 0)
    if "repetitions" in defaults:
        _config_int(defaults["repetitions"], "defaults repetitions", 1)
    if "timeout_seconds" in defaults:
        _config_number(defaults["timeout_seconds"], "defaults timeout_seconds", 0, strict=True)
    seen_phase_names: set[str] = set()
    for phase in workload["phases"]:
        if not isinstance(phase, dict):
            raise ConfigError("each workload phase must be an object")
        name = phase.get("name")
        kind = phase.get("kind")
        name = require_safe_identifier(name, "phase name")
        folded_name = name.casefold()
        if folded_name in seen_phase_names:
            raise ConfigError(f"phase names must be non-empty and unique, got {name!r}")
        seen_phase_names.add(folded_name)
        if kind not in REQUIRED_PHASE_KINDS:
            raise ConfigError(f"phase {name!r} has unknown kind {kind!r}")
        phase_families = _require_unique_identifiers(
            phase.get("families"), f"phase {name!r} families"
        )
        unknown = set(phase_families) - set(families)
        if unknown:
            raise ConfigError(
                f"phase {name!r} references unknown store families {sorted(unknown)}"
            )
        if kind == "recovery" and not phase.get("depends_on"):
            raise ConfigError(f"recovery phase {name!r} must declare depends_on")
        if kind == "recovery":
            require_safe_identifier(phase["depends_on"], f"phase {name!r} dependency")
        if kind == "aa_pairs" and not phase.get("target_phase"):
            raise ConfigError(f"aa_pairs phase {name!r} must declare target_phase")
        if kind == "aa_pairs":
            require_safe_identifier(phase["target_phase"], f"phase {name!r} target")
        for key in ("setup", "work", "recover", "teardown"):
            if key in phase:
                _validate_step(phase[key], f"phase {name!r} {key}")
        steps = phase.get("steps") or []
        if not isinstance(steps, list):
            raise ConfigError(f"phase {name!r} steps must be a list")
        for index, step in enumerate(steps):
            _validate_step(step, f"phase {name!r} step {index}")
            require_safe_identifier(step.get("name", ""), f"phase {name!r} step name")
        evidence_entries: list[dict] = []
        for key in ("evidence", "post_crash_evidence"):
            entries = phase.get(key) or []
            if not isinstance(entries, list):
                raise ConfigError(f"phase {name!r} {key} must be a list")
            evidence_entries.extend(entries)
        evidence_names: set[str] = set()
        for evidence in evidence_entries:
            if not isinstance(evidence, dict):
                raise ConfigError(f"phase {name!r} evidence entries must be objects")
            evidence_name = require_safe_identifier(
                evidence.get("name", ""), f"phase {name!r} evidence name"
            )
            if evidence_name.casefold() in evidence_names:
                raise ConfigError(f"phase {name!r} evidence names must be unique")
            evidence_names.add(evidence_name.casefold())
            if evidence.get("capture") not in {
                "logical_file",
                "sqlite_logical",
                "stdout_redacted",
            }:
                raise ConfigError(
                    f"phase {name!r} evidence has unsupported capture "
                    f"{evidence.get('capture')!r}"
                )
            if evidence_eligible and evidence.get("capture") == "logical_file":
                raise ConfigError("product-evidence workloads may not use logical_file fixtures")
            if evidence.get("capture") == "stdout_redacted":
                _validate_step(evidence, f"phase {name!r} evidence {evidence_name!r}")
        compares = phase.get("compare") or []
        if not isinstance(compares, list):
            raise ConfigError(f"phase {name!r} compare must be a list")
        for comparison in compares:
            if not isinstance(comparison, dict) or not all(
                isinstance(comparison.get(key), str) for key in ("a", "b")
            ):
                raise ConfigError(f"phase {name!r} comparisons need string a/b references")
            if comparison.get("expect", "equal") not in {"equal", "different"}:
                raise ConfigError(f"phase {name!r} comparison has an unknown expectation")
    phase_by_name = {phase["name"]: phase for phase in workload["phases"]}
    phase_positions = {phase["name"]: index for index, phase in enumerate(workload["phases"])}
    for phase in workload["phases"]:
        if phase["kind"] == "recovery":
            dependency = phase_by_name.get(phase["depends_on"])
            if dependency is None or dependency["kind"] != "crash":
                raise ConfigError(f"recovery phase {phase['name']!r} must depend on a crash phase")
            if phase_positions[dependency["name"]] >= phase_positions[phase["name"]]:
                raise ConfigError(f"recovery phase {phase['name']!r} dependency must run first")
        elif phase["kind"] == "aa_pairs":
            target = phase_by_name.get(phase["target_phase"])
            if target is None or target["kind"] != "closed_loop":
                raise ConfigError(f"aa_pairs phase {phase['name']!r} needs a closed_loop target")
    platforms = workload.get("platforms")
    if platforms is not None:
        if not isinstance(platforms, dict) or not isinstance(platforms.get("required"), list):
            raise ConfigError("platforms.required must be a list when platforms is declared")
        normalized = [normalized_platform_name(str(item)) for item in platforms["required"]]
        if not normalized or len(set(normalized)) != len(normalized):
            raise ConfigError("platforms.required must contain unique normalized platforms")
        unsupported = set(normalized) - {"linux", "windows", "macos"}
        if unsupported:
            raise ConfigError(f"unsupported required platform(s): {sorted(unsupported)}")
        platforms["required"] = normalized
        statuses = platforms.get("status")
        if statuses is not None:
            if not isinstance(statuses, dict):
                raise ConfigError("platforms.status must be an object")
            normalized_statuses = {
                normalized_platform_name(str(key)): value for key, value in statuses.items()
            }
            if set(normalized_statuses) != set(normalized):
                raise ConfigError("platforms.status must cover exactly platforms.required")
            platforms["status"] = normalized_statuses
    return workload


def phase_pending_reason(phase: dict) -> str | None:
    """A phase is pending when any executable step lacks a concrete argv."""
    if phase.get("pending_reason"):
        return str(phase["pending_reason"])
    steps: list[dict] = []
    for key in ("setup", "work", "recover", "teardown"):
        if isinstance(phase.get(key), dict):
            steps.append(phase[key])
    steps.extend(phase.get("steps") or [])
    for evidence in phase.get("evidence") or []:
        if evidence.get("capture") == "stdout_redacted":
            steps.append(evidence)
    for evidence in phase.get("post_crash_evidence") or []:
        if evidence.get("capture") == "stdout_redacted":
            steps.append(evidence)
    if phase.get("kind") == "aa_pairs":
        return None
    for step in steps:
        if step.get("argv") is None:
            return "step has null argv (product command not yet wired)"
    if phase.get("kind") in {"closed_loop", "open_loop", "crash"} and not isinstance(
        phase.get("work"), dict
    ):
        return "missing work command"
    return None


def effective_phase_pending_reason(workload: dict, phase: dict) -> str | None:
    """Include an A/A phase's closed-loop target in pending preflight."""
    reason = phase_pending_reason(phase)
    if reason is not None or phase.get("kind") != "aa_pairs":
        return reason
    target_name = phase.get("target_phase")
    target = next((item for item in workload["phases"] if item["name"] == target_name), None)
    if target is None:
        return f"target phase {target_name!r} is unknown"
    target_reason = phase_pending_reason(target)
    if target_reason is not None:
        return f"target phase {target['name']!r} is pending ({target_reason})"
    return None


def _fingerprint_matches_bound(actual: dict[str, Any], bound: dict[str, Any]) -> bool:
    return (
        actual.get("kind") == bound.get("kind")
        and actual.get("sha256", actual.get("aggregate_sha256")) == bound.get("sha256")
        and actual.get("size_bytes") == bound.get("size_bytes")
        and actual.get("file_count") == bound.get("file_count")
    )
