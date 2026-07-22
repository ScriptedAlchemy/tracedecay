"""Logical evidence capture and result artifact validation."""

from __future__ import annotations

import json
import os
import sqlite3
from pathlib import Path
from typing import Any

from runner_contract import (
    ConfigError, ExecutionError, LOGICAL_SQLITE_EVIDENCE_SCHEMA,
    RESULT_ARTIFACT_ID, RESULT_SCHEMA_VERSION, SHA256_HEX,
)
from safe_paths import assert_safe_path_components, sha256_file, sha256_text
from process_execution import (
    command_failure_detail, command_succeeded, require_safe_identifier,
    safe_expanded_path,
)
from workload_model import _config_int, counts_invariants_ok
from run_context import RunContext

def relative_to_output(ctx: RunContext, path: Path) -> str:
    safe_expanded_path(str(path), ctx.output_root, "result-relative path")
    return path.relative_to(ctx.output_root).as_posix()


def _sqlite_identifier(value: object, role: str) -> str:
    return require_safe_identifier(value, role)


def capture_logical_sqlite_evidence(target: Path, spec: dict) -> dict[str, Any]:
    """Capture logical SQLite state without publishing raw DB bytes/rows/FTS text."""
    target = assert_safe_path_components(target, "logical SQLite evidence", require_directory=False)
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(f"{target.as_uri()}?mode=ro", uri=True)
        connection.execute("PRAGMA query_only = ON")
        integrity_rows = connection.execute("PRAGMA integrity_check").fetchall()
        schema_rows = connection.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_master "
            "WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name"
        ).fetchall()
        tables = []
        for raw_name in spec.get("tables") or []:
            name = _sqlite_identifier(raw_name, "SQLite evidence table")
            quoted = '"' + name.replace('"', '""') + '"'
            count = connection.execute(f"SELECT COUNT(*) FROM {quoted}").fetchone()[0]
            tables.append({"table_id": name, "row_count": int(count)})
        fts = []
        fts_probes = spec.get("fts_probes") or []
        if spec.get("require_fts_probes") is True and not fts_probes:
            raise ConfigError("logical SQLite evidence requires at least one FTS probe")
        for probe in fts_probes:
            if not isinstance(probe, dict):
                raise ConfigError("SQLite FTS probes must be objects")
            probe_id = require_safe_identifier(probe.get("name", ""), "SQLite FTS probe")
            table = _sqlite_identifier(probe.get("table", ""), "SQLite FTS table")
            query = probe.get("query")
            if not isinstance(query, str):
                raise ConfigError(f"SQLite FTS probe {probe_id!r} needs a string query")
            limit = _config_int(probe.get("limit", 1000), f"SQLite FTS probe {probe_id!r} limit", 1)
            if limit > 10000:
                raise ConfigError(f"SQLite FTS probe {probe_id!r} limit must be 1..10000")
            projection = probe.get("projection", "rowid")
            if projection not in {"rowid", "rowid_rank_snippet"}:
                raise ConfigError(f"SQLite FTS probe {probe_id!r} has unknown projection")
            quoted = '"' + table.replace('"', '""') + '"'
            if projection == "rowid_rank_snippet":
                rows = connection.execute(
                    f"SELECT rowid, bm25({quoted}), "
                    f"snippet({quoted}, ?, '[', ']', '...', 64) "
                    f"FROM {quoted} WHERE {quoted} MATCH ? "
                    f"ORDER BY bm25({quoted}), rowid LIMIT ?",
                    (-1, query, limit + 1),
                ).fetchall()
            else:
                rows = connection.execute(
                    f"SELECT rowid FROM {quoted} WHERE {quoted} MATCH ? ORDER BY rowid LIMIT ?",
                    (query, limit + 1),
                ).fetchall()
            truncated = len(rows) > limit
            rows = rows[:limit]
            row_ids = [row[0] for row in rows]
            fts.append(
                {
                    "probe_id": probe_id,
                    "projection": projection,
                    "match_count": len(row_ids),
                    "row_identity_sha256": sha256_text(
                        json.dumps(row_ids, separators=(",", ":"), ensure_ascii=False)
                    ),
                    "result_sha256": sha256_text(
                        json.dumps(rows, separators=(",", ":"), ensure_ascii=False)
                    ),
                    "truncated": truncated,
                }
            )
    except (sqlite3.Error, OSError) as exc:
        raise ExecutionError(f"logical SQLite evidence could not be captured: {type(exc).__name__}") from exc
    finally:
        try:
            if connection is not None:
                connection.close()
        except sqlite3.Error:
            pass
    integrity_text = json.dumps(integrity_rows, separators=(",", ":"), ensure_ascii=False)
    schema_text = json.dumps(schema_rows, separators=(",", ":"), ensure_ascii=False)
    return {
        "schema": LOGICAL_SQLITE_EVIDENCE_SCHEMA,
        "integrity": {
            "status": "ok" if integrity_rows == [("ok",)] else "not_ok",
            "result_sha256": sha256_text(integrity_text),
            "result_row_count": len(integrity_rows),
        },
        "schema_sha256": sha256_text(schema_text),
        "tables": tables,
        "fts": fts,
    }


def record_evidence(
    ctx: RunContext,
    phase: dict,
    family: str,
    run_dir: Path,
    evidence_specs: list[dict],
) -> dict:
    captured: dict[str, Any] = {}
    for spec in evidence_specs or []:
        name = spec.get("name")
        if not name:
            raise ConfigError(f"phase {phase['name']!r} evidence entry missing name")
        capture = spec.get("capture")
        if capture == "logical_file":
            target = ctx.expand_path(spec.get("path", ""), family, run_dir, f"evidence {name}")
            target = assert_safe_path_components(target, f"evidence {name}", require_directory=False)
            info = os.lstat(target)
            captured[name] = {
                "schema": "storage-runtime-logical-file-evidence-v1",
                "content_sha256": sha256_file(target, f"evidence {name}"),
                "size_bytes": info.st_size,
            }
        elif capture == "sqlite_logical":
            target = ctx.expand_path(spec.get("path", ""), family, run_dir, f"evidence {name}")
            logical = capture_logical_sqlite_evidence(target, spec)
            if logical["integrity"]["status"] != "ok":
                raise ExecutionError(
                    f"logical SQLite evidence {name!r} failed integrity_check"
                )
            captured[name] = logical
        elif capture == "stdout_redacted":
            result = ctx.command(spec, family, run_dir)
            if not command_succeeded(result, spec.get("expect_exit_code", 0)):
                raise ExecutionError(
                    f"evidence command {name!r} failed in phase {phase['name']!r}: "
                    f"{command_failure_detail(result)}"
                )
            captured[name] = {
                "schema": "storage-runtime-redacted-stdout-evidence-v1",
                "capture": "fts_or_stdout_redacted",
                "output": result["stdout"],
            }
        else:
            raise ConfigError(f"evidence {name!r} has unknown capture {capture!r}")
    ctx.phase_evidence[(phase["name"], family)] = captured
    return captured


def resolve_compare_ref(ctx: RunContext, phase: dict, family: str, ref: str) -> Any:
    if ":" in ref:
        phase_name, name = ref.split(":", 1)
    else:
        phase_name, name = phase["name"], ref
    evidence = ctx.phase_evidence.get((phase_name, family), {})
    if name not in evidence:
        raise ExecutionError(
            f"compare reference {ref!r} has no captured evidence for family {family!r}"
        )
    return evidence[name]


def evaluate_compares(
    ctx: RunContext, phase: dict, family: str, compare_specs: list[dict]
) -> list[dict]:
    results = []
    for spec in compare_specs or []:
        a_value = resolve_compare_ref(ctx, phase, family, spec["a"])
        b_value = resolve_compare_ref(ctx, phase, family, spec["b"])
        expect = spec.get("expect", "equal")
        if expect == "equal":
            passed = a_value == b_value
        elif expect == "different":
            passed = a_value != b_value
        else:
            raise ConfigError(f"unknown compare expectation {expect!r}")
        results.append(
            {
                "a": spec["a"],
                "b": spec["b"],
                "expect": expect,
                "pass": passed,
            }
        )
    return results


RESULT_REQUIRED_KEYS = {
    "artifact_id",
    "schema_version",
    "status",
    "evidence_status",
    "execution_scope",
    "workload",
    "frozen_identity",
    "identity_binding",
    "environment",
    "platform",
    "process_tree_control",
    "safety",
    "logical_evidence_schema",
    "input_fingerprint",
    "runs",
    "limitations",
}


def validate_open_loop_ledger(run: dict, counts: dict) -> list[str]:
    phase = run.get("phase")
    requests = run.get("requests")
    if not isinstance(requests, list):
        return [f"run {phase!r} missing overload request ledger"]
    problems: list[str] = []
    if len(requests) != counts["offered"]:
        problems.append(f"run {phase!r}: request ledger count != offered")
    ids = [request.get("request_id") for request in requests if isinstance(request, dict)]
    valid_ids = all(isinstance(value, int) and not isinstance(value, bool) for value in ids)
    if not valid_ids or len(ids) != len(requests) or len(set(ids)) != len(ids):
        problems.append(f"run {phase!r}: request ids are not unique")
    outcomes = {
        "completed": 0,
        "failed": 0,
        "shed_command_saturation": 0,
        "shed_runner_in_flight_cap": 0,
    }
    retries = 0
    for request in requests:
        if not isinstance(request, dict) or not request.get("terminal"):
            problems.append(f"run {phase!r}: offered request lacks terminal record")
            continue
        for key in ("scheduled_at_ns", "admitted_at_ns", "started_at_ns", "terminal_at_ns"):
            if key not in request:
                problems.append(f"run {phase!r}: request missing {key}")
        outcome = request.get("outcome")
        if request.get("terminal_at_ns") is None or outcome not in outcomes:
            problems.append(f"run {phase!r}: request terminal outcome is incomplete")
            continue
        outcomes[outcome] += 1
        attempts = request.get("attempts")
        if not isinstance(attempts, int) or isinstance(attempts, bool) or attempts < 0:
            problems.append(f"run {phase!r}: request attempts is invalid")
            continue
        if outcome == "shed_runner_in_flight_cap":
            if attempts != 0 or request.get("admitted_at_ns") is not None or request.get(
                "started_at_ns"
            ) is not None:
                problems.append(f"run {phase!r}: runner-shed request was marked admitted")
        else:
            if attempts < 1 or request.get("admitted_at_ns") is None or request.get(
                "started_at_ns"
            ) is None:
                problems.append(f"run {phase!r}: admitted request timing is incomplete")
            retries += max(0, attempts - 1)
        ordered_times: list[int] = []
        invalid_time = False
        for key in ("scheduled_at_ns", "admitted_at_ns", "started_at_ns", "terminal_at_ns"):
            value = request.get(key)
            if value is None:
                continue
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                invalid_time = True
                continue
            ordered_times.append(value)
        if invalid_time or any(
            earlier > later for earlier, later in zip(ordered_times, ordered_times[1:])
        ):
            problems.append(f"run {phase!r}: request timing is invalid")
    expected = {
        "completed": counts["completed"],
        "failed": counts["failed"],
        "shed_command_saturation": counts["shed"]["command_saturation"],
        "shed_runner_in_flight_cap": counts["shed"]["runner_in_flight_cap"],
    }
    if outcomes != expected:
        problems.append(f"run {phase!r}: request outcomes do not match aggregate counts")
    if retries != counts["retried"]:
        problems.append(f"run {phase!r}: request attempts do not match retried count")
    return problems


def identity_components_valid(components: object) -> bool:
    required = {
        "product_binary",
        "evidence_binary",
        "schema_manifest",
        "workload",
        "corpus",
        "config",
    }
    if not isinstance(components, dict) or set(components) != required:
        return False
    return all(
        isinstance(component, dict)
        and component.get("verified") is True
        and component.get("kind") in {"file", "tree"}
        and isinstance(component.get("sha256"), str)
        and SHA256_HEX.fullmatch(component["sha256"]) is not None
        for component in components.values()
    ) and components["product_binary"]["sha256"] != components["evidence_binary"]["sha256"]


def redacted_summary_valid(summary: object) -> bool:
    return (
        isinstance(summary, dict)
        and summary.get("redacted") is True
        and isinstance(summary.get("sha256"), str)
        and SHA256_HEX.fullmatch(summary["sha256"]) is not None
        and all(
            isinstance(summary.get(key), int)
            and not isinstance(summary.get(key), bool)
            and summary[key] >= 0
            for key in ("byte_count", "line_count")
        )
    )


def validate_result(result: dict) -> list[str]:
    problems: list[str] = []
    missing = RESULT_REQUIRED_KEYS - set(result)
    if missing:
        problems.append(f"missing result keys: {sorted(missing)}")
        return problems
    if result["artifact_id"] != RESULT_ARTIFACT_ID:
        problems.append(f"artifact_id must be {RESULT_ARTIFACT_ID!r}")
    if result["schema_version"] != RESULT_SCHEMA_VERSION:
        problems.append(f"schema_version must be {RESULT_SCHEMA_VERSION}")
    if result.get("logical_evidence_schema") != LOGICAL_SQLITE_EVIDENCE_SCHEMA:
        problems.append("logical_evidence_schema is missing or unsupported")
    if result.get("status") not in {"completed", "not_evidence", "failed_validation"}:
        problems.append("result status must be completed, not_evidence, or failed_validation")
    raw_evidence_status = result.get("evidence_status")
    evidence_status = raw_evidence_status if isinstance(raw_evidence_status, dict) else {}
    if evidence_status.get("state") not in {
        "evidence",
        "not_evidence",
    }:
        problems.append("evidence_status must explicitly state evidence or not_evidence")
    raw_scope = result.get("execution_scope")
    scope = raw_scope if isinstance(raw_scope, dict) else {}
    if scope.get("mode") not in {"full", "partial"}:
        problems.append("execution_scope must explicitly state full or partial")
    raw_environment = result.get("environment")
    env_block = raw_environment if isinstance(raw_environment, dict) else {}
    if not isinstance(raw_environment, dict):
        problems.append("environment must be an object")
    for key in (
        "os",
        "platform_id",
        "machine",
        "python_version",
        "captured_at_utc",
        "runner_version",
    ):
        if key not in env_block:
            problems.append(f"environment missing {key!r}")
    platform_block = result.get("platform", {})
    if not isinstance(platform_block, dict) or not platform_block.get("current"):
        problems.append("platform block missing normalized current platform")
    raw_process_tree = result.get("process_tree_control", {})
    process_tree = raw_process_tree if isinstance(raw_process_tree, dict) else {}
    if not process_tree.get("state"):
        problems.append("process_tree_control is missing")
    raw_workload = result.get("workload")
    workload_block = raw_workload if isinstance(raw_workload, dict) else {}
    if not isinstance(raw_workload, dict):
        problems.append("workload must be an object")

    incomplete_runs = False
    raw_runs = result.get("runs")
    runs = raw_runs if isinstance(raw_runs, list) else []
    if not isinstance(raw_runs, list):
        problems.append("runs must be a list")
        incomplete_runs = True
    for run in runs:
        if not isinstance(run, dict):
            problems.append("run entries must be objects")
            incomplete_runs = True
            continue
        if run.get("status") not in {"completed", "pending", "failed", "partial", "not_run"}:
            problems.append(f"run {run.get('phase')!r} has an invalid status")
            incomplete_runs = True
            continue
        if run.get("status") in {"pending", "failed", "partial", "not_run"}:
            incomplete_runs = True
            continue
        counts = run.get("counts")
        if counts is None:
            problems.append(f"run {run.get('phase')!r} missing counts")
            continue
        for violation in counts_invariants_ok(counts):
            problems.append(f"run {run.get('phase')!r}: {violation}")
        if isinstance(counts, dict) and isinstance(counts.get("failed"), int) and counts["failed"]:
            problems.append(f"run {run.get('phase')!r}: failed operations are not evidence")
        for comparison in run.get("comparisons", []) or []:
            if not comparison.get("pass"):
                problems.append(
                    f"run {run.get('phase')!r}: failed comparison {comparison}"
                )
        if run.get("kind") == "open_loop":
            if not counts_invariants_ok(counts):
                problems.extend(validate_open_loop_ledger(run, counts))
        raw_evidence = run.get("evidence") or {}
        if not isinstance(raw_evidence, dict):
            problems.append(f"run {run.get('phase')!r}: evidence must be an object")
            raw_evidence = {}
        for value in raw_evidence.values():
            if not isinstance(value, dict) or "schema" not in value:
                problems.append(
                    f"run {run.get('phase')!r}: evidence must use a logical/redacted schema"
                )
            elif value["schema"] == "storage-runtime-redacted-stdout-evidence-v1":
                if not redacted_summary_valid(value.get("output")):
                    problems.append(f"run {run.get('phase')!r}: stdout evidence is not redacted")
            elif value["schema"] == LOGICAL_SQLITE_EVIDENCE_SCHEMA:
                integrity = value.get("integrity")
                if not isinstance(integrity, dict) or integrity.get("status") != "ok":
                    problems.append(f"run {run.get('phase')!r}: SQLite integrity is not ok")
            elif value["schema"] == "storage-runtime-logical-file-evidence-v1":
                if workload_block.get("evidence_eligible") is True:
                    problems.append(
                        f"run {run.get('phase')!r}: product evidence uses a synthetic logical file"
                    )
            else:
                problems.append(f"run {run.get('phase')!r}: unknown evidence schema")

    if result.get("status") == "completed":
        if evidence_status.get("state") != "evidence":
            problems.append("completed result may not be marked not_evidence")
        if scope.get("mode") != "full":
            problems.append("partial/--only result must never be completed")
        if incomplete_runs:
            problems.append("pending or failed run makes completed result invalid")
        raw_binding = result.get("identity_binding")
        binding = raw_binding if isinstance(raw_binding, dict) else {}
        if binding.get("status") != "bound":
            problems.append("completed evidence must be bound to frozen identity")
        elif not identity_components_valid(binding.get("components")):
            problems.append("completed evidence has malformed identity components")
        elif (
            not isinstance(binding.get("product_commit_sha"), str)
            or len(binding["product_commit_sha"]) not in {40, 64}
            or any(
                character not in "0123456789abcdef"
                for character in binding["product_commit_sha"]
            )
        ):
            problems.append("completed evidence has invalid product commit identity")
        raw_frozen = result.get("frozen_identity")
        frozen = raw_frozen if isinstance(raw_frozen, dict) else {}
        if (
            frozen.get("status") != "supplied"
            or not isinstance(frozen.get("sha256"), str)
            or SHA256_HEX.fullmatch(frozen["sha256"]) is None
        ):
            problems.append("completed evidence requires a supplied frozen identity")
        if process_tree.get("state") != "supported_best_effort":
            problems.append("completed evidence requires verified process-tree capability")
        if workload_block.get("evidence_eligible") is not True:
            problems.append("completed evidence requires an evidence-eligible workload")
        if not isinstance(workload_block.get("sha256"), str) or SHA256_HEX.fullmatch(
            workload_block["sha256"]
        ) is None:
            problems.append("completed evidence requires a workload hash")
        input_fingerprint = result.get("input_fingerprint")
        if (
            not isinstance(input_fingerprint, dict)
            or not isinstance(input_fingerprint.get("aggregate_sha256"), str)
            or SHA256_HEX.fullmatch(input_fingerprint["aggregate_sha256"]) is None
        ):
            problems.append("completed evidence requires an input fingerprint")
        raw_safety = result.get("safety")
        safety = raw_safety if isinstance(raw_safety, dict) else {}
        input_fs = safety.get("input_filesystem")
        output_fs = safety.get("output_filesystem")
        if (
            not isinstance(input_fs, dict)
            or not isinstance(output_fs, dict)
            or input_fs.get("state") != "local"
            or output_fs.get("state") != "local"
        ):
            problems.append("completed evidence requires verified local filesystems")
        if not runs:
            problems.append("completed evidence requires at least one run")
    elif evidence_status.get("state") == "evidence":
        problems.append("non-completed result may not claim evidence")
    return problems


def result_contains_absolute_paths(result: dict) -> list[str]:
    """Machine-specific absolute paths must never enter a tracked artifact."""
    hits: list[str] = []

    def scan(node, trail: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                scan(value, f"{trail}.{key}")
        elif isinstance(node, list):
            for index, value in enumerate(node):
                scan(value, f"{trail}[{index}]")
        elif isinstance(node, str):
            if node.startswith(("/", "\\")) or (
                len(node) > 2 and node[1] == ":" and node[2] in "\\/"
            ):
                hits.append(trail)

    scan(result, "$")
    return hits
