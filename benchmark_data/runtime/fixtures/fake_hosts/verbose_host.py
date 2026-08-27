#!/usr/bin/env python3
"""Deterministic host output, capture IDs, and hanging-child behavior."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def _write_record(
    capture_id: str,
    *,
    activated: bool,
    restart_required: bool,
) -> None:
    print(
        json.dumps(
            {
                "activated": activated,
                "availability": "available",
                "capture_id": capture_id,
                "restart_required": restart_required,
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        flush=True,
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture-id")
    parser.add_argument("--repeat-capture-id", type=int, default=1)
    parser.add_argument("--conflicting-capture-id")
    parser.add_argument("--activated", action="store_true")
    parser.add_argument("--restart-required", action="store_true")
    parser.add_argument("--exit-code", type=int, default=0)
    parser.add_argument("--spawn-child", action="store_true")
    parser.add_argument("--child-lines", type=int, default=0)
    parser.add_argument("--child-hang-seconds", type=float, default=0.0)
    parser.add_argument("--child-ignore-term", action="store_true")
    arguments = parser.parse_args()

    sys.stdin.buffer.read()
    if arguments.capture_id is not None:
        for _ in range(arguments.repeat_capture_id):
            _write_record(
                arguments.capture_id,
                activated=arguments.activated,
                restart_required=arguments.restart_required,
            )
    if arguments.conflicting_capture_id is not None:
        _write_record(
            arguments.conflicting_capture_id,
            activated=arguments.activated,
            restart_required=arguments.restart_required,
        )

    if arguments.spawn_child:
        command = [
            sys.executable,
            str(Path(__file__).with_name("verbose_child.py")),
            "--lines",
            str(arguments.child_lines),
            "--hang-seconds",
            str(arguments.child_hang_seconds),
        ]
        if arguments.child_ignore_term:
            command.append("--ignore-term")
        child = subprocess.Popen(command)
        return child.wait()
    return arguments.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
