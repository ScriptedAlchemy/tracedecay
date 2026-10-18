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


def invoke(
    root: Path, assets: tuple[str, ...], profile: str
) -> subprocess.CompletedProcess[str]:
    (root / "assets").write_text("\n".join(assets), encoding="utf-8")
    for output in ("github-output", "retained"):
        (root / output).unlink(missing_ok=True)
    return subprocess.run(
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
            str(root / "retained"),
            "--github-output",
            str(root / "github-output"),
        ],
        capture_output=True,
        text=True,
    )


def run(
    root: Path, assets: tuple[str, ...], profile: str = "stable"
) -> tuple[str, dict[str, object], list[str]]:
    completed = invoke(root, assets, profile)
    if completed.returncode != 0:
        raise AssertionError(completed.stdout + completed.stderr)
    outputs = dict(
        line.split("=", 1)
        for line in (root / "github-output").read_text(encoding="utf-8").splitlines()
    )
    retained = (root / "retained").read_text(encoding="utf-8").splitlines()
    return outputs["build_required"], json.loads(outputs["matrix"]), retained


def refuses(
    root: Path, assets: tuple[str, ...], reason: str, profile: str = "stable"
) -> None:
    completed = invoke(root, assets, profile)
    if completed.returncode != 1 or completed.stderr != f"{reason}\n":
        raise AssertionError(completed.stdout + completed.stderr)
    if (root / "github-output").exists() or (root / "retained").exists():
        raise AssertionError("a refused plan still wrote release outputs")


def premature_metadata(asset: str) -> str:
    return (
        f"final release metadata exists before all immutable artifacts ({asset}); "
        "refusing destructive recovery"
    )


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "targets.json").write_text(json.dumps(TARGETS), encoding="utf-8")

        build_required, matrix, retained = run(root, ())
        assert build_required == "true"
        assert matrix == TARGETS
        assert retained == []

        linux_binary = "tracedecay-v1.2.3-linux.tar.gz"
        linux_mcpb = "tracedecay-v1.2.3-linux.mcpb"
        build_required, matrix, retained = run(root, (linux_binary,))
        assert build_required == "true"
        assert matrix == TARGETS
        assert retained == [linux_binary]

        build_required, matrix, retained = run(root, (linux_binary, linux_mcpb))
        assert build_required == "true"
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
        build_required, matrix, retained = run(root, stable_assets)
        assert build_required == "false"
        assert matrix == {"include": []}
        assert retained == [
            linux_mcpb,
            linux_binary,
            "tracedecay-v1.2.3-windows.mcpb",
            "tracedecay-v1.2.3-windows.zip",
        ]

        refuses(root, (linux_binary, "SHA256SUMS"), premature_metadata("SHA256SUMS"))
        refuses(root, (linux_binary, "install.sh"), premature_metadata("install.sh"))
        refuses(
            root, ("unexpected.tar.gz",), "unexpected existing release assets: unexpected.tar.gz"
        )

        beta_linux = "tracedecay-beta-v1.2.3-linux.tar.gz"
        beta_linux_mcpb = "tracedecay-beta-v1.2.3-linux.mcpb"
        build_required, matrix, retained = run(
            root,
            (beta_linux, beta_linux_mcpb),
            profile="beta",
        )
        assert build_required == "true"
        assert matrix == {"include": [TARGETS["include"][1]]}
        assert retained == sorted((beta_linux, beta_linux_mcpb))

        beta_assets = (
            beta_linux,
            beta_linux_mcpb,
            "tracedecay-beta-v1.2.3-windows.zip",
            "tracedecay-beta-v1.2.3-windows.mcpb",
            "SHA256SUMS",
            "install.sh",
        )
        build_required, matrix, retained = run(root, beta_assets, profile="beta")
        assert build_required == "false"
        assert matrix == {"include": []}
        assert retained == [
            beta_linux_mcpb,
            beta_linux,
            "tracedecay-beta-v1.2.3-windows.mcpb",
            "tracedecay-beta-v1.2.3-windows.zip",
        ]

        refuses(root, (beta_linux, "install.sh"), premature_metadata("install.sh"), profile="beta")

    print("release recovery planner tests passed")


if __name__ == "__main__":
    main()
