#!/usr/bin/env python3
"""Cargo-free command line entrypoint for runtime performance captures."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, NoReturn, Sequence


SCHEMA_VERSION = 1
SUBCOMMANDS = ("prepare", "capture", "paired", "compare", "smoke")
FORBIDDEN_REPORT_FIELDS = frozenset({"pr_stage", "milestone_budget_ns"})


class HarnessError(RuntimeError):
    """A user-actionable harness validation or execution failure."""


def fail(message: str) -> NoReturn:
    raise HarnessError(message)


def require_binary(value: str | os.PathLike[str]) -> Path:
    path = Path(value).expanduser()
    if not path.exists():
        fail(f"binary does not exist: {path}")
    if not path.is_file():
        fail(f"binary is not a regular file: {path}")
    if not os.access(path, os.X_OK):
        fail(f"binary is not executable: {path}")
    return path.resolve()


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        fail(f"cannot read JSON report {path}: {exc}")
    if not isinstance(value, dict):
        fail(f"JSON report must be an object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def validate_comparison_report(report: dict[str, Any], label: str) -> None:
    forbidden = sorted(FORBIDDEN_REPORT_FIELDS.intersection(report))
    if forbidden:
        fail(f"{label} report contains forbidden field: {forbidden[0]}")

    identity = report.get("identity")
    if not isinstance(identity, dict):
        fail(f"{label} report identity is required")
    for field in ("crate_id", "journey_id", "workload_id"):
        if not isinstance(identity.get(field), str) or not identity[field]:
            fail(f"{label} report identity.{field} is required")

    route = report.get("production_route")
    outcome = report.get("outcome")
    if (
        isinstance(identity.get("journey_id"), str)
        and identity["journey_id"].startswith("remote-")
        and isinstance(outcome, dict)
        and outcome.get("status") == "success"
    ):
        if not isinstance(route, dict) or not (
            route.get("committed") is True and route.get("mounted") is True
        ):
            fail(
                f"{label} remote route is unwired: committed production route "
                "must be mounted before success"
            )

    if "measurements" not in report:
        fail(f"{label} report measurements are required")


def prepare(args: argparse.Namespace) -> int:
    binary = require_binary(args.binary)
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=False)
    write_json(
        output / "prepared.json",
        {
            "schema_version": SCHEMA_VERSION,
            "binary": os.fspath(binary),
            "capture_policy": "n=1_regression_only",
            "latency_policy": "advisory_until_stable_baseline",
        },
    )
    return 0


def compare(args: argparse.Namespace) -> int:
    baseline = load_json(Path(args.baseline))
    treatment = load_json(Path(args.treatment))
    baseline_schema = baseline.get("schema_version")
    treatment_schema = treatment.get("schema_version")
    if baseline_schema != treatment_schema:
        fail(
            "report schema mismatch: "
            f"baseline={baseline_schema!r}, treatment={treatment_schema!r}"
        )
    if baseline_schema != SCHEMA_VERSION:
        fail(f"unsupported report schema: {baseline_schema!r}")
    validate_comparison_report(baseline, "baseline")
    validate_comparison_report(treatment, "treatment")
    baseline_fixture = baseline.get("fixture_digest")
    treatment_fixture = treatment.get("fixture_digest")
    if (
        baseline_fixture is not None
        and treatment_fixture is not None
        and baseline_fixture != treatment_fixture
    ):
        fail("report fixture digest mismatch")
    write_json(
        Path(args.output),
        {
            "schema_version": SCHEMA_VERSION,
            "decision": "descriptive_only",
            "evidence_class": "n=1_regression_only",
            "latency_policy": "advisory_until_stable_baseline",
            "baseline": os.fspath(Path(args.baseline)),
            "treatment": os.fspath(Path(args.treatment)),
        },
    )
    return 0


def not_yet_captured(args: argparse.Namespace) -> int:
    require_binary(args.binary)
    fail(f"{args.command} requires a prepared runtime fixture")


def paired(args: argparse.Namespace) -> int:
    baseline = require_binary(args.baseline)
    treatment = require_binary(args.treatment)
    if baseline.samefile(treatment):
        fail("baseline and treatment resolve to the same binary")
    fail("paired requires a prepared runtime fixture")


def parser() -> argparse.ArgumentParser:
    argument_parser = argparse.ArgumentParser(
        description="Capture Cargo-free TraceDecay CLI/MCP runtime performance evidence."
    )
    subparsers = argument_parser.add_subparsers(
        dest="command",
        required=True,
        metavar="{" + ",".join(SUBCOMMANDS) + "}",
    )

    prepare_parser = subparsers.add_parser(
        "prepare",
        help="prepare deterministic fixtures without starting a daemon",
    )
    prepare_parser.add_argument("--binary", required=True)
    prepare_parser.add_argument("--output", required=True)
    prepare_parser.set_defaults(handler=prepare)

    capture_parser = subparsers.add_parser(
        "capture",
        help="capture one candidate into raw JSONL and a report",
    )
    capture_parser.add_argument("--binary", required=True)
    capture_parser.add_argument("--output", required=True)
    capture_parser.add_argument("--prepared")
    capture_parser.set_defaults(handler=not_yet_captured)

    paired_parser = subparsers.add_parser(
        "paired",
        help="capture an ABBA baseline/treatment comparison",
    )
    paired_parser.add_argument("--baseline", required=True)
    paired_parser.add_argument("--treatment", required=True)
    paired_parser.add_argument("--output", required=True)
    paired_parser.add_argument("--prepared")
    paired_parser.set_defaults(handler=paired)

    compare_parser = subparsers.add_parser(
        "compare",
        help="compare compatible captured reports",
    )
    compare_parser.add_argument("--baseline", required=True)
    compare_parser.add_argument("--treatment", required=True)
    compare_parser.add_argument("--output", required=True)
    compare_parser.set_defaults(handler=compare)

    smoke_parser = subparsers.add_parser(
        "smoke",
        help="run a bounded capture against an explicit prebuilt binary",
    )
    smoke_parser.add_argument("--binary", required=True)
    smoke_parser.add_argument("--output", required=True)
    smoke_parser.add_argument("--prepared")
    smoke_parser.set_defaults(handler=not_yet_captured)
    return argument_parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        return int(arguments.handler(arguments))
    except HarnessError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
