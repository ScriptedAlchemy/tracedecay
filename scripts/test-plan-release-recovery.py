#!/usr/bin/env python3
"""Behavioral tests for immutable release recovery planning."""

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("plan-release-recovery.py")
TARGETS = {
    "include": [
        {
            "name": "linux",
            "runner": "ubuntu",
            "target": "x86_64-linux",
            "archive": "tar.gz",
        },
        {
            "name": "windows",
            "runner": "windows",
            "target": "x86_64-windows",
            "archive": "zip",
        },
    ]
}


def run(
    root: Path,
    assets: tuple[str, ...],
    profile: str = "stable",
    success: bool = True,
) -> tuple[dict[str, object], list[str]]:
    (root / "assets").write_text("\n".join(assets), encoding="utf-8")
    github_output = root / "github-output"
    retained = root / "retained"
    completed = subprocess.run(
        [
            "python3",
            str(SCRIPT),
            "--manifest",
            str(root / "targets.json"),
            "--tag",
            "v1.2.3",
            "--profile",
            profile,
            "--asset-names",
            str(root / "assets"),
            "--retained-output",
            str(retained),
            "--github-output",
            str(github_output),
        ],
        capture_output=True,
        text=True,
    )
    if (completed.returncode == 0) != success:
        raise AssertionError(completed.stdout + completed.stderr)
    if not success:
        return {}, []
    outputs = dict(
        line.split("=", 1)
        for line in github_output.read_text(encoding="utf-8").splitlines()
    )
    matrix = json.loads(outputs["matrix"])
    expected_build = "true" if matrix["include"] else "false"
    assert outputs["build_required"] == expected_build
    return matrix, retained.read_text().splitlines()


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "targets.json").write_text(json.dumps(TARGETS), encoding="utf-8")

        matrix, retained = run(root, ())
        assert matrix == TARGETS
        assert retained == []

        linux_binary = "tracedecay-v1.2.3-linux.tar.gz"
        linux_mcpb = "tracedecay-v1.2.3-linux.mcpb"
        matrix, retained = run(root, (linux_binary,))
        assert matrix == TARGETS
        assert retained == [linux_binary]

        matrix, retained = run(root, (linux_binary, linux_mcpb))
        assert matrix == {"include": [TARGETS["include"][1]]}
        assert retained == sorted((linux_binary, linux_mcpb))

        stable_assets = (
            linux_binary,
            linux_mcpb,
            "tracedecay-v1.2.3-windows.zip",
            "tracedecay-v1.2.3-windows.mcpb",
            "SHA256SUMS",
            "install.sh",
        )
        matrix, retained = run(root, stable_assets)
        assert matrix == {"include": []}
        assert len(retained) == 4

        run(root, (linux_binary, "SHA256SUMS"), success=False)
        run(root, (linux_binary, "install.sh"), success=False)
        run(root, ("unexpected.tar.gz",), success=False)

        beta_linux = "tracedecay-beta-v1.2.3-linux.tar.gz"
        beta_linux_mcpb = "tracedecay-beta-v1.2.3-linux.mcpb"
        matrix, retained = run(
            root,
            (beta_linux, beta_linux_mcpb),
            profile="beta",
        )
        assert matrix == {"include": [TARGETS["include"][1]]}
        assert retained == sorted((beta_linux, beta_linux_mcpb))

    print("release recovery planner tests passed")


if __name__ == "__main__":
    main()
