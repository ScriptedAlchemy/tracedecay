#!/usr/bin/env python3
"""Fail-closed adapter for real TraceDecay storage-runtime product probes.

The adapter accepts only an explicitly supplied binary and isolated fixture copy.
It never searches the host profile.  Fixture-specific query text lives in the
fixture manifest rather than being synthesized here.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import uuid
from pathlib import Path


_SCRIPT_DIR = Path(__file__).resolve().parent
if str(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_DIR))

import run_storage_baseline as runner


MANIFEST_NAME = "storage-runtime-fixture-v1.json"
RESULT_SCHEMA = "tracedecay-storage-runtime-product-probe-v1"


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
        json.dumps(arguments, sort_keys=True, separators=(",", ":")),
    ]


def isolated_environment(
    base_env: dict[str, str],
    child_sandbox: dict[str, Path],
    forbidden: list[tuple[str, Path]],
) -> dict[str, str]:
    return runner.build_child_env(base_env, {}, [], forbidden, child_sandbox)


def _prepare_fts_invocation(
    binary: Path, fixture: Path, sandbox: Path, forbidden: list[tuple[str, Path]]
) -> tuple[Path, dict, Path, dict[str, Path], dict]:
    """Create a fresh private invocation and copy only its validated fixture."""
    binary = runner.guard_path(binary, "binary", forbidden)
    binary_identity = runner.binary_identity(binary)
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
    return binary, manifest, project, child_sandbox, binary_identity


def _write_result(child_sandbox: dict[str, Path], result: dict) -> None:
    runner.atomic_write_new(
        child_sandbox["output"] / "product-adapter-result.json",
        json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n",
        "product adapter result",
    )


def run_fts(binary: Path, fixture: Path, sandbox: Path, family: str) -> dict:
    base_env = dict(os.environ)
    forbidden = runner.forbidden_profile_roots(base_env, Path.home())
    try:
        (
            binary,
            manifest,
            project,
            child_sandbox,
            binary_identity,
        ) = _prepare_fts_invocation(binary, fixture, sandbox, forbidden)
        if runner.binary_identity(binary) != binary_identity:
            raise AdapterError("binary changed after safety preflight")
        argv = product_command(binary, manifest, project, family)
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
    canonical_output = json.dumps(
        product_output, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
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


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="operation", required=True)
    fts = subparsers.add_parser("fts")
    fts.add_argument("--binary", required=True)
    fts.add_argument("--fixture", required=True)
    fts.add_argument("--sandbox", required=True)
    fts.add_argument("--family", required=True, choices=("graph", "session"))
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = run_fts(
            _absolute(args.binary, "binary"),
            _absolute(args.fixture, "fixture"),
            _absolute(args.sandbox, "sandbox"),
            args.family,
        )
    except AdapterError as exc:
        print(f"product adapter refused: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
