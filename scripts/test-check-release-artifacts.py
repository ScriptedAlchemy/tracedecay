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
    profile: str = "stable",
    *,
    allow_missing: bool = False,
) -> subprocess.CompletedProcess[str]:
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
    return subprocess.run(
        command,
        capture_output=True,
        text=True,
    )


def expect(
    root: Path,
    status: int,
    message: str,
    profile: str = "stable",
    *,
    allow_missing: bool = False,
) -> None:
    """Success reports on stdout and a refusal on stderr, each as one line."""
    completed = run(root, profile, allow_missing=allow_missing)
    output = completed.stdout if status == 0 else completed.stderr
    if completed.returncode != status or output != f"{message}\n":
        raise AssertionError(completed.stdout + completed.stderr)


BETA_PARTIAL = {"profile": "beta", "allow_missing": True}


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

        # Partial publish: a whole missing target is accepted, a half target
        # (archive without MCPB) is not, a foreign file is not, and an empty
        # set is not.
        expect(
            root,
            1,
            "release target linux is half built: archive=missing, mcpb=present",
            **BETA_PARTIAL,
        )
        (root / "mcpbs" / "tracedecay-beta-v1.2.3-linux.mcpb").unlink()
        expect(
            root,
            0,
            "release artifact coverage is partial; missing targets: linux",
            **BETA_PARTIAL,
        )
        (root / "binaries" / "stray.tar.gz").write_bytes(b"artifact")
        expect(root, 1, "unexpected release artifacts: stray.tar.gz", **BETA_PARTIAL)
        (root / "binaries" / "stray.tar.gz").unlink()
        (root / "binaries" / "tracedecay-beta-v1.2.3-windows.zip").unlink()
        (root / "mcpbs" / "tracedecay-beta-v1.2.3-windows.mcpb").unlink()
        expect(root, 1, "no release target is complete; nothing to publish", **BETA_PARTIAL)
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
        expect(
            root,
            1,
            "binary coverage mismatch: unexpected tracedecay-beta-v1.2.3-linux.mcpb",
            profile="beta",
        )
        expect(
            root,
            1,
            "unexpected release artifacts: tracedecay-beta-v1.2.3-linux.mcpb",
            **BETA_PARTIAL,
        )
    print("release artifact validator tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
