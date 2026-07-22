#!/usr/bin/env python3
"""Issue content-addressed PR7 owner acceptance receipts.

Owner evidence is canonical JSON plus SHA-256 digests of the source
commit/tree, workload/config/toolchain pins, and executed gate receipts.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


def sha256_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def sha256_text(text: str) -> str:
    return sha256_bytes(text.encode("utf-8"))


def repository_root() -> Path:
    return Path(__file__).resolve().parents[2]


def git_rev_parse(repository: Path, rev: str) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", rev],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    return completed.stdout.strip()


def worktree_dirty(repository: Path) -> bool:
    completed = subprocess.run(
        [
            "git",
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    return bool(completed.stdout.strip())


def toolchain_metadata() -> dict[str, str]:
    rustc = subprocess.run(["rustc", "-V"], check=True, capture_output=True, text=True)
    cargo = subprocess.run(["cargo", "-V"], check=True, capture_output=True, text=True)
    uname = subprocess.run(["uname", "-srm"], check=True, capture_output=True, text=True)
    return {
        "rustc": rustc.stdout.strip(),
        "cargo": cargo.stdout.strip(),
        "host": uname.stdout.strip(),
        "os": "linux",
    }


def config_digests(repository: Path) -> dict[str, str]:
    candidates = [
        repository / "Cargo.toml",
        repository / "Cargo.lock",
        repository / ".cargo" / "config.toml",
    ]
    digests: dict[str, str] = {}
    for path in candidates:
        if path.is_file():
            digests[str(path.relative_to(repository))] = sha256_file(path)
    return digests


def aggregate_occupied() -> bool:
    state = Path("/run/user/1000/cargo-slot-aggregate.state")
    return state.is_file() and bool(state.read_text(encoding="utf-8").strip())


def wait_for_aggregate(timeout_seconds: int) -> None:
    deadline = time.time() + timeout_seconds
    while aggregate_occupied():
        if time.time() >= deadline:
            raise SystemExit("aggregate Cargo admission still occupied; not overlapping")
        time.sleep(5)


def run_gate(
    *,
    repository: Path,
    gate_id: str,
    command: list[str],
    log_dir: Path,
    wait_aggregate: bool,
    wait_timeout: int,
) -> dict[str, Any]:
    uses_cargo = bool(command) and (
        command[0] == "cargo" or any("cargo " in part or part == "cargo" for part in command)
    )
    if wait_aggregate and uses_cargo:
        wait_for_aggregate(wait_timeout)
    log_dir.mkdir(parents=True, exist_ok=True)
    stdout_path = log_dir / f"{gate_id}.stdout.log"
    stderr_path = log_dir / f"{gate_id}.stderr.log"
    started = time.perf_counter_ns()
    completed = subprocess.run(
        command,
        cwd=repository,
        check=False,
        capture_output=True,
    )
    elapsed_ms = (time.perf_counter_ns() - started) // 1_000_000
    stdout_path.write_bytes(completed.stdout)
    stderr_path.write_bytes(completed.stderr)
    passed = completed.returncode == 0
    summary_line = ""
    for line in completed.stdout.decode("utf-8", errors="replace").splitlines()[::-1]:
        if "passed" in line.lower() or "test result:" in line.lower() or "DONE" in line:
            summary_line = line.strip()
            break
    receipt: dict[str, Any] = {
        "gate_id": gate_id,
        "state": "executed_passed" if passed else "failed",
        "command": command,
        "command_sha256": sha256_text(json.dumps(command, separators=(",", ":"), sort_keys=True)),
        "exit_code": completed.returncode,
        "elapsed_ms": elapsed_ms,
        "recorded_at": datetime.now(UTC).replace(microsecond=0).isoformat(),
        "stdout_sha256": sha256_file(stdout_path),
        "stderr_sha256": sha256_file(stderr_path),
        "stdout_path": str(stdout_path.relative_to(repository)),
        "stderr_path": str(stderr_path.relative_to(repository)),
    }
    if summary_line:
        receipt["summary"] = summary_line
    if not passed:
        receipt["reason"] = f"command exited {completed.returncode}"
    return receipt


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_canonical_json(path: Path, payload: dict[str, Any]) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    canonical = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    path.write_text(canonical, encoding="utf-8")
    return sha256_text(canonical)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--log-dir", type=Path, required=True)
    parser.add_argument("--evidence-index", type=Path, required=True)
    parser.add_argument(
        "--workload",
        type=Path,
        action="append",
        default=[],
        help="Workload/config JSON files to digest into the receipt",
    )
    parser.add_argument("--gate", action="append", default=[])
    parser.add_argument(
        "--pending",
        action="append",
        default=[],
        metavar="GATE=REASON",
        help="Record pending without executing",
    )
    parser.add_argument("--wait-aggregate", action="store_true")
    parser.add_argument("--wait-timeout", type=int, default=7200)
    parser.add_argument("--merge-existing", action="store_true")
    args = parser.parse_args()

    repository = repository_root()
    manifest = load_json(args.manifest if args.manifest.is_absolute() else repository / args.manifest)
    gates_spec = manifest.get("gates")
    if not isinstance(gates_spec, dict) or not gates_spec:
        raise SystemExit("manifest.gates must be a non-empty object")

    selected = args.gate or list(gates_spec)
    for gate_id in selected:
        if gate_id not in gates_spec:
            raise SystemExit(f"unknown gate id: {gate_id}")

    existing: dict[str, Any] = {}
    out_path = args.out if args.out.is_absolute() else repository / args.out
    if args.merge_existing and out_path.is_file():
        existing = load_json(out_path)

    workload_paths = [
        path if path.is_absolute() else repository / path for path in args.workload
    ]
    workload_digests = {str(path.relative_to(repository)): sha256_file(path) for path in workload_paths}

    commit = git_rev_parse(repository, "HEAD")
    tree = git_rev_parse(repository, "HEAD^{tree}")
    dirty = worktree_dirty(repository)
    clean_logical_snapshot = not dirty
    toolchain = toolchain_metadata()

    receipt_root: dict[str, Any] = {
        "schema_version": 1,
        "receipt_kind": "owner_acceptance_v1",
        "authority": "owner_acceptance",
        "evidence_contract": "canonical_json_sha256_executed_receipts_only",
        "slice": "pr7-memory-fact-provenance",
        "source_repository_commit": commit,
        "source_repository_tree": tree,
        "clean_logical_snapshot": clean_logical_snapshot,
        "worktree_dirty": dirty,
        "toolchain": toolchain,
        "toolchain_sha256": sha256_text(json.dumps(toolchain, separators=(",", ":"), sort_keys=True)),
        "workload_sha256": workload_digests,
        "config_sha256": config_digests(repository),
        "manifest_sha256": sha256_file(
            args.manifest if args.manifest.is_absolute() else repository / args.manifest
        ),
        "executed_at": datetime.now(UTC).replace(microsecond=0).isoformat(),
        "host": {"os": "linux", "uname": toolchain["host"]},
        "gates": dict(existing.get("gates", {})) if args.merge_existing else {},
        "blockers": [],
    }

    for item in args.pending:
        if "=" not in item:
            raise SystemExit(f"--pending expects GATE=REASON, got {item!r}")
        gate_id, reason = item.split("=", 1)
        receipt_root["gates"][gate_id] = {
            "gate_id": gate_id,
            "state": "pending",
            "reason": reason,
            "recorded_at": datetime.now(UTC).replace(microsecond=0).isoformat(),
        }

    log_dir = args.log_dir if args.log_dir.is_absolute() else repository / args.log_dir
    failures = 0
    for gate_id in selected:
        command = gates_spec[gate_id]
        if not isinstance(command, list) or not all(isinstance(part, str) for part in command):
            raise SystemExit(f"{gate_id} command must be a string argv array")
        print(f"running {gate_id}: {' '.join(command)}", flush=True)
        gate_receipt = run_gate(
            repository=repository,
            gate_id=gate_id,
            command=command,
            log_dir=log_dir,
            wait_aggregate=args.wait_aggregate,
            wait_timeout=args.wait_timeout,
        )
        receipt_root["gates"][gate_id] = gate_receipt
        print(
            f"{gate_id}: {gate_receipt['state']} exit={gate_receipt['exit_code']} "
            f"stdout={gate_receipt['stdout_sha256']}",
            flush=True,
        )
        if gate_receipt["state"] != "executed_passed":
            failures += 1

    blockers: list[dict[str, str]] = []
    if dirty:
        blockers.append(
            {
                "code": "dirty_worktree",
                "detail": "clean_logical_snapshot requires an empty git status porcelain set",
            }
        )
    for gate_id, gate_receipt in receipt_root["gates"].items():
        state = gate_receipt.get("state")
        if state == "executed_passed":
            continue
        blockers.append(
            {
                "code": f"gate_{state}",
                "detail": f"{gate_id}: {gate_receipt.get('reason', state)}",
            }
        )
    receipt_root["blockers"] = blockers

    all_gates_passed = failures == 0 and all(
        gate.get("state") == "executed_passed" for gate in receipt_root["gates"].values()
    )
    promote = all_gates_passed and clean_logical_snapshot
    receipt_root["current_acceptance_eligible"] = promote

    receipt_digest = write_canonical_json(out_path, receipt_root)
    print(f"wrote {out_path} digest={receipt_digest}", flush=True)

    evidence_index_path = (
        args.evidence_index
        if args.evidence_index.is_absolute()
        else repository / args.evidence_index
    )
    index = load_json(evidence_index_path) if evidence_index_path.is_file() else {
        "schema_version": 1,
        "current_acceptance": None,
        "provisional": "result-provisional.json",
        "historical_stale": [],
    }
    index["schema_version"] = 1
    index["owner_receipt"] = str(out_path.relative_to(repository))
    index["owner_receipt_sha256"] = receipt_digest
    index["source_repository_commit"] = commit
    index["source_repository_tree"] = tree
    index["clean_logical_snapshot"] = clean_logical_snapshot
    index["blockers"] = blockers
    if promote:
        index["current_acceptance"] = str(out_path.relative_to(repository))
    else:
        index["current_acceptance"] = None
    write_canonical_json(evidence_index_path, index)
    print(
        f"updated {evidence_index_path} current_acceptance={index['current_acceptance']!r} "
        f"blockers={len(blockers)}",
        flush=True,
    )
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    os.environ.pop("TRACEDECAY_DATA_DIR", None)
    sys.exit(main())
