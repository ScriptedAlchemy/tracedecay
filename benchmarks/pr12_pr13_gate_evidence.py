#!/usr/bin/env python3
"""Shared PR12/PR13 gate evidence helpers.

Checked-in packets may only declare awaiting_ci or failed. A gate is treated as
passed only when ephemeral CI/local evidence (OS-tagged junit XML or an executed
command) proves it. Validators never invent passed status into packet files.

Platform lifecycle gates are bound to runner OS identity: Linux junit cannot
satisfy Windows/macOS, and aggregation requires each OS artifact separately.

Feature-scoped gates are bound the same way to a cargo feature configuration.
The junit evidence comes from one `--workspace --all-features` run, so it cannot
witness a reduced build, and a test filtered by name is indistinguishable
between the two: `structural_checks_ignore_commented_out_symbols` is one #[test]
that both pr13_host_structure (--all-features) and pr13_lite_grammar_contract
(--no-default-features --features lite) run. Renaming cannot separate them.
"""

from __future__ import annotations

import re
import shlex
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any


CHECKED_IN_GATE_STATES = frozenset({"awaiting_ci", "failed"})
EVIDENCE_PASSED = "passed"
KNOWN_RUNNER_OS = frozenset({"linux", "windows", "macos"})
PLATFORM_GATE_OS = {
    "platform_linux_lifecycle": "linux",
    "platform_windows_lifecycle": "windows",
    "platform_macos_lifecycle": "macos",
}


def command_is_feature_scoped(command: str) -> bool:
    """True when a gate command proves a REDUCED cargo feature configuration.

    The junit evidence is produced by one `--workspace --all-features` run, so it
    can only witness gates that also run with all features. A command that opts
    out of default features is proving that the smaller build compiles and
    passes, which the all-features run says nothing about, so such a gate must
    never be closed by a junit name match.
    """
    return "--no-default-features" in shlex.split(command)


def cargo_filter_from_command(command: str) -> str | None:
    argv = shlex.split(command)
    if not argv or argv[0] != "cargo":
        return None
    separator = argv.index("--") if "--" in argv else len(argv)
    positionals: list[str] = []
    index = 2
    value_options = {"--test", "--features", "-p", "--package"}
    while index < separator:
        argument = argv[index]
        if argument in value_options:
            index += 2
            continue
        if argument.startswith("-"):
            index += 1
            continue
        positionals.append(argument)
        index += 1
    if len(positionals) != 1:
        return None
    return positionals[0]


def load_junit_passed_names(path: Path) -> set[str]:
    passed: set[str] = set()
    tree = ET.parse(path)
    root = tree.getroot()
    suites = [root] if root.tag.endswith("testsuite") else []
    suites.extend(root.iter("testsuite"))
    for suite in suites:
        for case in suite.findall("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                continue
            if case.find("skipped") is not None:
                continue
            name = case.attrib.get("name") or ""
            classname = case.attrib.get("classname") or ""
            if name:
                passed.add(name)
                if "::" in name:
                    passed.add(name.rsplit("::", 1)[-1])
            if classname and name:
                passed.add(f"{classname}::{name}")
                passed.add(f"{classname.rsplit('.', 1)[-1]}::{name}")
    return passed


def parse_junit_spec(spec: str, *, repository: Path) -> tuple[str, Path]:
    if "=" not in spec:
        raise ValueError(
            "junit evidence must be OS=path (linux|windows|macos=/path/to/junit.xml)"
        )
    os_name, raw_path = spec.split("=", 1)
    os_name = os_name.strip().lower()
    if os_name not in KNOWN_RUNNER_OS:
        raise ValueError(f"unknown junit runner OS {os_name!r}")
    path = Path(raw_path.strip())
    if not path.is_absolute():
        path = repository / path
    return os_name, path


def load_junit_passed_by_os(
    specs: list[str], *, repository: Path
) -> dict[str, set[str]]:
    by_os: dict[str, set[str]] = {os_name: set() for os_name in KNOWN_RUNNER_OS}
    for spec in specs:
        os_name, path = parse_junit_spec(spec, repository=repository)
        if not path.is_file():
            raise FileNotFoundError(f"junit evidence missing: {path}")
        by_os[os_name].update(load_junit_passed_names(path))
    return by_os


def parse_gate_passed_spec(spec: str) -> tuple[str | None, str]:
    """Return (optional_os, gate_id). Platform gates require OS:gate_id."""
    if ":" in spec:
        os_name, gate_id = spec.split(":", 1)
        os_name = os_name.strip().lower()
        gate_id = gate_id.strip()
        if os_name not in KNOWN_RUNNER_OS:
            raise ValueError(f"unknown gate-passed runner OS {os_name!r}")
        if not gate_id:
            raise ValueError("gate-passed gate id is empty")
        return os_name, gate_id
    return None, spec.strip()


def npm_gate_passed(command: str, npm_markers: set[str]) -> bool:
    argv = shlex.split(command)
    if not argv or argv[0] != "npm":
        return False
    script = "test"
    if "run" in argv:
        script = argv[argv.index("run") + 1]
    return script in npm_markers or command in npm_markers


def filter_matches_junit(filter_name: str, junit_passed: set[str]) -> bool:
    if filter_name in junit_passed:
        return True
    for passed in junit_passed:
        if passed == filter_name or passed.endswith(f"::{filter_name}"):
            return True
        if re.search(rf"(^|::){re.escape(filter_name)}($|::)", passed):
            return True
    return False


def evaluate_gate(
    *,
    gate_id: str,
    command: str,
    checked_in_state: str,
    junit_by_os: dict[str, set[str]],
    npm_markers: set[str],
    executed_passed: set[str],
    executed_passed_os: dict[str, str],
) -> str:
    required_os = PLATFORM_GATE_OS.get(gate_id)

    if gate_id in executed_passed:
        if required_os is not None:
            proven_os = executed_passed_os.get(gate_id)
            if proven_os != required_os:
                return "awaiting_ci"
        return EVIDENCE_PASSED

    if required_os is not None:
        # Default-feature platform lifecycle is OS-bound and never borrowed from
        # another OS or from untagged/all-features junit name matches.
        return checked_in_state if checked_in_state in CHECKED_IN_GATE_STATES else "awaiting_ci"

    if command_is_feature_scoped(command):
        # Same principle, bound to features instead of OS: the all-features
        # junit cannot witness a reduced build, and the test name is shared with
        # the all-features gate, so only executed evidence closes this one.
        return checked_in_state if checked_in_state in CHECKED_IN_GATE_STATES else "awaiting_ci"

    if command.startswith("npm"):
        if npm_gate_passed(command, npm_markers):
            return EVIDENCE_PASSED
        return checked_in_state if checked_in_state in CHECKED_IN_GATE_STATES else "awaiting_ci"

    filter_name = cargo_filter_from_command(command)
    if filter_name is not None:
        for os_junit in junit_by_os.values():
            if filter_matches_junit(filter_name, os_junit):
                return EVIDENCE_PASSED

    argv = shlex.split(command)
    if argv[:2] == ["cargo", "test"] and "--no-run" in argv:
        if "--test" in argv:
            target = argv[argv.index("--test") + 1]
            if any(target in name for names in junit_by_os.values() for name in names):
                return EVIDENCE_PASSED
        if any(junit_by_os.values()) and gate_id.endswith("_compile"):
            return "awaiting_ci"
    return checked_in_state if checked_in_state in CHECKED_IN_GATE_STATES else "awaiting_ci"


def run_gate_command(repository: Path, command: str) -> bool:
    completed = subprocess.run(
        shlex.split(command),
        cwd=repository,
        check=False,
        capture_output=True,
        text=True,
    )
    return completed.returncode == 0


def require_ci_gate_status_shape(
    packet: dict[str, Any],
    gate_ids: list[str],
    *,
    fail,
) -> dict[str, str]:
    status = packet.get("ci_gate_status")
    if not isinstance(status, dict):
        fail("ci_gate_status must be an object mapping every ci_gate_id")
    if set(status) != set(gate_ids):
        missing = sorted(set(gate_ids) - set(status))
        extra = sorted(set(status) - set(gate_ids))
        detail = []
        if missing:
            detail.append("missing=" + ",".join(missing))
        if extra:
            detail.append("extra=" + ",".join(extra))
        fail("ci_gate_status keys must exactly match ci_gate_ids (" + "; ".join(detail) + ")")
    normalized: dict[str, str] = {}
    for gate_id, value in status.items():
        if value not in CHECKED_IN_GATE_STATES:
            fail(
                f"ci_gate_status[{gate_id}] must be awaiting_ci or failed "
                "(passed is never checked in; it comes from CI/junit/command evidence)"
            )
        normalized[str(gate_id)] = str(value)
    return normalized


def gates_required_for_mode(gate_ids: list[str], runner_os: str | None) -> list[str]:
    """Full aggregation requires every gate; runner-scoped mode skips other OS platforms."""
    if runner_os is None:
        return list(gate_ids)
    if runner_os not in KNOWN_RUNNER_OS:
        raise ValueError(f"unknown runner OS {runner_os!r}")
    required: list[str] = []
    for gate_id in gate_ids:
        required_os = PLATFORM_GATE_OS.get(gate_id)
        if required_os is None or required_os == runner_os:
            required.append(gate_id)
    return required
