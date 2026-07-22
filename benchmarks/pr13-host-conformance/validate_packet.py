#!/usr/bin/env python3
"""Lint the PR13 host packet; strict mode consumes named CI/test evidence."""

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
    KNOWN_RUNNER_OS,
    PLATFORM_GATE_OS,
    evaluate_gate,
    gates_required_for_mode,
    load_junit_passed_by_os,
    parse_gate_passed_spec,
    require_ci_gate_status_shape,
    run_gate_command,
)


TIMEOUT_SECONDS = 5
PARENT_GATE_COMMANDS = {
    "pr11_project_open_runtime": "cargo test --all-features --test pr11_pr12_runtime_acceptance project_open_application_boundary -- --exact",
    "pr12_git_cli_runtime": "cargo test --all-features --test api_application_parity cli_mcp_and_http_dispatch_the_same_callable_contracts -- --exact",
    "pr12_git_mcp_runtime": "cargo test --all-features --test api_application_parity cli_mcp_and_http_dispatch_the_same_callable_contracts -- --exact",
    "pr12_git_http_runtime": "cargo test --all-features --test api_application_parity cli_mcp_and_http_dispatch_the_same_callable_contracts -- --exact",
    "pr12_http_sse_stream": "cargo test --all-features --test api_application_parity sse_projects_the_same_canonical_feedback_payload -- --exact",
    "pr12_lsp_gateway_runtime": "cargo test --test hooks_lsp_suite lsp_gateway_protocol -- --nocapture",
    "pr12_feedback_handle_bootstrap": "cargo test --all-features --test pr11_pr12_runtime_acceptance feedback_handle_bootstrap_reads -- --exact",
    "pr12_primitives_config_parity": "cargo test --all-features --test pr11_pr12_runtime_acceptance primitive_config_markdown_json_parity -- --exact",
    "pr12_cancellation_capacity_resume": "cargo test --all-features --test pr11_pr12_runtime_acceptance cancellation_capacity_resume -- --exact",
    "pr13_host_schema": "cargo test --all-features --test pr13_host_bundle_acceptance draft07_schemas_validate_contract_packets -- --exact",
    "pr13_host_decoders": "cargo test --all-features --test pr13_host_bundle_acceptance authentic_host_fixtures_use_production_typed_decoders -- --exact",
    "pr13_daemon_runtime_pipeline": "cargo test --all-features --test pr13_daemon_runtime_acceptance authentic_callback_to_all_delivery_surfaces -- --exact",
    "pr13_host_receipt_doctor": "cargo test --all-features --test pr13_host_bundle_acceptance receipt_backed_doctor_checks_deployed_digests_registration_and_repair -- --exact",
    "pr13_host_structure": "cargo test --all-features --test pr13_host_bundle_acceptance structural_checks_ignore_commented_out_symbols -- --exact",
    "pr13_host_no_secret": "cargo test --all-features --test pr13_host_bundle_acceptance packets_pass_shared_minimal_no_secret_kernel -- --exact",
    "pr13_lite_grammar_contract": "cargo test --no-default-features --features lite --test pr13_host_bundle_acceptance structural_checks_ignore_commented_out_symbols -- --exact",
    "cursor_native_extension_check": "npm --prefix plugin/cursor-native-extension run check",
    "cursor_native_extension_test": "npm --prefix plugin/cursor-native-extension test",
    "cursor_native_extension_package": "npm --prefix plugin/cursor-native-extension run package",
    "cursor_native_extension_receipt": "cargo test --all-features --test pr13_host_bundle_acceptance cursor_native_extension_receipt_matches_embedded_assets -- --exact",
    "cursor_native_extension_runtime": "cargo test --all-features --lib cursor_native_diagnostics_are_merged_but_not_republished",
    # Default-feature product lifecycle only — never satisfied by another OS.
    "platform_linux_lifecycle": "cargo test --test pr13_host_bundle_acceptance receipt_backed_doctor_checks_deployed_digests_registration_and_repair -- --exact",
    "platform_windows_lifecycle": "cargo test --test pr13_host_bundle_acceptance receipt_backed_doctor_checks_deployed_digests_registration_and_repair -- --exact",
    "platform_macos_lifecycle": "cargo test --test pr13_host_bundle_acceptance receipt_backed_doctor_checks_deployed_digests_registration_and_repair -- --exact",
}
KNOWN_CI_GATES = set(PARENT_GATE_COMMANDS)
# Host-capture gaps may remain only while hosts[].edit/stop is unavailable.
UNAVAILABLE_HOST_GAPS = {
    "codex.edit",
    "cursor_desktop.stop",
    "kiro.edit",
    "kiro.stop",
    "cursor_cloud.edit",
    "cursor_cloud.stop",
    "cline_family.edit",
    "cline_family.stop",
}


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


def check_rust_parent_gate(repository: Path, gate_id: str, argv: list[str]) -> None:
    if "--test" in argv:
        target_index = argv.index("--test") + 1
        if target_index >= len(argv):
            fail(f"{gate_id} is missing its integration test target")
        sources = integration_test_sources(repository, argv[target_index], gate_id)
    elif "--lib" in argv:
        sources = sorted((repository / "src").rglob("*.rs"))
    else:
        fail(f"{gate_id} must select an explicit --test or --lib runtime target")
    test_filter = command_test_filter(argv, gate_id)
    if test_filter is None:
        fail(f"{gate_id} must select a non-empty runtime test filter")
    function = re.compile(
        rf"(?m)^\s*#\[(?:tokio::)?test(?:\([^]]*\))?\]\s*"
        rf"(?:async\s+)?fn\s+{re.escape(test_filter)}\s*\("
    )
    module = re.compile(rf"(?m)^\s*mod\s+{re.escape(test_filter)}(?:_test)?\s*;")
    try:
        source_texts = [path.read_text(encoding="utf-8") for path in sources]
    except OSError as error:
        fail(f"cannot inspect {gate_id} target: {error}")
    function_match = any(function.search(source) for source in source_texts)
    module_match = any(module.search(source) for source in source_texts) and any(
        "#[test]" in source or "#[tokio::test" in source for source in source_texts
    )
    if not function_match and not module_match:
        fail(f"{gate_id} filter {test_filter!r} matches no test")


def check_parent_gate_commands(repository: Path) -> None:
    package_path = repository / "plugin" / "cursor-native-extension" / "package.json"
    package = load_object(package_path)
    scripts = package.get("scripts")
    if not isinstance(scripts, dict):
        fail("Cursor extension package scripts must be an object")
    for gate_id, command in PARENT_GATE_COMMANDS.items():
        argv = shlex.split(command)
        if argv[:2] == ["cargo", "test"]:
            check_rust_parent_gate(repository, gate_id, argv)
        elif argv and argv[0] == "npm":
            script = argv[argv.index("run") + 1] if "run" in argv else "test"
            if script not in scripts:
                fail(f"{gate_id} references missing npm script {script!r}")
        else:
            fail(f"{gate_id} must be a cargo test or npm command")


def check_packet_json(packet: dict[str, Any], repository: Path) -> None:
    repository_file(repository, packet.get("schema"), "Draft-07 schema")
    if packet.get("ci_mode") != "strict":
        fail("milestone CI mode must be strict")
    ci_gates = packet.get("ci_gate_ids")
    if not isinstance(ci_gates, list) or set(ci_gates) != KNOWN_CI_GATES:
        fail("ci_gate_ids must exactly match the parent gate allowlist")
    require_ci_gate_status_shape(packet, cast(list[str], ci_gates), fail=fail)
    platforms = packet.get("platform_contracts")
    if not isinstance(platforms, dict):
        fail("platform_contracts must be an object")
    for platform in ("linux", "windows", "macos"):
        if platforms.get(platform) != "ci_matrix_required":
            fail(f"platform_contracts.{platform} must be ci_matrix_required")
    check_parent_gate_commands(repository)


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


def unavailable_host_gap_set(packet: dict[str, Any]) -> set[str]:
    allowed: set[str] = set()
    for lane in cast(list[dict[str, Any]], packet.get("hosts", [])):
        host_id = lane.get("id")
        if not isinstance(host_id, str):
            continue
        for event_name in ("edit", "stop"):
            event = lane.get(event_name)
            if isinstance(event, dict) and event.get("state") == "unavailable":
                gap = f"{host_id}.{event_name}"
                if gap in UNAVAILABLE_HOST_GAPS:
                    allowed.add(gap)
    return allowed


def strict_acceptance(
    packet: dict[str, Any],
    repository: Path,
    *,
    junit_specs: list[str],
    npm_markers: set[str],
    run_gates: bool,
    gate_passed_specs: list[str],
    runner_os: str | None,
) -> None:
    gaps = packet.get("red_gaps")
    if not isinstance(gaps, list):
        fail("red_gaps must be an array")
    allowed = unavailable_host_gap_set(packet)
    unexpected = [str(gap) for gap in gaps if gap not in allowed]
    if unexpected:
        fail(
            "strict host gaps must only list unavailable host captures already "
            f"marked unavailable in hosts[]: {', '.join(unexpected)}"
        )
    for gap in gaps:
        if gap not in UNAVAILABLE_HOST_GAPS:
            fail(f"unsupported strict host gap {gap!r}")

    ci_gates = cast(list[str], packet["ci_gate_ids"])
    checked_in = require_ci_gate_status_shape(packet, ci_gates, fail=fail)
    try:
        junit_by_os = load_junit_passed_by_os(junit_specs, repository=repository)
    except (ValueError, FileNotFoundError, OSError) as error:
        fail(str(error))

    executed_passed: set[str] = set()
    executed_passed_os: dict[str, str] = {}
    for spec in gate_passed_specs:
        try:
            os_name, gate_id = parse_gate_passed_spec(spec)
        except ValueError as error:
            fail(str(error))
        if gate_id not in KNOWN_CI_GATES:
            fail(f"unknown --gate-passed id: {gate_id}")
        required_os = PLATFORM_GATE_OS.get(gate_id)
        if required_os is not None:
            if os_name is None:
                fail(
                    f"platform gate {gate_id} requires OS-tagged evidence "
                    f"({required_os}:{gate_id})"
                )
            if os_name != required_os:
                fail(
                    f"platform gate {gate_id} cannot accept {os_name} evidence "
                    f"(requires {required_os})"
                )
            executed_passed_os[gate_id] = os_name
        elif os_name is not None and runner_os is not None and os_name != runner_os:
            fail(
                f"non-platform gate {gate_id} evidence OS {os_name} does not match "
                f"--runner-os {runner_os}"
            )
        executed_passed.add(gate_id)

    if run_gates:
        required = gates_required_for_mode(ci_gates, runner_os)
        for gate_id in required:
            if not run_gate_command(repository, PARENT_GATE_COMMANDS[gate_id]):
                fail(f"strict local gate command failed: {gate_id}")
            executed_passed.add(gate_id)
            required_os = PLATFORM_GATE_OS.get(gate_id)
            if required_os is not None:
                if runner_os is None:
                    fail(
                        f"--run-gates for platform gate {gate_id} requires --runner-os "
                        f"{required_os}"
                    )
                if runner_os != required_os:
                    fail(
                        f"--run-gates on {runner_os} cannot prove platform gate {gate_id}"
                    )
                executed_passed_os[gate_id] = required_os

    try:
        required_gates = gates_required_for_mode(ci_gates, runner_os)
    except ValueError as error:
        fail(str(error))

    unresolved: list[str] = []
    for gate_id in required_gates:
        state = evaluate_gate(
            gate_id=gate_id,
            command=PARENT_GATE_COMMANDS[gate_id],
            checked_in_state=checked_in[gate_id],
            junit_by_os=junit_by_os,
            npm_markers=npm_markers,
            executed_passed=executed_passed,
            executed_passed_os=executed_passed_os,
        )
        if state != EVIDENCE_PASSED:
            unresolved.append(f"{gate_id}={state}")
    if unresolved:
        mode = f"runner-os={runner_os}" if runner_os else "aggregate"
        fail(
            f"strict acceptance awaiting direct CI/test evidence ({mode}): "
            + ", ".join(unresolved)
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--strict", action="store_true")
    parser.add_argument(
        "--junit",
        action="append",
        default=[],
        help="Ephemeral OS-tagged junit evidence: linux|windows|macos=/path/junit.xml",
    )
    parser.add_argument(
        "--runner-os",
        choices=sorted(KNOWN_RUNNER_OS),
        help=(
            "Scope strict mode to one CI runner: shared gates + that OS platform gate. "
            "Omit for full Linux+Windows+macOS aggregation."
        ),
    )
    parser.add_argument(
        "--npm-passed",
        action="append",
        default=[],
        help="Ephemeral npm script names that already passed (check|test|package)",
    )
    parser.add_argument(
        "--run-gates",
        action="store_true",
        help="Execute allowlisted parent gate commands and require exit 0",
    )
    parser.add_argument(
        "--gate-passed",
        action="append",
        default=[],
        help=(
            "Ephemeral gate proof. Platform gates must be OS-tagged "
            "(linux:platform_linux_lifecycle)."
        ),
    )
    parser.add_argument("--list-parent-gates", action="store_true")
    args = parser.parse_args()
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
    if args.strict:
        strict_acceptance(
            packet,
            repository,
            junit_specs=list(args.junit),
            npm_markers=set(args.npm_passed),
            run_gates=args.run_gates,
            gate_passed_specs=list(args.gate_passed),
            runner_os=args.runner_os,
        )
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
    if awaiting and not args.strict:
        print("awaiting_ci: " + ", ".join(awaiting))
    if args.list_parent_gates:
        for gate_id in sorted(PARENT_GATE_COMMANDS):
            print(f"{gate_id}: {PARENT_GATE_COMMANDS[gate_id]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
