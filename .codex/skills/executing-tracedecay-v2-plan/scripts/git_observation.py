#!/usr/bin/env python3
"""Bounded Git observations shared by V2 plan-execution validators."""

from __future__ import annotations

import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any


GIT_TIMEOUT_SECONDS = 10
MAX_GIT_OUTPUT_BYTES = 64 * 1024


@dataclass(frozen=True)
class GitResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    error: str | None = None


def _bounded(stream: Any, label: str, maximum: int) -> tuple[bytes, str | None]:
    size = stream.tell()
    stream.seek(0)
    payload = stream.read(maximum + 1)
    if size > maximum or len(payload) > maximum:
        return b"", f"{label} exceeded {maximum} bytes"
    return payload, None


def run_git(root: Path, *args: str, max_output_bytes: int = MAX_GIT_OUTPUT_BYTES) -> GitResult:
    """Run Git with finite time and output bounds; never raise process errors."""
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            completed = subprocess.run(
                ["git", *args], cwd=root, stdout=stdout, stderr=stderr, check=False,
                timeout=GIT_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            return GitResult(-1, b"", b"", f"timed out after {GIT_TIMEOUT_SECONDS} seconds")
        except OSError as error:
            return GitResult(-1, b"", b"", f"{type(error).__name__}: {error}")
        out, out_error = _bounded(stdout, "stdout", max_output_bytes)
        err, err_error = _bounded(stderr, "stderr", MAX_GIT_OUTPUT_BYTES)
    return GitResult(completed.returncode, out, err, out_error or err_error)
