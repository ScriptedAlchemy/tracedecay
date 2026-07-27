#!/usr/bin/env python3
"""Lint the PR13 advisory packet; strict mode consumes named CI/test evidence."""

from __future__ import annotations

import argparse
import json
import re
import shlex
import sys
from concurrent.futures import ThreadPoolExecutor, TimeoutError
from pathlib import Path
from typing import Any, Callable, NoReturn, cast

BENCHMARKS_DIR = Path(__file__).resolve().parents[1]
if str(BENCHMARKS_DIR) not in sys.path:
    sys.path.insert(0, str(BENCHMARKS_DIR))

from pr12_pr13_gate_evidence import (  # noqa: E402
    EVIDENCE_PASSED,
    PLATFORM_GATE_OS,
    command_is_feature_scoped,
    evaluate_gate,
    load_junit_passed_names,
    require_ci_gate_status_shape,
    run_gate_command,
)


TIMEOUT_SECONDS = 5
KNOWN_CI_GATES = {
    "pr13_advisory_compile",
    "pr13_advisory_schema",
    "pr13_advisory_runtime_decoders",
    "pr13_advisory_pagination_cas",
    "pr13_advisory_proximity_pillar",
    "pr13_advisory_structure",
    "pr13_advisory_no_secret",
}
PARENT_GATE_COMMANDS = {
    "pr13_advisory_compile": "cargo test --all-features --test pr13_advisory_runtime_acceptance --no-run",
    "pr13_advisory_schema": "cargo test --all-features --test pr13_host_bundle_acceptance draft07_schemas_validate_contract_packets -- --exact",
    "pr13_advisory_runtime_decoders": "cargo test --all-features --test pr13_advisory_runtime_acceptance authentic_github_and_ci_responses_use_production_decoders -- --exact",
    "pr13_advisory_pagination_cas": "cargo test --all-features --lib github_nested_pagination_and_cas_are_owner_bound -- --exact",
    # This gate attests that the proximity pillar participates in the production
    # advisory cycle, NOT that overlap detection and tiering produce the right
    # answer. The end-to-end assertion (SameFile warning class, overlap_size,
    # ProximityTierV1::Immediate) lived in `proximity_file_overlap_and_tiering`,
    # which 9e3ca9fd2 deleted after 992934e03 narrowed
    # `production_proximity_evidence_authority_v1` to `pub(crate)` and broke the
    # test's import. Nothing replaced those assertions; see README.md.
    "pr13_advisory_proximity_pillar": "cargo test --all-features --test pr13_advisory_runtime_acceptance production_host_ingest_uses_registered_project_runtime -- --exact",
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
    require_ci_gate_status_shape(packet, cast(list[str], ci_gates), fail=fail)
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


def strict_acceptance(
    packet: dict[str, Any],
    repository: Path,
    *,
    junit_paths: list[Path],
    run_gates: bool,
) -> None:
    gaps = packet.get("provider_gaps")
    if not isinstance(gaps, list):
        fail("provider_gaps must be an array")
    if gaps:
        fail(
            "strict provider gaps must be empty once product journeys exist; "
            "remaining work is CI evidence, not packet gaps: "
            + ", ".join(str(gap) for gap in gaps)
        )

    ci_gates = cast(list[str], packet["ci_gate_ids"])
    checked_in = require_ci_gate_status_shape(packet, ci_gates, fail=fail)
    # CI hands this packet an untagged junit path because no advisory gate is
    # bound to a runner OS or to a reduced feature set. Prove that before using
    # the untagged bucket, so a later OS-bound or --no-default-features gate
    # cannot silently borrow all-features Linux evidence.
    for gate_id in ci_gates:
        if gate_id in PLATFORM_GATE_OS:
            fail(f"{gate_id} is OS-bound and cannot accept untagged junit evidence")
        if command_is_feature_scoped(PARENT_GATE_COMMANDS[gate_id]):
            fail(f"{gate_id} is feature-scoped and cannot accept all-features junit")
    junit_passed: set[str] = set()
    for path in junit_paths:
        junit_passed.update(load_junit_passed_names(path))
    junit_by_os = {"untagged": junit_passed}
    executed_passed: set[str] = set()
    if run_gates:
        for gate_id in ci_gates:
            if run_gate_command(repository, PARENT_GATE_COMMANDS[gate_id]):
                executed_passed.add(gate_id)
            else:
                fail(f"strict local gate command failed: {gate_id}")

    unresolved: list[str] = []
    for gate_id in ci_gates:
        # Compile-only gate: accept when the runtime suite has any passing
        # evidence or when --run-gates compiled it.
        if gate_id in COMPILE_ONLY_GATES and (
            gate_id in executed_passed
            or any("pr13_advisory_runtime_acceptance" in name for name in junit_passed)
        ):
            continue
        state = evaluate_gate(
            gate_id=gate_id,
            command=PARENT_GATE_COMMANDS[gate_id],
            checked_in_state=checked_in[gate_id],
            junit_by_os=junit_by_os,
            npm_markers=set(),
            executed_passed=executed_passed,
            executed_passed_os={},
        )
        if state != EVIDENCE_PASSED:
            unresolved.append(f"{gate_id}={state}")
    if unresolved:
        fail(
            "strict acceptance awaiting direct CI/test evidence: "
            + ", ".join(unresolved)
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--strict",
        action="store_true",
        help="require empty provider_gaps plus direct CI/junit/command evidence",
    )
    parser.add_argument(
        "--junit",
        action="append",
        default=[],
        type=Path,
        help="Ephemeral nextest/cargo junit.xml evidence (never checked in)",
    )
    parser.add_argument(
        "--run-gates",
        action="store_true",
        help="Execute allowlisted parent gate commands and require exit 0",
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
        junit_paths = [
            path if path.is_absolute() else repository / path for path in args.junit
        ]
        for path in junit_paths:
            if not path.is_file():
                fail(f"junit evidence missing: {path}")
        strict_acceptance(
            packet,
            repository,
            junit_paths=junit_paths,
            run_gates=args.run_gates,
        )
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
    if awaiting and not args.strict:
        print("awaiting_ci: " + ", ".join(awaiting))
    if args.list_parent_gates:
        for gate_id in sorted(PARENT_GATE_COMMANDS):
            print(f"{gate_id}: {PARENT_GATE_COMMANDS[gate_id]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
