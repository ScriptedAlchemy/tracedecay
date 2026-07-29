#!/usr/bin/env python3
"""Lint the host packet's stable schema and repository references."""

from __future__ import annotations

import argparse
import json
import sys
from concurrent.futures import ThreadPoolExecutor, TimeoutError
from pathlib import Path
from typing import Any, Callable, NoReturn, cast

TIMEOUT_SECONDS = 5


def fail(message: str) -> NoReturn:
    raise SystemExit(f"invalid PR13 host-conformance packet: {message}")


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
    platforms = packet.get("platform_contracts")
    if not isinstance(platforms, dict):
        fail("platform_contracts must be an object")
    for platform in ("linux", "windows", "macos"):
        if platforms.get(platform) != "ci_matrix_required":
            fail(f"platform_contracts.{platform} must be ci_matrix_required")


def check_fixture_references(packet: dict[str, Any], repository: Path) -> None:
    repository_file(
        repository,
        "tests/pr13_daemon_runtime_acceptance.rs",
        "mandatory PR13 daemon runtime target",
    )
    hosts = packet.get("hosts")
    if not isinstance(hosts, list):
        fail("hosts must be an array")
    for lane in hosts:
        if not isinstance(lane, dict):
            fail("host lane must be an object")
        for event_name in ("edit", "stop"):
            event = lane.get(event_name)
            if isinstance(event, dict) and event.get("state") == "evidenced":
                repository_file(repository, event.get("capture"), "host capture")
                repository_file(repository, event.get("provenance"), "host provenance")
    installs = packet.get("install_contracts")
    if not isinstance(installs, dict):
        fail("install_contracts must be an object")
    for key in ("claude_packages", "cursor_packages"):
        for path in cast(list[Any], installs.get(key, [])):
            repository_file(repository, path, key)
    cursor_native = installs.get("cursor_native_extension")
    if not isinstance(cursor_native, dict):
        fail("Cursor native extension contract must be an object")
    repository_file(repository, cursor_native.get("package"), "Cursor extension package")
    repository_file(
        repository,
        cursor_native.get("built_javascript"),
        "Cursor extension built JavaScript",
    )
    opencode = installs.get("opencode")
    if not isinstance(opencode, dict):
        fail("OpenCode install contract must be an object")
    repository_file(repository, opencode.get("plugin_capture"), "OpenCode plugin capture")


STATIC_GATES: dict[str, Callable[[dict[str, Any], Path], None]] = {
    "host_packet_json": check_packet_json,
    "host_fixture_references": check_fixture_references,
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.parse_args()
    directory = Path(__file__).resolve().parent
    repository = directory.parents[1]
    packet = load_object(directory / "workload-v1.json")
    try:
        resolve_static_gates(["unknown_gate"])
    except SystemExit:
        pass
    else:
        fail("unknown static gate self-test unexpectedly passed")
    run_static_gates(packet, repository)
    gaps = cast(list[Any], packet.get("red_gaps", []))
    status = packet.get("ci_gate_status", {})
    awaiting = sorted(
        gate_id
        for gate_id, state in cast(dict[str, Any], status).items()
        if state == "awaiting_ci"
    )
    print(
        f"valid PR13 host packet lint; host_gaps={len(gaps)} "
        f"awaiting_ci={len(awaiting)}"
    )
    if gaps:
        print("unavailable host captures: " + ", ".join(str(gap) for gap in gaps))
    if awaiting:
        print("awaiting_ci: " + ", ".join(awaiting))
    return 0


if __name__ == "__main__":
    sys.exit(main())
