#!/usr/bin/env python3
"""Resolve the cargo-installed TraceDecay binary across release platforms."""

from __future__ import annotations

import argparse
import os
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("install_root", type=Path)
    parser.add_argument("runner_os")
    arguments = parser.parse_args()

    name = "tracedecay.exe" if arguments.runner_os == "Windows" else "tracedecay"
    binary = arguments.install_root.resolve() / "bin" / name
    if not binary.is_file():
        raise SystemExit(f"cargo install did not produce {binary}")
    if arguments.runner_os != "Windows" and not os.access(binary, os.X_OK):
        raise SystemExit(f"cargo-installed binary is not executable: {binary}")
    print(binary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
