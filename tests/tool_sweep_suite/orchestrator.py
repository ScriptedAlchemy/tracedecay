#!/usr/bin/env python3
"""Run the dynamic MCP catalog with isolated read and effect profiles.

The read phase owns the negotiated catalog. Every available non-read operation
then gets a new harness-owned profile, project fixture, daemon, and producer /
consumer journey. A missing phase result is a failure, never a skipped tool.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any

from reports import PASSING_VERDICTS, write_reports
from runner import catalog_manifest, dispatch_policy, load_catalog_manifest
from journeys import has_effect_journey
from sweep import READ_ONLY_EFFECTS, _metadata_matches_annotations


INHERITED_ENVIRONMENT = frozenset(
    {
        "PATH",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
    }
)


@dataclass(frozen=True)
class PhaseResult:
    label: str
    root: Path
    returncode: int | None
    launch_error: str | None = None

    def value(self) -> dict[str, Any]:
        value = asdict(self)
        value["root"] = str(self.root)
        return value


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def _policy_for(definition: dict[str, Any]) -> Any | None:
    try:
        policy = dispatch_policy(definition)
    except Exception:
        return None
    return policy if _metadata_matches_annotations(definition, policy) is None else None


def _advertised_effect_names(manifest: dict[str, Any]) -> list[str]:
    """Derive every available mutation from the negotiated read-phase catalog."""
    tools = manifest.get("tools")
    if not isinstance(tools, list):
        raise RuntimeError("catalog manifest tools invalid")
    targets: list[str] = []
    for definition in tools:
        if not isinstance(definition, dict):
            raise RuntimeError("catalog manifest tool invalid")
        policy = _policy_for(definition)
        name = definition.get("name")
        if policy is None or not isinstance(name, str):
            continue
        if policy.availability_state == "available" and policy.effect not in READ_ONLY_EFFECTS:
            targets.append(name)
    return sorted(targets)


def effect_targets(manifest: dict[str, Any]) -> list[str]:
    """Select only mutations backed by a real reversible harness journey."""
    return [name for name in _advertised_effect_names(manifest) if has_effect_journey(name)]


def _policy_row(definition: dict[str, Any], note: str) -> dict[str, Any]:
    policy = _policy_for(definition)
    return {
        "name": definition.get("name", "unnamed-tool"),
        "effect": None if policy is None else policy.effect,
        "availability": None if policy is None else policy.availability_state,
        "deadline_ms": None if policy is None else policy.deadline_ms,
        "verdict": "FAIL",
        "note": note,
        "rollback": "not_started",
    }


def _rows(report: Any) -> list[dict[str, Any]]:
    if not isinstance(report, dict):
        return []
    values = report.get("tools")
    if not isinstance(values, list):
        return []
    return [dict(row) for row in values if isinstance(row, dict)]


def _report_fatal(report: Any, label: str, errors: list[str]) -> None:
    if not isinstance(report, dict):
        errors.append(f"{label} phase emitted no parseable report")
        return
    fatal = report.get("fatal")
    if isinstance(fatal, str) and fatal:
        errors.append(f"{label} phase fatal: {fatal}")


def _phase_execution_errors(expected: set[str], phases: dict[str, Any], errors: list[str]) -> None:
    for label in sorted(expected):
        phase = phases.get(label)
        if not isinstance(phase, dict):
            errors.append(f"{label} phase did not leave launch evidence")
            continue
        launch_error = phase.get("launch_error")
        if isinstance(launch_error, str) and launch_error:
            errors.append(f"{label} phase failed to launch: {launch_error}")
            continue
        returncode = phase.get("returncode")
        if not isinstance(returncode, int) or isinstance(returncode, bool) or returncode != 0:
            errors.append(f"{label} phase exited nonzero: {returncode!r}")
    for label in sorted(set(phases) - expected):
        errors.append(f"unexpected phase launch evidence: {label}")


def merge_phase_reports(
    manifest: dict[str, Any],
    read_report: dict[str, Any] | None,
    effect_reports: dict[str, dict[str, Any] | None],
    phases: dict[str, Any],
) -> dict[str, Any]:
    """Merge phase artifacts without allowing an omitted catalog member green."""
    canonical = catalog_manifest(manifest.get("tools", []))
    if canonical != manifest:
        raise RuntimeError("aggregate received a non-canonical catalog manifest")
    expected_names = set(canonical["tool_names"])
    advertised_effect_names = set(_advertised_effect_names(canonical))
    effect_names = set(effect_targets(canonical))
    missing_journey_names = advertised_effect_names - effect_names
    definitions = {definition["name"]: definition for definition in canonical["tools"]}
    merged: dict[str, dict[str, Any]] = {}
    errors: list[str] = []
    expected_phases = {"reads"}
    expected_phases.update(
        _phase_label(name, index) for index, name in enumerate(sorted(effect_names), start=1)
    )
    _phase_execution_errors(expected_phases, phases, errors)

    def add(row: dict[str, Any], source: str) -> None:
        name = row.get("name")
        if not isinstance(name, str) or name not in expected_names:
            errors.append(f"{source} phase emitted an unknown tool row: {name!r}")
            return
        if name in merged:
            errors.append(f"duplicate tool row across phases: {name}")
            return
        merged[name] = row

    _report_fatal(read_report, "reads", errors)
    for row in _rows(read_report):
        name = row.get("name")
        if name in advertised_effect_names:
            row["verdict"] = "FAIL"
            row["note"] = "mutable tool ran in shared read phase"
        add(row, "reads")

    for name in sorted(missing_journey_names):
        add(
            _policy_row(
                definitions[name],
                "advertised mutation has no real producer/consumer/rollback journey",
            ),
            "catalog",
        )

    for name in sorted(effect_names):
        report = effect_reports.get(name)
        _report_fatal(report, f"effect:{name}", errors)
        rows = _rows(report)
        target_rows = [row for row in rows if row.get("name") == name]
        extras = [row.get("name") for row in rows if row.get("name") != name]
        if extras:
            errors.append(f"effect:{name} phase emitted unrelated rows: {extras!r}")
        if len(target_rows) != 1:
            add(_policy_row(definitions[name], "isolated effect phase omitted its negotiated tool"), f"effect:{name}")
            if len(target_rows) > 1:
                errors.append(f"effect:{name} phase emitted duplicate target rows")
            continue
        add(target_rows[0], f"effect:{name}")

    for name in sorted(expected_names - set(merged)):
        add(_policy_row(definitions[name], "phase plan omitted negotiated tool"), "aggregate")

    ordered = [merged[name] for name in canonical["tool_names"] if name in merged]
    failed = sum(1 for row in ordered if row.get("verdict") not in PASSING_VERDICTS)
    report: dict[str, Any] = {
        "schema_version": 2,
        "phase": "aggregate",
        "catalog": {
            "fingerprint": canonical["fingerprint"],
            "tool_names": canonical["tool_names"],
        },
        "discovered_tools": canonical["tool_names"],
        "tools": ordered,
        "phases": phases,
        "summary": {
            "discovered": len(canonical["tool_names"]),
            "completed": len(ordered),
            "failed": failed,
        },
    }
    if errors:
        report["fatal"] = "; ".join(errors)
    return report


def _phase_environment(root: Path) -> dict[str, str]:
    environment = {
        key: value
        for key in INHERITED_ENVIRONMENT
        if (value := os.environ.get(key))
    }
    paths = {
        "HOME": root / "home",
        "CODEX_HOME": root / "codex",
        "XDG_CONFIG_HOME": root / "config",
        "XDG_DATA_HOME": root / "data",
        "XDG_STATE_HOME": root / "state",
        "TMPDIR": root / "tmp",
        "TMP": root / "tmp",
        "TEMP": root / "tmp",
        "TRACEDECAY_PROFILE_DIR": root / "profile",
        "TRACEDECAY_DATA_DIR": root / "profile",
        "TRACEDECAY_GLOBAL_DB": root / "profile" / "global.db",
        "TRACEDECAY_DAEMON_SOCKET": root / "daemon.sock",
    }
    for key, path in paths.items():
        (path.parent if key in {"TRACEDECAY_GLOBAL_DB", "TRACEDECAY_DAEMON_SOCKET"} else path).mkdir(
            parents=True, exist_ok=True
        )
    environment.update({key: str(path) for key, path in paths.items()})
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment


def _phase_label(name: str, index: int) -> str:
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "-", name).strip("-.")
    if not safe:
        raise RuntimeError(f"effect name cannot form an artifact label: {name!r}")
    return f"effects/{index:03d}-{safe}"


def run_phase(
    *,
    repo: Path,
    binary: Path,
    out: Path,
    shards: int,
    label: str,
    phase: str,
    effect: str | None = None,
    catalog: Path | None = None,
) -> PhaseResult:
    root = out / "phases" / label
    if root.exists():
        raise RuntimeError(f"refusing to reuse phase artifact directory: {root}")
    root.mkdir(parents=True)
    command = [
        str(repo / "scripts/with-isolated-tracedecay-daemon.sh"),
        "--bin",
        str(binary),
        "--ready-timeout",
        "60",
        "--stop-timeout",
        "10",
        "--lifecycle-label",
        f"MCP tool sweep {label}",
        "--",
        sys.executable,
        str(repo / "tests/tool_sweep_suite/runner.py"),
        "--bin",
        str(binary),
        "--out",
        str(root),
        "--shards",
        str(shards),
        "--phase",
        phase,
    ]
    if effect is not None:
        command.extend(["--effect", effect])
    if catalog is not None:
        command.extend(["--catalog", str(catalog)])
    try:
        with (root / "stdout.log").open("wb") as stdout, (root / "stderr.log").open("wb") as stderr:
            completed = subprocess.run(command, cwd=repo, env=_phase_environment(root), stdout=stdout, stderr=stderr, check=False)
    except OSError as error:
        (root / "launch-error.txt").write_text(f"{error}\n")
        return PhaseResult(label=label, root=root, returncode=None, launch_error=str(error))
    return PhaseResult(label=label, root=root, returncode=completed.returncode)


def _load_report(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run all negotiated MCP tools in isolated phase profiles.")
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--shards", type=int, default=4)
    args = parser.parse_args(argv)
    args.repo = args.repo.resolve()
    args.bin = args.bin.resolve()
    args.out = args.out.resolve()
    if not (args.repo / "scripts/with-isolated-tracedecay-daemon.sh").is_file():
        parser.error(f"repository harness missing under: {args.repo}")
    if not args.bin.is_file() or not args.bin.stat().st_mode & 0o111:
        parser.error(f"release binary is not executable: {args.bin}")
    if args.shards < 1:
        parser.error("--shards must be positive")
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)
    started = _utc_now()
    phases: dict[str, Any] = {}
    read_phase = run_phase(
        repo=args.repo,
        binary=args.bin,
        out=args.out,
        shards=args.shards,
        label="reads",
        phase="reads",
    )
    phases[read_phase.label] = read_phase.value()
    read_root = read_phase.root
    read_report = _load_report(read_root / "results.json")
    try:
        manifest = load_catalog_manifest(read_root / "catalog.json")
    except Exception as error:
        report = {
            "schema_version": 2,
            "phase": "aggregate",
            "started_at": started,
            "finished_at": _utc_now(),
            "tools": [],
            "discovered_tools": [],
            "phases": phases,
            "fatal": f"read phase did not leave a valid negotiated catalog: {error}",
            "summary": {"discovered": 0, "completed": 0, "failed": 0},
        }
        write_reports(args.out, report)
        return 1

    effect_reports: dict[str, dict[str, Any] | None] = {}
    for index, name in enumerate(effect_targets(manifest), start=1):
        phase = run_phase(
            repo=args.repo,
            binary=args.bin,
            out=args.out,
            shards=args.shards,
            label=_phase_label(name, index),
            phase="effect",
            effect=name,
            catalog=read_root / "catalog.json",
        )
        phases[phase.label] = phase.value()
        effect_reports[name] = _load_report(phase.root / "results.json")

    report = merge_phase_reports(manifest, read_report, effect_reports, phases)
    report["started_at"] = started
    report["finished_at"] = _utc_now()
    write_reports(args.out, report)
    return 0 if "fatal" not in report and report["summary"]["failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
