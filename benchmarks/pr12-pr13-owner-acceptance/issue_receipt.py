#!/usr/bin/env python3
"""Issue content-addressed owner acceptance receipts for PR12/PR13 gates.

Receipts use canonical SHA-256 digests plus exact commit/workload/toolchain
metadata. They never invent signatures or cryptographic trust roots.
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


def git_commit(repository: Path) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    return completed.stdout.strip()


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
    if wait_aggregate and command and command[0] == "cargo":
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
        "state": "executed_passed" if passed else "blocked",
        "command": command,
        "command_sha256": sha256_text(json.dumps(command, separators=(",", ":"))),
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


def load_manifest(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="JSON manifest of gate_id -> argv command arrays",
    )
    parser.add_argument(
        "--out",
        type=Path,
        required=True,
        help="Owner receipt JSON to write",
    )
    parser.add_argument(
        "--log-dir",
        type=Path,
        required=True,
        help="Directory for captured stdout/stderr logs",
    )
    parser.add_argument(
        "--workload",
        type=Path,
        action="append",
        default=[],
        help="Workload JSON files to digest into the receipt",
    )
    parser.add_argument(
        "--gate",
        action="append",
        default=[],
        help="Optional subset of gate ids; default runs all manifest gates in order",
    )
    parser.add_argument(
        "--pending",
        action="append",
        default=[],
        metavar="GATE=REASON",
        help="Record pending_unsupported_host without executing",
    )
    parser.add_argument(
        "--wait-aggregate",
        action="store_true",
        help="Wait for cargo-slot aggregate admission before each cargo gate",
    )
    parser.add_argument(
        "--wait-timeout",
        type=int,
        default=7200,
        help="Seconds to wait for aggregate admission",
    )
    parser.add_argument(
        "--merge-existing",
        action="store_true",
        help="Merge into an existing receipt file instead of replacing",
    )
    args = parser.parse_args()

    repository = repository_root()
    manifest = load_manifest(args.manifest)
    gates_spec = manifest.get("gates")
    if not isinstance(gates_spec, dict) or not gates_spec:
        raise SystemExit("manifest.gates must be a non-empty object")

    selected = args.gate or list(gates_spec)
    for gate_id in selected:
        if gate_id not in gates_spec:
            raise SystemExit(f"unknown gate id: {gate_id}")

    existing: dict[str, Any] = {}
    if args.merge_existing and args.out.is_file():
        existing = load_manifest(args.out)

    workload_digests = {
        str(path): sha256_file(path if path.is_absolute() else repository / path)
        for path in args.workload
    }
    receipt_root: dict[str, Any] = {
        "schema_version": 1,
        "receipt_kind": "owner_acceptance_v1",
        "authority": "owner_acceptance",
        "signature": "none_content_addressed_sha256_only",
        "commit": git_commit(repository),
        "toolchain": toolchain_metadata(),
        "workload_sha256": workload_digests,
        "executed_at": datetime.now(UTC).replace(microsecond=0).isoformat(),
        "host": {"os": "linux", "uname": toolchain_metadata()["host"]},
        "gates": dict(existing.get("gates", {})) if args.merge_existing else {},
    }

    for item in args.pending:
        if "=" not in item:
            raise SystemExit(f"--pending expects GATE=REASON, got {item!r}")
        gate_id, reason = item.split("=", 1)
        receipt_root["gates"][gate_id] = {
            "gate_id": gate_id,
            "state": "pending_unsupported_host",
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

    args.out.parent.mkdir(parents=True, exist_ok=True)
    canonical = json.dumps(receipt_root, indent=2, sort_keys=True) + "\n"
    args.out.write_text(canonical, encoding="utf-8")
    print(f"wrote {args.out} digest={sha256_text(canonical)}", flush=True)
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    # Avoid inherited TRACEDECAY_DATA_DIR / CARGO_TARGET_DIR overrides for evidence purity.
    os.environ.pop("TRACEDECAY_DATA_DIR", None)
    sys.exit(main())
