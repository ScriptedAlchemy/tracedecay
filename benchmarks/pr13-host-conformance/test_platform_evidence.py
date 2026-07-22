#!/usr/bin/env python3
"""Validator tests: OS-bound platform evidence cannot cross-satisfy."""

from __future__ import annotations

import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from pr12_pr13_gate_evidence import (
    EVIDENCE_PASSED,
    evaluate_gate,
    gates_required_for_mode,
    load_junit_passed_by_os,
    parse_gate_passed_spec,
)


LIFECYCLE_CMD = (
    "cargo test --test pr13_host_bundle_acceptance "
    "receipt_backed_doctor_checks_deployed_digests_registration_and_repair -- --exact"
)


def write_junit(directory: Path, name: str) -> Path:
    path = directory / f"{name}.xml"
    path.write_text(
        textwrap.dedent(
            f"""\
            <?xml version="1.0" encoding="UTF-8"?>
            <testsuites>
              <testsuite name="{name}">
                <testcase classname="pr13_host_bundle_acceptance"
                  name="receipt_backed_doctor_checks_deployed_digests_registration_and_repair"
                  time="0.1"/>
                <testcase classname="pr11_pr12_runtime_acceptance"
                  name="project_open_application_boundary" time="0.1"/>
              </testsuite>
            </testsuites>
            """
        ),
        encoding="utf-8",
    )
    return path


class PlatformEvidenceTests(unittest.TestCase):
    def test_linux_junit_does_not_pass_any_platform_lifecycle_gate(self) -> None:
        """JUnit name matches alone never close OS-bound platform gates."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            linux_junit = write_junit(root, "linux")
            by_os = load_junit_passed_by_os(
                [f"linux={linux_junit}"], repository=root
            )
            for gate_id in (
                "platform_linux_lifecycle",
                "platform_windows_lifecycle",
                "platform_macos_lifecycle",
            ):
                state = evaluate_gate(
                    gate_id=gate_id,
                    command=LIFECYCLE_CMD,
                    checked_in_state="awaiting_ci",
                    junit_by_os=by_os,
                    npm_markers=set(),
                    executed_passed=set(),
                    executed_passed_os={},
                )
                self.assertEqual(state, "awaiting_ci", gate_id)

    def test_os_tagged_gate_passed_only_matches_its_platform(self) -> None:
        state = evaluate_gate(
            gate_id="platform_linux_lifecycle",
            command=LIFECYCLE_CMD,
            checked_in_state="awaiting_ci",
            junit_by_os={"linux": set(), "windows": set(), "macos": set()},
            npm_markers=set(),
            executed_passed={"platform_linux_lifecycle"},
            executed_passed_os={"platform_linux_lifecycle": "linux"},
        )
        self.assertEqual(state, EVIDENCE_PASSED)

        mismatched = evaluate_gate(
            gate_id="platform_linux_lifecycle",
            command=LIFECYCLE_CMD,
            checked_in_state="awaiting_ci",
            junit_by_os={"linux": set(), "windows": set(), "macos": set()},
            npm_markers=set(),
            executed_passed={"platform_linux_lifecycle"},
            executed_passed_os={"platform_linux_lifecycle": "windows"},
        )
        self.assertEqual(mismatched, "awaiting_ci")

    def test_aggregation_requires_all_platform_gates(self) -> None:
        gates = [
            "pr11_project_open_runtime",
            "platform_linux_lifecycle",
            "platform_windows_lifecycle",
            "platform_macos_lifecycle",
        ]
        self.assertEqual(gates_required_for_mode(gates, None), gates)
        self.assertEqual(
            gates_required_for_mode(gates, "linux"),
            ["pr11_project_open_runtime", "platform_linux_lifecycle"],
        )
        self.assertEqual(
            gates_required_for_mode(gates, "windows"),
            ["pr11_project_open_runtime", "platform_windows_lifecycle"],
        )

    def test_parse_gate_passed_requires_os_for_platform_style(self) -> None:
        os_name, gate_id = parse_gate_passed_spec("macos:platform_macos_lifecycle")
        self.assertEqual(os_name, "macos")
        self.assertEqual(gate_id, "platform_macos_lifecycle")

    def test_aggregate_with_all_os_gate_passed(self) -> None:
        executed = {
            "platform_linux_lifecycle",
            "platform_windows_lifecycle",
            "platform_macos_lifecycle",
        }
        executed_os = {
            "platform_linux_lifecycle": "linux",
            "platform_windows_lifecycle": "windows",
            "platform_macos_lifecycle": "macos",
        }
        for gate_id in executed:
            state = evaluate_gate(
                gate_id=gate_id,
                command=LIFECYCLE_CMD,
                checked_in_state="awaiting_ci",
                junit_by_os={"linux": set(), "windows": set(), "macos": set()},
                npm_markers=set(),
                executed_passed=executed,
                executed_passed_os=executed_os,
            )
            self.assertEqual(state, EVIDENCE_PASSED, gate_id)


if __name__ == "__main__":
    unittest.main()
