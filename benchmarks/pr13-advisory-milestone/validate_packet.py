#!/usr/bin/env python3
"""Lint the advisory packet's stable schema and repository references."""

from __future__ import annotations

import argparse
import json
import sys
from concurrent.futures import ThreadPoolExecutor, TimeoutError
from pathlib import Path
from typing import Any, Callable, NoReturn, cast

TIMEOUT_SECONDS = 5


def fail(message: str) -> NoReturn:
    raise SystemExit(f"invalid PR13 advisory packet: {message}")


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path}: {error}")
    if not isinstance(value, dict):
        fail(f"{path} must contain an object")
    return cast(dict[str, Any], value)


def repository_file(repository: Path, value: Any, name: str) -> Path:
    if not isinstance(value, str) or not value:
        fail(f"{name} must be a repository-relative path")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        fail(f"{name} escapes the repository")
    path = repository / relative
    if not path.is_file():
        fail(f"{name} is missing: {value}")
    return path


def check_packet_json(packet: dict[str, Any], repository: Path) -> None:
    repository_file(repository, packet.get("schema"), "Draft-07 schema")


def check_references(packet: dict[str, Any], repository: Path) -> None:
    contract = packet.get("behavioral_contract")
    if not isinstance(contract, dict):
        fail("behavioral_contract must be an object")
    repository_file(repository, contract.get("test"), "behavioral acceptance test")
    repository_file(repository, contract.get("runtime_test"), "runtime decoder acceptance test")
    repository_file(repository, packet.get("host_packet"), "host acceptance packet")


STATIC_GATES: dict[str, Callable[[dict[str, Any], Path], None]] = {
    "advisory_packet_json": check_packet_json,
    "advisory_references": check_references,
}


def resolve_static_gates(value: Any) -> list[Callable[[dict[str, Any], Path], None]]:
    if not isinstance(value, list) or not value:
        fail("static_gate_ids must be a non-empty array")
    resolved = []
    for gate_id in value:
        if not isinstance(gate_id, str) or gate_id not in STATIC_GATES:
            fail(f"unknown static gate id: {gate_id!r}")
        resolved.append(STATIC_GATES[gate_id])
    return resolved


def run_static_gates(packet: dict[str, Any], repository: Path) -> None:
    gates = resolve_static_gates(packet.get("static_gate_ids"))
    executor = ThreadPoolExecutor(max_workers=len(gates))
    futures = [executor.submit(gate, packet, repository) for gate in gates]
    try:
        for future in futures:
            future.result(timeout=TIMEOUT_SECONDS)
    except TimeoutError:
        for future in futures:
            future.cancel()
        executor.shutdown(wait=False, cancel_futures=True)
        fail("static gate timed out")
    executor.shutdown(wait=True)


def assert_unknown_gate_rejected() -> None:
    try:
        resolve_static_gates(["unknown_gate"])
    except SystemExit:
        return
    fail("unknown static gate self-test unexpectedly passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.parse_args()
    directory = Path(__file__).resolve().parent
    repository = directory.parents[1]
    packet = load_object(directory / "workload-v1.json")
    assert_unknown_gate_rejected()
    run_static_gates(packet, repository)
    gaps = packet.get("provider_gaps", [])
    status = packet.get("ci_gate_status", {})
    awaiting = sorted(
        gate_id
        for gate_id, state in cast(dict[str, Any], status).items()
        if state == "awaiting_ci"
    )
    print(
        f"valid PR13 advisory packet lint; provider_gaps={len(gaps)} "
        f"awaiting_ci={len(awaiting)}"
    )
    if gaps:
        print("provider gaps: " + ", ".join(str(gap) for gap in gaps))
    if awaiting:
        print("awaiting_ci: " + ", ".join(awaiting))
    return 0


if __name__ == "__main__":
    sys.exit(main())
