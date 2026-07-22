#!/usr/bin/env python3
"""Fail-closed adapter for real TraceDecay storage-runtime product probes.

The adapter accepts distinct explicitly supplied product/evidence binaries and
an isolated fixture copy.
It never searches the host profile. FTS query text lives in the fixture
manifest. S11 maintenance, Doctor, repair, quarantine, backup, and restore
evidence is accepted only from three fixed product-facing gate commands with
strict typed evidence; unavailable commands remain not-run.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import subprocess
import sys
import uuid
from pathlib import Path


_SCRIPT_DIR = Path(__file__).resolve().parent
if str(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_DIR))

import run_storage_baseline as runner
from soak.executor import execute_fixed_argv
from soak.schemas import (
    S6_GATE_BINDINGS,
    product_adapter_output_valid,
    s6_gate_evidence_eligible,
    validate_s6_gate_evidence,
)


MANIFEST_NAME = "storage-runtime-fixture-v1.json"
RESULT_SCHEMA = "tracedecay-storage-runtime-product-probe-v1"
S11_RESULT_SCHEMA = "tracedecay-storage-runtime-product-probe-v2"
S11_GATE_IDS = tuple(S6_GATE_BINDINGS)


class AdapterError(RuntimeError):
    """Expected configuration or product-command failure."""


def _absolute(path: str, role: str) -> Path:
    candidate = Path(path)
    if not candidate.is_absolute():
        raise AdapterError(f"{role} must be an absolute path")
    return candidate


def _fixture_child(fixture: Path, relative: object, role: str) -> Path:
    if not isinstance(relative, str) or not relative:
        raise AdapterError(f"{role} must be a non-empty relative path")
    relative_path = Path(relative)
    if relative_path.is_absolute() or ".." in relative_path.parts:
        raise AdapterError(f"{role} must be a non-empty relative path")
    candidate = fixture / relative_path
    try:
        candidate.relative_to(fixture)
        return runner.assert_safe_path_components(
            candidate, role, require_directory=True
        )
    except (ValueError, runner.RunnerError) as exc:
        raise AdapterError(f"{role} must stay inside the copied fixture") from exc


def _load_fixture(
    fixture: Path, forbidden: list[tuple[str, Path]]
) -> tuple[dict, Path, Path]:
    try:
        fixture = runner.guard_path(fixture, "copied fixture", forbidden)
        fixture = runner.validate_safe_tree(fixture, "copied fixture")
        _manifest_path, manifest = runner.load_safe_json(
            fixture / MANIFEST_NAME, "fixture manifest"
        )
    except (runner.RunnerError, UnicodeError) as exc:
        raise AdapterError("fixture manifest must be readable UTF-8 JSON") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise AdapterError("fixture manifest schema_version must be 1")
    project = _fixture_child(fixture, manifest.get("project_root"), "project_root")
    profile = _fixture_child(fixture, manifest.get("profile_root"), "profile_root")
    return manifest, project, profile


def load_fixture(fixture: Path) -> tuple[dict, Path, Path]:
    """Load a fixture without exposing a live-profile guard bypass to callers."""
    return _load_fixture(
        fixture, runner.forbidden_profile_roots(dict(os.environ), Path.home())
    )


def product_command(
    binary: Path, manifest: dict, project: Path, family: str
) -> list[str]:
    queries = manifest.get("fts_queries")
    query = queries.get(family) if isinstance(queries, dict) else None
    if not isinstance(query, str) or not query:
        raise AdapterError(f"fixture has no non-empty FTS query for family {family!r}")
    if family == "graph":
        tool = "search"
        arguments = {
            "query": query,
            "format": "json",
            "limit": 50,
        }
    elif family == "session":
        tool = "message_search"
        arguments = {"query": query, "format": "json", "limit": 50}
    else:
        raise AdapterError("FTS product probe supports only graph and session families")
    return [
        str(binary),
        "tool",
        "--project",
        str(project),
        tool,
        "--args",
        runner.canonical_compact_json(arguments, ensure_ascii=True),
    ]


def _pending_outcome(gate_id: str) -> dict:
    if gate_id == "storage-runtime-maintenance-doctor-v1":
        return {
            "maintenance_reopened": False,
            "doctor_quick_check": "not_observed",
            "doctor_integrity_check": "not_observed",
            "writer_state": "not_observed",
            "reader_state": "not_observed",
            "wal_enabled": False,
        }
    if gate_id == "storage-runtime-crash-recovery-repair-v1":
        return {
            "crashes_requested": 0,
            "crashes_completed": 0,
            "recoveries_completed": 0,
            "doctor_detected_fault": False,
            "repair_class": "not_observed",
            "repair_receipt_bound": False,
            "quarantine_preserved": False,
            "recovery_health": "not_observed",
        }
    if gate_id == "storage-runtime-backup-restore-v1":
        return {
            "restores_requested": 0,
            "backups_completed": 0,
            "restores_completed": 0,
            "backup_manifest_verified": False,
            "artifact_digests_verified": False,
            "restore_verified": False,
            "replacement_published": False,
            "restored_binding_newer": False,
        }
    raise AdapterError(f"unknown S11 gate {gate_id!r}")


def pending_gate_evidence(gate_id: str, reason: str) -> dict:
    if gate_id not in S6_GATE_BINDINGS:
        raise AdapterError(f"unknown S11 gate {gate_id!r}")
    document = {
        "schema": "storage-runtime-s6-gate-evidence-v1",
        "gate_id": gate_id,
        "status": "not_run",
        "evidence_status": {"state": "not_evidence", "reasons": [reason]},
        "api_bindings": list(S6_GATE_BINDINGS[gate_id]),
        "fixture_sha256": None,
        "product_commit_sha": None,
        "product_binary_sha256": None,
        "evidence_binary_sha256": None,
        "logical_evidence": [],
        "outcome": _pending_outcome(gate_id),
    }
    validate_s11_gate_evidence(gate_id, document)
    return document


def validate_s11_gate_evidence(gate_id: str, document: object) -> dict:
    if gate_id not in S6_GATE_BINDINGS:
        raise AdapterError(f"unknown S11 gate {gate_id!r}")
    try:
        validate_s6_gate_evidence(document)
    except runner.ConfigError as exc:
        raise AdapterError(f"S11 typed evidence was rejected: {exc}") from exc
    if not isinstance(document, dict) or document.get("gate_id") != gate_id:
        raise AdapterError("S11 typed evidence gate identity mismatch")
    status = document["status"]
    evidence_state = document["evidence_status"]["state"]
    if status != "completed":
        if evidence_state != "not_evidence":
            raise AdapterError("unexecuted S11 gate may not claim evidence")
        return document
    if not s6_gate_evidence_eligible(document):
        raise AdapterError(f"completed S11 gate failed typed outcome checks: {gate_id}")
    return document


def s11_gate_commands(
    evidence_binary: Path,
    fixture: Path,
    output_root: Path,
    *,
    fixture_sha256: str,
    product_commit_sha: str,
    product_binary_sha256: str,
    evidence_binary_sha256: str,
    crash_count: int = 0,
    restore_rehearsals: int = 0,
) -> list[dict]:
    """Resolve fixed S6 gate IDs to one evidence-binary command each."""
    commands = []
    for gate_id, api_bindings in S6_GATE_BINDINGS.items():
        output = output_root / f"{gate_id}.json"
        argv = [
            str(evidence_binary),
            "--gate",
            gate_id,
            "--fixture",
            str(fixture),
            "--output",
            str(output),
            "--fixture-sha256",
            fixture_sha256,
            "--product-commit-sha",
            product_commit_sha,
            "--product-binary-sha256",
            product_binary_sha256,
            "--evidence-binary-sha256",
            evidence_binary_sha256,
        ]
        if gate_id == "storage-runtime-crash-recovery-repair-v1":
            argv.extend(["--crash-count", str(crash_count)])
        elif gate_id == "storage-runtime-backup-restore-v1":
            argv.extend(["--restore-rehearsals", str(restore_rehearsals)])
        commands.append(
            {
                "gate_id": gate_id,
                "api_bindings": list(api_bindings),
                "argv": argv,
            }
        )
    return commands


def isolated_environment(
    base_env: dict[str, str],
    child_sandbox: dict[str, Path],
    forbidden: list[tuple[str, Path]],
) -> dict[str, str]:
    return runner.build_child_env(base_env, {}, [], forbidden, child_sandbox)


def _prepare_fts_invocation(
    product_binary: Path,
    fixture: Path,
    sandbox: Path,
    forbidden: list[tuple[str, Path]],
) -> tuple[Path, dict, Path, Path, dict[str, Path], dict]:
    """Create a fresh private invocation and copy only its validated fixture."""
    product_binary = runner.guard_path(product_binary, "product binary", forbidden)
    product_binary_identity = runner.binary_identity(product_binary)
    fixture = runner.guard_path(fixture, "fixture", forbidden)
    fixture, fixture_snapshot = runner.snapshot_safe_tree(fixture, "fixture")
    sandbox = runner.guard_path(sandbox, "sandbox", forbidden)
    sandbox = runner.assert_safe_path_components(
        sandbox, "sandbox", require_directory=True
    )
    runner.require_disjoint_roots(fixture, sandbox)
    runner.reject_network_filesystem(fixture, "fixture")
    runner.reject_network_filesystem(sandbox, "sandbox")

    invocation = runner.create_fresh_directory(
        sandbox / f"product-adapter-{uuid.uuid4().hex}", "product adapter invocation"
    )
    copied_fixture = runner.copy_safe_tree(
        fixture,
        invocation / "fixture",
        "product adapter fixture",
        source_snapshot=fixture_snapshot,
    )
    manifest, project, profile = _load_fixture(copied_fixture, forbidden)
    runner.require_disjoint_roots(copied_fixture, invocation / "sandbox")
    child_sandbox = runner.create_child_sandbox(
        invocation, "product adapter", data_root=profile
    )
    return (
        product_binary,
        manifest,
        project,
        copied_fixture,
        child_sandbox,
        product_binary_identity,
    )


def _write_result(child_sandbox: dict[str, Path], result: dict) -> None:
    runner.atomic_write_json_new(
        child_sandbox["output"] / "product-adapter-result.json",
        result,
        "product adapter result",
    )


def run_fts(product_binary: Path, fixture: Path, sandbox: Path, family: str) -> dict:
    base_env = dict(os.environ)
    forbidden = runner.forbidden_profile_roots(base_env, Path.home())
    try:
        (
            product_binary,
            manifest,
            project,
            _copied_fixture,
            child_sandbox,
            product_binary_identity,
        ) = _prepare_fts_invocation(product_binary, fixture, sandbox, forbidden)
        if runner.binary_identity(product_binary) != product_binary_identity:
            raise AdapterError("product binary changed after safety preflight")
        argv = product_command(product_binary, manifest, project, family)
        env = isolated_environment(base_env, child_sandbox, forbidden)
    except runner.RunnerError as exc:
        raise AdapterError(str(exc)) from exc
    try:
        completed = subprocess.run(
            argv,
            cwd=str(child_sandbox["cwd"]),
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise AdapterError("TraceDecay product probe could not complete") from exc
    try:
        # A product process may mutate only its private copy. Reject links or
        # special files it creates before publishing any adapter output.
        runner.validate_safe_tree(
            child_sandbox["sandbox"].parent, "product adapter output"
        )
    except runner.RunnerError as exc:
        raise AdapterError(str(exc)) from exc
    if completed.returncode != 0:
        raise AdapterError(f"TraceDecay product probe exited {completed.returncode}")
    try:
        product_output = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise AdapterError("TraceDecay product probe did not return JSON") from exc
    if not isinstance(product_output, (dict, list)):
        raise AdapterError("TraceDecay product probe returned a non-structured JSON value")
    canonical_output = runner.canonical_compact_json(product_output)
    result = {
        "schema": RESULT_SCHEMA,
        "status": "not_evidence",
        "evidence_status": {
            "state": "not_evidence",
            "reasons": ["standalone adapter probe is not a wired S0 workload execution"],
        },
        "operation": "fts",
        "family": family,
        "product_output": {
            "redacted": True,
            "sha256": runner.sha256_text(canonical_output),
            "byte_count": len(canonical_output.encode("utf-8")),
        },
    }
    try:
        _write_result(child_sandbox, result)
        runner.validate_safe_tree(
            child_sandbox["sandbox"].parent, "product adapter output"
        )
    except runner.RunnerError as exc:
        raise AdapterError(str(exc)) from exc
    return result


async def _execute_s11_commands(
    commands: list[dict],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout_seconds: float,
) -> list[dict]:
    gates = []
    for command in commands:
        gate_id = command["gate_id"]
        try:
            execution = await execute_fixed_argv(
                command["argv"],
                cwd=cwd,
                env=env,
                timeout_seconds=timeout_seconds,
            )
        except (OSError, runner.RunnerError) as exc:
            gates.append(
                pending_gate_evidence(
                    gate_id, f"product adapter command failed: {type(exc).__name__}"
                )
            )
            continue
        if (
            execution["exit_code"] != 0
            or execution["timed_out"]
            or not execution["process_tree_clean"]
            or execution["stdout_truncated"]
            or execution["stderr_truncated"]
        ):
            gates.append(
                pending_gate_evidence(
                    gate_id,
                    "product adapter command did not complete cleanly",
                )
            )
            continue
        try:
            document = json.loads(execution["stdout"].decode("utf-8"))
            gates.append(validate_s11_gate_evidence(gate_id, document))
        except (UnicodeError, json.JSONDecodeError, AdapterError):
            gates.append(
                pending_gate_evidence(
                    gate_id, "product adapter returned invalid typed evidence"
                )
            )
    return gates


def run_s11(
    product_binary: Path,
    evidence_binary: Path,
    fixture: Path,
    sandbox: Path,
    *,
    fixture_sha256: str,
    product_commit_sha: str,
    product_binary_sha256: str,
    evidence_binary_sha256: str,
    crash_count: int = 0,
    restore_rehearsals: int = 0,
    timeout_seconds: float = 300.0,
) -> dict:
    """Run fixed evidence-binary S6 gates against a copied product fixture."""
    for name, value in (
        ("crash_count", crash_count),
        ("restore_rehearsals", restore_rehearsals),
    ):
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise AdapterError(f"{name} must be a non-negative integer")
    if (
        not isinstance(fixture_sha256, str)
        or len(fixture_sha256) != 64
        or any(character not in "0123456789abcdef" for character in fixture_sha256)
    ):
        raise AdapterError("fixture_sha256 must be a lowercase SHA-256")
    if (
        not isinstance(product_commit_sha, str)
        or len(product_commit_sha) not in {40, 64}
        or any(character not in "0123456789abcdef" for character in product_commit_sha)
    ):
        raise AdapterError("product_commit_sha must be a lowercase commit identity")
    for name, value in (
        ("product_binary_sha256", product_binary_sha256),
        ("evidence_binary_sha256", evidence_binary_sha256),
    ):
        if (
            not isinstance(value, str)
            or len(value) != 64
            or any(character not in "0123456789abcdef" for character in value)
        ):
            raise AdapterError(f"{name} must be a lowercase SHA-256")
    if (
        isinstance(timeout_seconds, bool)
        or not isinstance(timeout_seconds, (int, float))
        or not math.isfinite(float(timeout_seconds))
        or timeout_seconds <= 0
    ):
        raise AdapterError("timeout_seconds must be finite and positive")
    base_env = dict(os.environ)
    forbidden = runner.forbidden_profile_roots(base_env, Path.home())
    try:
        (
            product_binary,
            _manifest,
            _project,
            copied_fixture,
            child_sandbox,
            product_identity,
        ) = _prepare_fts_invocation(product_binary, fixture, sandbox, forbidden)
        evidence_binary = runner.guard_path(
            evidence_binary, "evidence binary", forbidden
        )
        evidence_identity = runner.binary_identity(evidence_binary)
        if product_identity["sha256"] == evidence_identity["sha256"]:
            raise AdapterError("product and evidence binaries must be distinct artifacts")
        if product_identity["sha256"] != product_binary_sha256:
            raise AdapterError("product binary identity mismatch")
        if evidence_identity["sha256"] != evidence_binary_sha256:
            raise AdapterError("evidence binary identity mismatch")
        if runner.binary_identity(product_binary) != product_identity:
            raise AdapterError("product binary changed after safety preflight")
        if runner.binary_identity(evidence_binary) != evidence_identity:
            raise AdapterError("evidence binary changed after safety preflight")
        copied_identity = runner.fingerprint_tree(
            copied_fixture, "S11 copied fixture"
        )["aggregate_sha256"]
        if copied_identity != fixture_sha256:
            raise AdapterError("S11 copied fixture identity mismatch")
        commands = s11_gate_commands(
            evidence_binary,
            copied_fixture,
            child_sandbox["output"],
            fixture_sha256=fixture_sha256,
            product_commit_sha=product_commit_sha,
            product_binary_sha256=product_binary_sha256,
            evidence_binary_sha256=evidence_binary_sha256,
            crash_count=crash_count,
            restore_rehearsals=restore_rehearsals,
        )
        env = isolated_environment(base_env, child_sandbox, forbidden)
    except runner.RunnerError as exc:
        raise AdapterError(str(exc)) from exc
    gates = asyncio.run(
        _execute_s11_commands(
            commands,
            cwd=child_sandbox["cwd"],
            env=env,
            timeout_seconds=timeout_seconds,
        )
    )
    if any(
        gate["status"] == "completed"
        and (
            gate["fixture_sha256"] != fixture_sha256
            or gate["product_commit_sha"] != product_commit_sha
            or gate["product_binary_sha256"] != product_binary_sha256
            or gate["evidence_binary_sha256"] != evidence_binary_sha256
        )
        for gate in gates
    ):
        raise AdapterError("S11 gate output identity mismatch")
    if runner.binary_identity(product_binary) != product_identity:
        raise AdapterError("product binary changed during S11 execution")
    if runner.binary_identity(evidence_binary) != evidence_identity:
        raise AdapterError("evidence binary changed during S11 execution")
    reasons = [
        reason
        for gate in gates
        if gate["evidence_status"]["state"] != "evidence"
        for reason in gate["evidence_status"]["reasons"]
    ]
    reasons.append(
        "standalone S11 adapter output requires soak receipt and baseline acceptance"
    )
    result = {
        "schema": S11_RESULT_SCHEMA,
        "status": "not_evidence",
        "evidence_status": {"state": "not_evidence", "reasons": reasons},
        "operation": "s11_product_gates",
        "gates": gates,
    }
    if not product_adapter_output_valid(result):
        raise AdapterError("S11 product adapter result failed schema validation")
    try:
        runner.validate_safe_tree(
            child_sandbox["sandbox"].parent, "S11 product adapter output"
        )
        _write_result(child_sandbox, result)
        runner.validate_safe_tree(
            child_sandbox["sandbox"].parent, "S11 product adapter output"
        )
    except runner.RunnerError as exc:
        raise AdapterError(str(exc)) from exc
    return result


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="operation", required=True)
    fts = subparsers.add_parser("fts")
    fts.add_argument("--product-binary", required=True)
    fts.add_argument("--fixture", required=True)
    fts.add_argument("--sandbox", required=True)
    fts.add_argument("--family", required=True, choices=("graph", "session"))
    s11 = subparsers.add_parser("s11")
    s11.add_argument("--product-binary", required=True)
    s11.add_argument("--evidence-binary", required=True)
    s11.add_argument("--fixture", required=True)
    s11.add_argument("--sandbox", required=True)
    s11.add_argument("--fixture-sha256", required=True)
    s11.add_argument("--product-commit-sha", required=True)
    s11.add_argument("--product-binary-sha256", required=True)
    s11.add_argument("--evidence-binary-sha256", required=True)
    s11.add_argument("--crash-count", required=True, type=int)
    s11.add_argument("--restore-rehearsals", required=True, type=int)
    s11.add_argument("--timeout-seconds", type=float, default=300.0)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.operation == "fts":
            result = run_fts(
                _absolute(args.product_binary, "product binary"),
                _absolute(args.fixture, "fixture"),
                _absolute(args.sandbox, "sandbox"),
                args.family,
            )
        else:
            result = run_s11(
                _absolute(args.product_binary, "product binary"),
                _absolute(args.evidence_binary, "evidence binary"),
                _absolute(args.fixture, "fixture"),
                _absolute(args.sandbox, "sandbox"),
                fixture_sha256=args.fixture_sha256,
                product_commit_sha=args.product_commit_sha,
                product_binary_sha256=args.product_binary_sha256,
                evidence_binary_sha256=args.evidence_binary_sha256,
                crash_count=args.crash_count,
                restore_rehearsals=args.restore_rehearsals,
                timeout_seconds=args.timeout_seconds,
            )
    except AdapterError as exc:
        print(f"product adapter refused: {exc}", file=sys.stderr)
        return 2
    print(runner.canonical_compact_json(result, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
