#!/usr/bin/env python3
"""Focused tests for exact release artifact coverage."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile


SCRIPT = Path(__file__).with_name("check-release-artifacts.py")


def run(
    root: Path,
    expect_success: bool,
    profile: str = "stable",
    *,
    allow_missing: bool = False,
) -> None:
    command = [
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
    ]
    if allow_missing:
        command.append("--allow-missing-targets")
    command.extend(
        [
            "--mcpbs",
            str(root / "mcpbs"),
        ]
    )
    completed = subprocess.run(
        command,
        capture_output=True,
        text=True,
    )
    if (completed.returncode == 0) != expect_success:
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
        run(root, True)
        (root / "mcpbs" / expected["mcpbs"][0]).unlink()
        run(root, False)
        (root / "mcpbs" / expected["mcpbs"][0]).write_bytes(b"artifact")
        (root / "binaries" / "unexpected.zip").write_bytes(b"artifact")
        run(root, False)
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
        run(root, True, profile="beta")
        (root / "binaries" / "tracedecay-beta-v1.2.3-linux.tar.gz").unlink()
        run(root, False, profile="beta")

        # Partial publish: a whole missing target is accepted, a half target
        # (archive without MCPB) is not, a foreign file is not, and an empty
        # set is not.
        run(root, False, profile="beta", allow_missing=True)
        (root / "mcpbs" / "tracedecay-beta-v1.2.3-linux.mcpb").unlink()
        run(root, True, profile="beta", allow_missing=True)
        (root / "binaries" / "stray.tar.gz").write_bytes(b"artifact")
        run(root, False, profile="beta", allow_missing=True)
        (root / "binaries" / "stray.tar.gz").unlink()
        (root / "binaries" / "tracedecay-beta-v1.2.3-windows.zip").unlink()
        (root / "mcpbs" / "tracedecay-beta-v1.2.3-windows.mcpb").unlink()
        run(root, False, profile="beta", allow_missing=True)
        # An MCPB that leaked into the binaries directory (a `tracedecay-beta-*`
        # artifact glob did this) is foreign there, in both modes.
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
        run(root, False, profile="beta")
        run(root, False, profile="beta", allow_missing=True)
    print("release artifact validator tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
