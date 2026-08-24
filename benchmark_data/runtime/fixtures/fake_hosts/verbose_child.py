#!/usr/bin/env python3
"""Verbose child that can outlive TERM for process-group reaping tests."""

from __future__ import annotations

import argparse
import signal
import sys
import time


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lines", type=int, default=0)
    parser.add_argument("--hang-seconds", type=float, default=0.0)
    parser.add_argument("--ignore-term", action="store_true")
    arguments = parser.parse_args()

    if arguments.ignore_term:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)

    padding = "x" * 80
    for index in range(arguments.lines):
        sys.stdout.write(f"child-stdout-{index:06d}-{padding}\n")
        sys.stderr.write(f"child-stderr-{index:06d}-{padding}\n")
    sys.stdout.flush()
    sys.stderr.flush()

    if arguments.hang_seconds:
        time.sleep(arguments.hang_seconds)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
