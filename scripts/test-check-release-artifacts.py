#!/usr/bin/env python3
"""Focused tests for exact release artifact coverage."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile


SCRIPT = Path(__file__).with_name("check-release-artifacts.py")


def run(root: Path, profile: str = "stable") -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--manifest",
            str(root / "targets.json"),
            "--tag",
            "v1.2.3",
            "--profile",
            profile,
            "--binaries",
            str(root / "binaries"),
            "--mcpbs",
            str(root / "mcpbs"),
        ],
        capture_output=True,
        text=True,
    )


def expect(
    root: Path,
    status: int,
    message: str,
    profile: str = "stable",
) -> None:
    """Success reports on stdout and a refusal on stderr, each as one line."""
    completed = run(root, profile)
    output = completed.stdout if status == 0 else completed.stderr
    if completed.returncode != status or output != f"{message}\n":
        raise AssertionError(completed.stdout + completed.stderr)


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        manifest = {
            "include": [
                {
                    "name": "linux",
                    "runner": "linux",
                    "target": "linux",
                    "archive": "tar.gz",
                },
                {
                    "name": "windows",
                    "runner": "windows",
                    "target": "windows",
                    "archive": "zip",
                },
            ]
        }
        (root / "targets.json").write_text(json.dumps(manifest), encoding="utf-8")
        for child in ("binaries", "mcpbs"):
            (root / child).mkdir()
        expected = {
            "binaries": (
                "tracedecay-v1.2.3-linux.tar.gz",
                "tracedecay-v1.2.3-windows.zip",
            ),
            "mcpbs": (
                "tracedecay-v1.2.3-linux.mcpb",
                "tracedecay-v1.2.3-windows.mcpb",
            ),
        }
        for child, names in expected.items():
            for name in names:
                (root / child / name).write_bytes(b"artifact")
        expect(root, 0, "release artifact coverage matches target manifest")
        (root / "mcpbs" / expected["mcpbs"][0]).unlink()
        expect(root, 1, "MCPB coverage mismatch: missing tracedecay-v1.2.3-linux.mcpb")
        (root / "mcpbs" / expected["mcpbs"][0]).write_bytes(b"artifact")
        (root / "binaries" / "unexpected.zip").write_bytes(b"artifact")
        expect(root, 1, "binary coverage mismatch: unexpected unexpected.zip")
        for item in (root / "binaries").iterdir():
            item.unlink()
        for item in (root / "mcpbs").iterdir():
            item.unlink()
        for target in manifest["include"]:
            (root / "binaries" / (
                f"tracedecay-beta-v1.2.3-{target['name']}.{target['archive']}"
            )).write_bytes(b"artifact")
            (root / "mcpbs" / (
                f"tracedecay-beta-v1.2.3-{target['name']}.mcpb"
            )).write_bytes(b"artifact")
        expect(root, 0, "release artifact coverage matches target manifest", profile="beta")
        (root / "binaries" / "tracedecay-beta-v1.2.3-linux.tar.gz").unlink()
        expect(
            root,
            1,
            "binary coverage mismatch: missing tracedecay-beta-v1.2.3-linux.tar.gz",
            profile="beta",
        )

        # A whole missing target (beta.86 lost macOS, #3756) is refused too.
        (root / "mcpbs" / "tracedecay-beta-v1.2.3-linux.mcpb").unlink()
        expect(
            root,
            1,
            "binary coverage mismatch: missing tracedecay-beta-v1.2.3-linux.tar.gz",
            profile="beta",
        )
        # An MCPB that leaked into the binaries directory (a `tracedecay-beta-*`
        # artifact glob did this) is foreign there.
        for child in ("binaries", "mcpbs"):
            for item in (root / child).iterdir():
                item.unlink()
        for target in manifest["include"]:
            (root / "binaries" / (
                f"tracedecay-beta-v1.2.3-{target['name']}.{target['archive']}"
            )).write_bytes(b"artifact")
            (root / "mcpbs" / (
                f"tracedecay-beta-v1.2.3-{target['name']}.mcpb"
            )).write_bytes(b"artifact")
        (root / "binaries" / "tracedecay-beta-v1.2.3-linux.mcpb").write_bytes(b"artifact")
        expect(
            root,
            1,
            "binary coverage mismatch: unexpected tracedecay-beta-v1.2.3-linux.mcpb",
            profile="beta",
        )
    print("release artifact validator tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
