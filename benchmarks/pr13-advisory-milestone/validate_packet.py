#!/usr/bin/env python3
"""Lint the PR13 advisory packet; strict mode enforces milestone completeness."""

from __future__ import annotations

import argparse
import json
import re
import shlex
import sys
from concurrent.futures import ThreadPoolExecutor, TimeoutError
from pathlib import Path
from typing import Any, Callable, NoReturn, cast


TIMEOUT_SECONDS = 5
KNOWN_CI_GATES = {
    "pr13_advisory_compile",
    "pr13_advisory_schema",
    "pr13_advisory_runtime_decoders",
    "pr13_advisory_pagination_cas",
    "pr13_advisory_proximity_overlap",
    "pr13_advisory_structure",
    "pr13_advisory_no_secret",
}
PARENT_GATE_COMMANDS = {
    "pr13_advisory_compile": "cargo test --all-features --test pr13_advisory_runtime_acceptance --no-run",
    "pr13_advisory_schema": "cargo test --all-features --test pr13_host_bundle_acceptance draft07_schemas_validate_contract_packets -- --exact",
    "pr13_advisory_runtime_decoders": "cargo test --all-features --test pr13_advisory_runtime_acceptance authentic_github_and_ci_responses_use_production_decoders -- --exact",
    "pr13_advisory_pagination_cas": "cargo test --all-features --lib github_nested_pagination_and_cas_are_owner_bound -- --exact",
    "pr13_advisory_proximity_overlap": "cargo test --all-features --test pr13_advisory_runtime_acceptance proximity_file_overlap_and_tiering -- --exact",
    "pr13_advisory_structure": "cargo test --all-features --test pr13_host_bundle_acceptance structural_checks_ignore_commented_out_symbols -- --exact",
    "pr13_advisory_no_secret": "cargo test --all-features --test pr13_host_bundle_acceptance packets_pass_shared_minimal_no_secret_kernel -- --exact",
}
COMPILE_ONLY_GATES = {"pr13_advisory_compile"}


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


def integration_test_sources(repository: Path, target: str, gate_id: str) -> list[Path]:
    direct = repository / "tests" / f"{target}.rs"
    directory = repository / "tests" / target
    if direct.is_file():
        return [direct]
    main = directory / "main.rs"
    if main.is_file():
        return sorted(directory.rglob("*.rs"))
    fail(f"{gate_id} references missing integration test target {target!r}")


def command_test_filter(argv: list[str], gate_id: str) -> str | None:
    separator = argv.index("--") if "--" in argv else len(argv)
    positionals: list[str] = []
    index = 2
    value_options = {"--test", "--features", "-p", "--package"}
    while index < separator:
        argument = argv[index]
        if argument in value_options:
            index += 2
        elif argument.startswith("-"):
            index += 1
        else:
            positionals.append(argument)
            index += 1
    if len(positionals) > 1:
        fail(f"{gate_id} declares ambiguous Cargo test filters")
    return positionals[0] if positionals else None


def check_parent_gate_commands(repository: Path) -> None:
    for gate_id, command in PARENT_GATE_COMMANDS.items():
        argv = shlex.split(command)
        if argv[:2] != ["cargo", "test"]:
            fail(f"{gate_id} must be a cargo test command")
        if "--test" in argv:
            target_index = argv.index("--test") + 1
            if target_index >= len(argv):
                fail(f"{gate_id} is missing its integration test target")
            sources = integration_test_sources(repository, argv[target_index], gate_id)
        elif "--lib" in argv:
            sources = sorted((repository / "src").rglob("*.rs"))
        else:
            fail(f"{gate_id} must select a registered integration or library test target")
        test_filter = command_test_filter(argv, gate_id)
        if gate_id in COMPILE_ONLY_GATES:
            if "--no-run" not in argv:
                fail(f"{gate_id} must remain compile-only")
            continue
        if test_filter is None:
            fail(f"{gate_id} must select a non-empty runtime test filter")
        function = re.compile(
            rf"(?m)^\s*#\[(?:tokio::)?test(?:\([^]]*\))?\]\s*"
            rf"(?:async\s+)?fn\s+{re.escape(test_filter)}\s*\("
        )
        if not any(function.search(path.read_text(encoding="utf-8")) for path in sources):
            fail(f"{gate_id} filter {test_filter!r} matches no test function")


def check_packet_json(packet: dict[str, Any], repository: Path) -> None:
    repository_file(repository, packet.get("schema"), "Draft-07 schema")
    if packet.get("ci_mode") != "strict":
        fail("milestone CI mode must be strict")
    ci_gates = packet.get("ci_gate_ids")
    if not isinstance(ci_gates, list) or set(ci_gates) != KNOWN_CI_GATES:
        fail("ci_gate_ids must exactly match the declared Rust acceptance gates")
    if set(PARENT_GATE_COMMANDS) != KNOWN_CI_GATES:
        fail("parent gate command allowlist is incomplete")
    check_parent_gate_commands(repository)


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


def strict_acceptance(packet: dict[str, Any]) -> None:
    gaps = packet.get("provider_gaps")
    if not isinstance(gaps, list):
        fail("provider_gaps must be an array")
    if gaps:
        fail("strict acceptance incomplete: " + ", ".join(str(gap) for gap in gaps))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--strict",
        action="store_true",
        help="enforce milestone completeness; lint mode is the default",
    )
    parser.add_argument(
        "--list-parent-gates",
        action="store_true",
        help="print fixed parent-run CI commands without executing them",
    )
    args = parser.parse_args()
    directory = Path(__file__).resolve().parent
    repository = directory.parents[1]
    packet = load_object(directory / "workload-v1.json")
    assert_unknown_gate_rejected()
    run_static_gates(packet, repository)
    if args.strict:
        strict_acceptance(packet)
    gaps = packet.get("provider_gaps", [])
    print(f"valid PR13 advisory packet lint; strict gaps={len(gaps)}")
    if gaps:
        print("unavailable: " + ", ".join(str(gap) for gap in gaps))
    if args.list_parent_gates:
        for gate_id in sorted(PARENT_GATE_COMMANDS):
            print(f"{gate_id}: {PARENT_GATE_COMMANDS[gate_id]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
