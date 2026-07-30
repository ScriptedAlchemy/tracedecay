#!/usr/bin/env python3
"""Cross-platform fixtures for installed binary path resolution."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile


RESOLVER = Path(__file__).with_name("resolve-installed-binary.py")


def resolve(root: Path, runner_os: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(RESOLVER), str(root), runner_os],
        check=False,
        capture_output=True,
        text=True,
    )


def main() -> int:
    with tempfile.TemporaryDirectory() as temporary_directory:
        root = Path(temporary_directory)
        binary_directory = root / "bin"
        binary_directory.mkdir()
        windows_binary = binary_directory / "tracedecay.exe"
        windows_binary.write_bytes(b"MZ")

        windows = resolve(root, "Windows")
        if windows.returncode != 0:
            raise SystemExit(windows.stderr)
        if Path(windows.stdout.strip()) != windows_binary:
            raise SystemExit("Windows resolver did not return tracedecay.exe")

        extensionless = resolve(root, "Linux")
        if extensionless.returncode == 0:
            raise SystemExit("Unix resolver incorrectly accepted tracedecay.exe")

        windows_binary.unlink()
        unix_binary = binary_directory / "tracedecay"
        unix_binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        unix_binary.chmod(unix_binary.stat().st_mode | 0o111)
        unix = resolve(root, "Linux")
        if unix.returncode != 0:
            raise SystemExit(unix.stderr)
        if Path(unix.stdout.strip()) != unix_binary:
            raise SystemExit("Unix resolver did not return tracedecay")
        if not os.access(unix_binary, os.X_OK):
            raise SystemExit("Unix fixture unexpectedly lost executable mode")

    print("installed binary path fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
