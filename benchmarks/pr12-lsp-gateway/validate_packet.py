#!/usr/bin/env python3
"""Validate the PR12 LSP workload packet and optionally run its protocol gate."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, NoReturn, cast


REQUIRED_METRICS = {
    "wall_clock_ns",
    "cpu_user_system_ticks",
    "peak_rss_bytes",
    "queued_bytes",
    "queued_messages",
    "overlay_bytes",
    "publication_bytes",
    "superseded_publications",
    "backpressure_events",
    "reconnects",
    "expirations",
}

REQUIRED_ARCHITECTURE = {
    "session_owner",
    "protocol_actor",
    "feedback_authority",
    "diagnostic_projection",
    "semantic_navigation",
    "transport_bridge",
}

REQUIRED_SCENARIOS = {
    "diagnostics.pull": "textDocument/diagnostic",
    "diagnostics.push": "textDocument/didSave",
    "navigation.definition": "textDocument/definition",
    "navigation.references": "textDocument/references",
    "navigation.hover": "textDocument/hover",
    "feedback.same-authority": "textDocument/diagnostic",
}

EXPECTED_PROTOCOL_GATE = [
    "cargo",
    "test",
    "--test",
    "hooks_lsp_suite",
    "lsp_gateway_protocol",
    "--",
    "--nocapture",
]


def fail(message: str) -> NoReturn:
    raise SystemExit(f"invalid PR12 LSP packet: {message}")


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path}: {error}")
    if not isinstance(value, dict):
        fail(f"{path} must contain an object")
    return cast(dict[str, Any], value)


def object_field(value: dict[str, Any], name: str) -> dict[str, Any]:
    field = value.get(name)
    if not isinstance(field, dict):
        fail(f"{name} must be an object")
    return cast(dict[str, Any], field)


def string_list(value: Any, name: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) and item for item in value):
        fail(f"{name} must be a non-empty string array")
    return cast(list[str], value)


def validate_protocol_gate(command: list[str], repository: Path) -> None:
    if command != EXPECTED_PROTOCOL_GATE:
        fail(
            "protocol_gate must target registered hooks_lsp_suite "
            "filter lsp_gateway_protocol"
        )
    target = repository / "tests" / "hooks_lsp_suite" / "main.rs"
    module = repository / "tests" / "hooks_lsp_suite" / "lsp_gateway_protocol_test.rs"
    if not target.is_file():
        fail("protocol_gate target hooks_lsp_suite is not registered")
    if not module.is_file():
        fail("protocol_gate filter lsp_gateway_protocol has no source module")
    try:
        target_source = target.read_text(encoding="utf-8")
        module_source = module.read_text(encoding="utf-8")
    except OSError as error:
        fail(f"cannot inspect protocol_gate target: {error}")
    if "mod lsp_gateway_protocol_test;" not in target_source or "#[test]" not in module_source:
        fail("protocol_gate filter lsp_gateway_protocol matches no registered tests")


def validate(
    workload: dict[str, Any], baseline: dict[str, Any], repository: Path
) -> list[str]:
    if workload.get("version") != 1:
        fail("workload.version must be 1")
    workload_id = workload.get("workload_id")
    if not isinstance(workload_id, str) or not workload_id:
        fail("workload_id must be a non-empty string")
    architecture = object_field(workload, "architecture")
    if not REQUIRED_ARCHITECTURE.issubset(architecture):
        fail("architecture must name each gateway owner and projection boundary")
    if not all(
        isinstance(architecture[name], str) and "::" in architecture[name]
        for name in REQUIRED_ARCHITECTURE
    ):
        fail("architecture boundaries must be qualified source symbols")
    invariants = string_list(architecture.get("invariants"), "architecture.invariants")
    if not any("same feedback-cycle authority" in invariant for invariant in invariants):
        fail("architecture must preserve one feedback-cycle authority")
    scenarios = workload.get("acceptance_scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        fail("acceptance_scenarios must be a non-empty array")
    observed_scenarios: dict[str, str] = {}
    for scenario in scenarios:
        if not isinstance(scenario, dict):
            fail("each acceptance scenario must be an object")
        scenario_id = scenario.get("id")
        method = scenario.get("method")
        path = scenario.get("path")
        if (
            not isinstance(scenario_id, str)
            or not isinstance(method, str)
            or not isinstance(path, str)
            or "->" not in path
        ):
            fail("each acceptance scenario needs id, method, and callable path")
        observed_scenarios[scenario_id] = method
    if observed_scenarios != REQUIRED_SCENARIOS:
        fail("acceptance_scenarios must exactly cover diagnostics, navigation, and feedback")
    request_mix = object_field(workload, "request_mix")
    if not request_mix or not all(isinstance(value, int) and value > 0 for value in request_mix.values()):
        fail("request_mix must contain positive request counts")
    limits = object_field(workload, "limits")
    if not limits or not all(isinstance(value, int) and value > 0 for value in limits.values()):
        fail("limits must contain positive integer bounds")
    if set(string_list(workload.get("required_metrics"), "required_metrics")) != REQUIRED_METRICS:
        fail("required_metrics must exactly match the telemetry contract")
    command = string_list(workload.get("protocol_gate"), "protocol_gate")
    validate_protocol_gate(command, repository)
    if baseline.get("schema_version") != 1 or baseline.get("workload_id") != workload_id:
        fail("baseline must use schema 1 and the workload's id")
    measurement = object_field(baseline, "measurement")
    status = measurement.get("status")
    if status not in {"measured", "pending_execution"}:
        fail("baseline measurement status is invalid")
    if not isinstance(measurement.get("reason"), str) or not measurement["reason"]:
        fail("baseline measurement requires a reason")
    samples = baseline.get("samples")
    if status == "measured" and (not isinstance(samples, list) or not samples):
        fail("a measured baseline requires non-empty samples")
    if status == "measured":
        for sample in samples:
            if not isinstance(sample, dict) or not REQUIRED_METRICS.issubset(sample):
                fail("each measured sample must contain the telemetry contract")
    elif samples is not None:
        fail("a pending baseline must use samples: null")
    return command


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--protocol-gate",
        action="store_true",
        help="run the declared semantic gate without manufacturing a performance baseline",
    )
    args = parser.parse_args()
    directory = Path(__file__).resolve().parent
    workload = load_object(directory / "workload-v1.json")
    baseline = load_object(directory / "baseline-v1.json")
    command = validate(workload, baseline, directory.parents[1])
    print(f"valid PR12 LSP workload packet; baseline status={baseline['measurement']['status']}")
    if not args.protocol_gate:
        return 0
    return subprocess.run(command, cwd=directory.parents[1], check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
