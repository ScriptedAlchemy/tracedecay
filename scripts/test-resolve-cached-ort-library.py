#!/usr/bin/env python3
"""Behavioral tests for cached ONNX Runtime library discovery."""

from __future__ import annotations

import os
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
RESOLVER = ROOT / "scripts" / "resolve-cached-ort-library.py"


def run_resolver(cache_root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["python3", str(RESOLVER), "--cache-root", str(cache_root)],
        text=True,
        capture_output=True,
    )


def run_default_resolver(cache_root: Path) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment["ORT_CACHE_DIR"] = str(cache_root)
    return subprocess.run(
        ["python3", str(RESOLVER)],
        text=True,
        capture_output=True,
        env=environment,
    )


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        cache_root = Path(directory)
        old = cache_root / "dfbin" / "old" / "libonnxruntime.so.1"
        old.parent.mkdir(parents=True)
        old.write_bytes(b"old runtime")
        os.utime(old, ns=(1_000_000_000, 1_000_000_000))

        newest = cache_root / "dfbin" / "new" / "libonnxruntime.dylib"
        newest.parent.mkdir(parents=True)
        newest.write_bytes(b"new runtime")
        os.utime(newest, ns=(2_000_000_000, 2_000_000_000))

        ignored = cache_root / "dfbin" / "newer-link" / "onnxruntime.lib"
        ignored.parent.mkdir(parents=True)
        ignored.symlink_to(newest)
        os.utime(ignored, ns=(3_000_000_000, 3_000_000_000), follow_symlinks=False)

        result = run_resolver(cache_root)
        assert result.returncode == 0, result.stderr
        assert result.stdout.strip() == str(newest.parent.resolve())

        result = run_default_resolver(cache_root)
        assert result.returncode == 0, result.stderr
        assert result.stdout.strip() == str(newest.parent.resolve())

        empty = cache_root / "empty"
        empty.mkdir()
        result = run_resolver(empty)
        assert result.returncode == 0, result.stderr
        assert result.stdout == ""


if __name__ == "__main__":
    main()
