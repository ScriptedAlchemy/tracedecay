#!/usr/bin/env python3
"""Validate or explicitly remeasure a PR11-PR13 compile-contract baseline.

Validation is side-effect free.  ``--run`` executes the declared argv and
prints a candidate measurement; it never rewrites a checked-in baseline.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, NoReturn, cast


VALID_STATUSES = {"measured", "placeholder", "unavailable"}


def fail(message: str) -> NoReturn:
    raise SystemExit(f"invalid compile baseline: {message}")


def load_baseline(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path}: {error}")
    if not isinstance(value, dict):
        fail("root must be an object")
    return cast(dict[str, Any], value)


def validate(value: dict[str, Any]) -> tuple[list[str], dict[str, Any]]:
    if value.get("schema_version") != 1:
        fail("schema_version must be 1")
    if not isinstance(value.get("workload_id"), str) or not value["workload_id"]:
        fail("workload_id must be a non-empty string")
    packages = value.get("packages")
    if not isinstance(packages, list) or not packages or not all(
        isinstance(package, str) and package for package in packages
    ):
        fail("packages must be a non-empty array of names")
    command = value.get("command")
    if not isinstance(command, list) or not command or not all(
        isinstance(argument, str) and argument for argument in command
    ):
        fail("command must be a non-empty argv array")
    if command[0] != "cargo":
        fail("command must start with cargo")
    measurement = value.get("measurement")
    if not isinstance(measurement, dict):
        fail("measurement must be an object")
    status = measurement.get("status")
    if status not in VALID_STATUSES:
        fail(f"measurement.status must be one of {sorted(VALID_STATUSES)}")
    elapsed_ms = measurement.get("elapsed_ms")
    if status == "measured":
        if not isinstance(elapsed_ms, int) or elapsed_ms <= 0:
            fail("measured baselines require a positive integer elapsed_ms")
        if not isinstance(measurement.get("recorded_at"), str):
            fail("measured baselines require recorded_at")
    elif elapsed_ms is not None:
        fail("placeholder or unavailable baselines must use elapsed_ms: null")
    return cast(list[str], command), cast(dict[str, Any], measurement)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", type=Path)
    parser.add_argument(
        "--run",
        action="store_true",
        help="execute the declared command and print, but do not persist, a candidate measurement",
    )
    args = parser.parse_args()

    command, measurement = validate(load_baseline(args.baseline))
    if not args.run:
        print(
            f"valid {args.baseline}: status={measurement['status']} command={' '.join(command)}"
        )
        return 0

    repository = Path(__file__).resolve().parents[2]
    started = time.perf_counter_ns()
    completed = subprocess.run(command, cwd=repository, check=False)
    elapsed_ms = (time.perf_counter_ns() - started) // 1_000_000
    candidate = {
        "measurement": {
            "status": "measured" if completed.returncode == 0 else "unavailable",
            "elapsed_ms": elapsed_ms if completed.returncode == 0 else None,
            "recorded_at": datetime.now(UTC).replace(microsecond=0).isoformat(),
            "exit_code": completed.returncode,
        }
    }
    print(json.dumps(candidate, indent=2, sort_keys=True))
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
