#!/usr/bin/env python3
"""Run every negotiated production MCP surface inside bounded hermetic phases."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
from typing import Any, BinaryIO
from xml.sax.saxutils import escape


SUITE_DIR = Path(__file__).resolve().parent
if str(SUITE_DIR) not in sys.path:
    sys.path.insert(0, str(SUITE_DIR))

from runner import READ_EFFECTS, SweepError, load_manifest, tool_policy


PROBLEM_DEADLINE = "tool_sweep.whole_run_deadline_exceeded"
PROBLEM_PHASE_MISSING = "tool_sweep.phase_result_missing"
INHERITED_ENVIRONMENT = frozenset(
    {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "LANG", "LC_ALL", "LC_CTYPE", "TZ"}
)


@dataclass(frozen=True)
class CommandOutcome:
    returncode: int | None
    cancelled: bool = False
    reason: str | None = None
    launch_error: str | None = None


@dataclass(frozen=True)
class PhaseResult:
    label: str
    root: Path
    outcome: CommandOutcome

    def value(self) -> dict[str, Any]:
        value = asdict(self)
        value["root"] = str(self.root)
        return value


class WholeRunDeadline:
    """One monotonic limit that includes daemon start, fixture work, and all calls."""

    def __init__(self, milliseconds: int) -> None:
        self._ends_at = time.monotonic() + milliseconds / 1_000

    def remaining_s(self) -> float:
        return max(0.0, self._ends_at - time.monotonic())

    def expired(self) -> bool:
        return self.remaining_s() == 0.0


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def catalog_entries(manifest: dict[str, Any]) -> list[dict[str, str]]:
    """Give every live public item one aggregate identity, without a count target."""
    entries: list[dict[str, str]] = []
    for kind, identity in (("tool", "name"), ("resource", "uri"), ("prompt", "name")):
        values = manifest.get(f"{kind}s", [])
        if not isinstance(values, list):
            raise ValueError(f"negotiated {kind}s must be a list")
        for value in values:
            name = value.get(identity) if isinstance(value, dict) else None
            if not isinstance(name, str) or not name:
                raise ValueError(f"negotiated {kind} has no {identity}")
            entries.append({"kind": kind, "name": name})
    return sorted(entries, key=lambda value: (value["kind"], value["name"]))


def _row(kind: str, name: str, verdict: str, note: str, problem_code: str | None) -> dict[str, Any]:
    return {
        "kind": kind,
        "name": name,
        "verdict": verdict,
        "note": note,
        "problem_code": problem_code,
        "elapsed_ms": 0,
        "deadline_ms": 0,
    }


def cancelled_report(manifest: dict[str, Any], reason: str) -> dict[str, Any]:
    """Represent expiry truthfully instead of losing the negotiated surface."""
    entries = [
        _row(entry["kind"], entry["name"], "CANCELLED", reason, PROBLEM_DEADLINE)
        for entry in catalog_entries(manifest)
    ]
    return {
        "schema_version": 1,
        "entries": entries,
        "summary": {"discovered": len(entries), "completed": 0, "failed": 0, "cancelled": len(entries)},
        "fatal": reason,
        "fatal_problem_code": PROBLEM_DEADLINE,
    }


def _summary(rows: list[dict[str, Any]]) -> dict[str, int]:
    return {
        "discovered": len(rows),
        "completed": sum(1 for row in rows if row.get("verdict") != "CANCELLED"),
        "failed": sum(1 for row in rows if row.get("verdict") == "FAIL"),
        "cancelled": sum(1 for row in rows if row.get("verdict") == "CANCELLED"),
    }


def _write_junit(
    out: Path, rows: list[dict[str, Any]], *, fatal: str | None = None,
    fatal_problem_code: str | None = None,
) -> None:
    cases: list[str] = []
    for row in rows:
        name = escape(f"{row['kind']}:{row['name']}", {'"': "&quot;"})
        note = escape(str(row["note"]), {'"': "&quot;"})
        problem_code = row.get("problem_code")
        code = escape(str(problem_code), {'"': "&quot;"}) if isinstance(problem_code, str) else ""
        message = f"{code}: {note}" if code else note
        seconds = int(row.get("elapsed_ms", 0)) / 1_000
        if row["verdict"] == "PASS":
            detail = ""
        elif row["verdict"] == "CANCELLED":
            detail = f'<skipped message="{message}" type="{code}" />'
        else:
            detail = f'<failure message="{message}" type="{code}" />'
        cases.append(f'<testcase name="{name}" time="{seconds:.3f}">{detail}</testcase>')
    if fatal is not None:
        code = escape(fatal_problem_code or "tool_sweep.fatal", {'"': "&quot;"})
        note = escape(fatal, {'"': "&quot;"})
        cases.append(
            f'<testcase name="fatal:aggregate" time="0.000"><error message="{code}: {note}" type="{code}" /></testcase>'
        )
    failures = sum(1 for row in rows if row["verdict"] == "FAIL")
    skipped = sum(1 for row in rows if row["verdict"] == "CANCELLED")
    (out / "junit.xml").write_text(
        f'<testsuite name="mcp-catalog-sweep" tests="{len(rows)}" failures="{failures}" skipped="{skipped}">'
        f'{"".join(cases)}</testsuite>\n'
    )


def write_final_report(out: Path, report: dict[str, Any]) -> None:
    """Persist both final artifacts in a finally path, including after cancellation."""
    out.mkdir(parents=True, exist_ok=True)
    (out / "results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    rows = report.get("entries")
    fatal = report.get("fatal")
    _write_junit(
        out,
        rows if isinstance(rows, list) else [],
        fatal=fatal if isinstance(fatal, str) else None,
        fatal_problem_code=report.get("fatal_problem_code")
        if isinstance(report.get("fatal_problem_code"), str)
        else None,
    )


def load_report(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"report is not an object: {path}")
    return value


def _terminate(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        process.terminate()
    else:
        try:
            os.killpg(os.getpgid(process.pid), signal.SIGTERM)
        except ProcessLookupError:
            return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            process.kill()
        else:
            try:
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
            except ProcessLookupError:
                return
        process.wait(timeout=5)


def run_bounded_command(
    command: list[str], *, cwd: Path, environment: dict[str, str], remaining_s: float,
    stdout: BinaryIO | int | None = None, stderr: BinaryIO | int | None = None,
) -> CommandOutcome:
    """Run one phase under the shared deadline and cancel its full process group on expiry."""
    if remaining_s <= 0:
        return CommandOutcome(None, cancelled=True, reason="whole_run_deadline_exceeded")
    try:
        process = subprocess.Popen(
            command, cwd=cwd, env=environment, stdout=stdout, stderr=stderr,
            start_new_session=os.name != "nt",
        )
    except OSError as error:
        return CommandOutcome(None, launch_error=str(error))
    try:
        return CommandOutcome(process.wait(timeout=remaining_s))
    except subprocess.TimeoutExpired:
        _terminate(process)
        return CommandOutcome(process.returncode, cancelled=True, reason="whole_run_deadline_exceeded")


def _phase_environment(root: Path, *, temp_root: Path | None = None) -> dict[str, str]:
    """Build one hermetic phase environment with a pre-created short temp root."""
    tmp_root = temp_root or root / "tmp"
    environment = {
        key: value
        for key, value in os.environ.items()
        if value and (key in INHERITED_ENVIRONMENT or key.startswith("LC_"))
    }
    environment.update(
        {
            "HOME": str(root / "home"),
            "CODEX_HOME": str(root / "codex"),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_DATA_HOME": str(root / "data"),
            "XDG_STATE_HOME": str(root / "state"),
            "TMPDIR": str(tmp_root),
            "TMP": str(tmp_root),
            "TEMP": str(tmp_root),
            "TRACEDECAY_PROFILE_DIR": str(root / "profile"),
            "TRACEDECAY_DATA_DIR": str(root / "profile"),
            "TRACEDECAY_GLOBAL_DB": str(root / "profile" / "global.db"),
            "TRACEDECAY_DAEMON_SOCKET": str(root / "daemon.sock"),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
    )
    for value in environment.values():
        if value.startswith(str(root)):
            Path(value).parent.mkdir(parents=True, exist_ok=True)
    for variable in (
        "HOME", "CODEX_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
        "TMPDIR", "TMP", "TEMP", "TRACEDECAY_PROFILE_DIR", "TRACEDECAY_DATA_DIR",
    ):
        Path(environment[variable]).mkdir(parents=True, exist_ok=True)
    return environment


def _phase_label(name: str, index: int) -> str:
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "-", name).strip("-.")
    if not safe:
        raise ValueError(f"effect name cannot form an artifact label: {name!r}")
    return f"effects/{index:03d}-{safe}"


def run_phase(
    *, repo: Path, binary: Path, out: Path, deadline: WholeRunDeadline, label: str,
    phase: str, effect: str | None = None, catalog: Path | None = None,
) -> PhaseResult:
    root = out / "phases" / label
    root.mkdir(parents=True, exist_ok=False)
    command = [
        str(repo / "scripts/with-isolated-tracedecay-daemon.sh"), "--bin", str(binary),
        "--ready-timeout", "60", "--stop-timeout", "10", "--lifecycle-label", f"MCP catalog sweep {label}",
        "--", sys.executable, str(repo / "tests/tool_sweep_suite/runner.py"),
        "--bin", str(binary), "--out", str(root), "--phase", phase,
    ]
    if effect is not None:
        command.extend(["--effect", effect])
    if catalog is not None:
        command.extend(["--catalog", str(catalog)])
    # The daemon wrapper appends another random directory and a Unix socket below
    # TMPDIR. Keep that root short even when the retained artifact path is deep.
    with tempfile.TemporaryDirectory(prefix="tds-") as raw_tmp:
        environment = _phase_environment(root, temp_root=Path(raw_tmp))
        with (root / "stdout.log").open("wb") as stdout, (root / "stderr.log").open("wb") as stderr:
            outcome = run_bounded_command(
                command, cwd=repo, environment=environment, remaining_s=deadline.remaining_s(),
                stdout=stdout, stderr=stderr,
            )
    return PhaseResult(label, root, outcome)


def effect_targets(manifest: dict[str, Any]) -> list[str]:
    """Select mutating targets only from this run's negotiated catalog."""
    targets: list[str] = []
    tools = manifest.get("tools")
    if not isinstance(tools, list):
        raise ValueError("catalog tools must be a list")
    for definition in tools:
        if not isinstance(definition, dict):
            continue
        try:
            policy = tool_policy(definition)
        except SweepError:
            continue
        if policy.availability == "available" and policy.effect not in READ_EFFECTS:
            targets.append(policy.name)
    return sorted(set(targets))


def _read_phase_report(phase: PhaseResult) -> dict[str, Any] | None:
    path = phase.root / "results.json"
    try:
        return load_report(path)
    except (OSError, ValueError, json.JSONDecodeError):
        return None


def _missing_row(entry: dict[str, str], *, cancelled: bool) -> dict[str, Any]:
    if cancelled:
        return _row(entry["kind"], entry["name"], "CANCELLED", "whole run deadline exceeded", PROBLEM_DEADLINE)
    return _row(entry["kind"], entry["name"], "FAIL", "phase omitted negotiated surface", PROBLEM_PHASE_MISSING)


def merge_phase_reports(
    manifest: dict[str, Any], read_report: dict[str, Any] | None,
    effect_reports: dict[str, dict[str, Any] | None], deadline_expired: bool,
) -> dict[str, Any]:
    """Aggregate phase rows while making a missing negotiated item an explicit result."""
    expected = {(entry["kind"], entry["name"]): entry for entry in catalog_entries(manifest)}
    effects = set(effect_targets(manifest))
    rows: dict[tuple[str, str], dict[str, Any]] = {}
    errors: list[str] = []

    def add(row: Any, source: str) -> None:
        if not isinstance(row, dict):
            errors.append(f"{source} emitted a non-object result")
            return
        kind, name = row.get("kind"), row.get("name")
        if not isinstance(kind, str) or not isinstance(name, str) or (kind, name) not in expected:
            errors.append(f"{source} emitted an unknown result: {kind!r}:{name!r}")
            return
        if name in effects and kind == "tool" and source == "reads":
            errors.append(f"reads phase emitted mutable tool {name}")
            return
        key = (kind, name)
        if key in rows:
            errors.append(f"duplicate negotiated result: {kind}:{name}")
            return
        verdict = row.get("verdict")
        code = row.get("problem_code")
        if verdict not in {"PASS", "FAIL", "CANCELLED"}:
            errors.append(f"{source} emitted invalid verdict for {kind}:{name}")
            return
        if verdict != "PASS" and not isinstance(code, str):
            errors.append(f"{source} omitted typed problem code for {kind}:{name}")
            return
        rows[key] = dict(row)

    if isinstance(read_report, dict):
        if isinstance(read_report.get("fatal"), str):
            errors.append(f"reads phase fatal: {read_report['fatal']}")
        for row in read_report.get("entries", []):
            add(row, "reads")
    else:
        errors.append("reads phase emitted no report")
    for name in sorted(effects):
        report = effect_reports.get(name)
        if isinstance(report, dict):
            if isinstance(report.get("fatal"), str):
                errors.append(f"effect:{name} phase fatal: {report['fatal']}")
            for row in report.get("entries", []):
                add(row, f"effect:{name}")
        elif not deadline_expired:
            errors.append(f"effect:{name} phase emitted no report")
    for key, entry in expected.items():
        if key not in rows:
            rows[key] = _missing_row(entry, cancelled=deadline_expired)
    ordered = [rows[(entry["kind"], entry["name"])] for entry in catalog_entries(manifest)]
    report: dict[str, Any] = {
        "schema_version": 1,
        "phase": "aggregate",
        "catalog": manifest,
        "entries": ordered,
        "summary": _summary(ordered),
    }
    if errors:
        report["fatal"] = "; ".join(errors)
        report["fatal_problem_code"] = "tool_sweep.phase_fatal"
    elif deadline_expired:
        report["fatal"] = "whole run deadline exceeded"
        report["fatal_problem_code"] = PROBLEM_DEADLINE
    return report


def _catalog_from_phase(phase: PhaseResult) -> dict[str, Any] | None:
    try:
        return load_manifest(phase.root / "catalog.json")
    except SweepError:
        return None


def _phase_execution_errors(phases: list[PhaseResult]) -> list[str]:
    errors: list[str] = []
    for phase in phases:
        outcome = phase.outcome
        if outcome.launch_error:
            errors.append(f"{phase.label} phase failed to launch: {outcome.launch_error}")
        elif outcome.cancelled:
            errors.append(f"{phase.label} phase was cancelled: {outcome.reason}")
        elif outcome.returncode != 0:
            errors.append(f"{phase.label} phase exited nonzero: {outcome.returncode!r}")
    return errors


def run(args: argparse.Namespace) -> int:
    """Run discovery first, then isolated mutation journeys, always emitting final artifacts."""
    deadline = WholeRunDeadline(args.whole_run_deadline_ms)
    report: dict[str, Any] = {
        "schema_version": 1, "phase": "aggregate", "started_at": _utc_now(), "entries": [],
        "summary": {"discovered": 0, "completed": 0, "failed": 0, "cancelled": 0},
    }
    phases: list[PhaseResult] = []
    try:
        read_phase = run_phase(
            repo=args.repo, binary=args.bin, out=args.out, deadline=deadline, label="reads", phase="reads"
        )
        phases.append(read_phase)
        manifest = _catalog_from_phase(read_phase)
        if manifest is None:
            reason = "whole run deadline exceeded before catalog discovery" if deadline.expired() else "read phase did not emit a canonical catalog"
            report.update({
                "fatal": reason,
                "fatal_problem_code": PROBLEM_DEADLINE if deadline.expired() else "tool_sweep.discovery_failed",
            })
        else:
            effect_reports: dict[str, dict[str, Any] | None] = {}
            catalog = read_phase.root / "catalog.json"
            for index, name in enumerate(effect_targets(manifest), start=1):
                if deadline.expired():
                    effect_reports[name] = None
                    continue
                phase = run_phase(
                    repo=args.repo, binary=args.bin, out=args.out, deadline=deadline,
                    label=_phase_label(name, index), phase="effect", effect=name, catalog=catalog,
                )
                phases.append(phase)
                effect_reports[name] = _read_phase_report(phase)
            report = merge_phase_reports(manifest, _read_phase_report(read_phase), effect_reports, deadline.expired())
        execution_errors = _phase_execution_errors(phases)
        if execution_errors:
            existing = report.get("fatal")
            report["fatal"] = "; ".join(([existing] if isinstance(existing, str) and existing else []) + execution_errors)
            report["fatal_problem_code"] = "tool_sweep.phase_execution_failed"
    except Exception as error:
        report["fatal"] = str(error)
        report["fatal_problem_code"] = "tool_sweep.orchestration_failed"
    finally:
        report["started_at"] = report.get("started_at", _utc_now())
        report["finished_at"] = _utc_now()
        report["phases"] = [phase.value() for phase in phases]
        write_final_report(args.out, report)
    return 0 if "fatal" not in report and report["summary"]["failed"] == 0 and report["summary"]["cancelled"] == 0 else 1


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run the negotiated production MCP surface sweep.")
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--whole-run-deadline-ms", type=int, required=True)
    args = parser.parse_args(argv)
    args.repo = args.repo.resolve()
    args.bin = args.bin.resolve()
    args.out = args.out.resolve()
    if not (args.repo / "scripts/with-isolated-tracedecay-daemon.sh").is_file():
        parser.error("--repo must contain the isolated daemon harness")
    if not args.bin.is_file() or not args.bin.stat().st_mode & 0o111:
        parser.error("--bin must name an executable release binary")
    if args.whole_run_deadline_ms <= 0:
        parser.error("--whole-run-deadline-ms must be positive")
    return args


def main(argv: list[str]) -> int:
    return run(parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
