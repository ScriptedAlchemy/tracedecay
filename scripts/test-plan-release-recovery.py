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
    root: Path,
    assets: tuple[str, ...],
    profile: str,
    *,
    with_execution_manifest: bool = True,
) -> subprocess.CompletedProcess[str]:
    (root / "assets").write_text("\n".join(assets), encoding="utf-8")
    for output in ("github-output", "retained"):
        (root / output).unlink(missing_ok=True)
    execution_arguments = (
        ["--execution-manifest", str(root / "execution-targets.json")]
        if with_execution_manifest
        else []
    )
    return subprocess.run(
        [
            "python3",
            str(SCRIPT),
            "--manifest",
            str(root / "targets.json"),
            *execution_arguments,
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


def runner_authority_cases(root: Path) -> None:
    source = [
        {
            "name": "macos",
            "runner": "macos-14",
            "target": "aarch64-apple-darwin",
            "archive": "tar.gz",
        },
        *TARGETS["include"],
    ]
    current = [{**source[0], "runner": "macos-26"}, *source[1:]]

    def write_manifests(source_rows=source, current_rows=current) -> None:
        (root / "targets.json").write_text(
            json.dumps({"include": source_rows}), encoding="utf-8"
        )
        (root / "execution-targets.json").write_text(
            json.dumps({"include": current_rows}), encoding="utf-8"
        )

    write_manifests()
    source_bytes = (root / "targets.json").read_bytes()
    for profile, prefix in (("stable", "tracedecay"), ("beta", "tracedecay-beta")):
        build_required, matrix, retained = run(root, (), profile)
        assert build_required == "true"
        assert matrix == {"include": current}
        assert retained == []

        published_other_platforms = tuple(
            f"{prefix}-v1.2.3-{target['name']}.{extension}"
            for target in source[1:]
            for extension in (target["archive"], "mcpb", "spdx.json")
        )
        build_required, matrix, retained = run(root, published_other_platforms, profile)
        assert build_required == "true"
        assert matrix == {"include": [current[0]]}
        assert retained == sorted(published_other_platforms)

        mac_packages = (
            f"{prefix}-v1.2.3-macos.tar.gz",
            f"{prefix}-v1.2.3-macos.mcpb",
        )
        published_packages = (*published_other_platforms, *mac_packages)
        build_required, matrix, retained = run(root, published_packages, profile)
        assert build_required == "true"
        assert matrix == {"include": [{**current[0], "inventory_only": True}]}
        assert retained == sorted(published_packages)
        outputs = dict(
            line.split("=", 1)
            for line in (root / "github-output").read_text(encoding="utf-8").splitlines()
        )
        assert json.loads(outputs["release_matrix"]) == {"include": current}
        assert "package_build_required=false\n" in (
            root / "github-output"
        ).read_text(encoding="utf-8")

        # A completed historical roster needs no current execution entry.
        write_manifests(current_rows=current[1:])
        published = (*published_packages, f"{prefix}-v1.2.3-macos.spdx.json")
        build_required, matrix, retained = run(root, published, profile)
        assert build_required == "false"
        assert matrix == {"include": []}
        assert retained == sorted(published)
        outputs = dict(
            line.split("=", 1)
            for line in (root / "github-output").read_text(encoding="utf-8").splitlines()
        )
        assert json.loads(outputs["release_matrix"]) == {"include": current[1:]}
        write_manifests(current_rows=[{**current[0], "target": "x86_64-apple-darwin"}, *current[1:]])
        build_required, matrix, retained = run(root, published, profile)
        assert build_required == "false"
        assert matrix == {"include": []}
        write_manifests()

    # Later platforms cannot expand an immutable tag's artifact roster.
    added = {**current[1], "name": "future-linux"}
    write_manifests(current_rows=[*current, added])
    _, matrix, _ = run(root, ())
    assert matrix == {"include": current}
    assert (root / "targets.json").read_bytes() == source_bytes

    write_manifests(current_rows=current[1:])
    refuses(root, (), "no execution runner for release target: macos")
    for field, changed in (("target", "x86_64-apple-darwin"), ("archive", "zip")):
        write_manifests(current_rows=[{**current[0], field: changed}, *current[1:]])
        refuses(
            root,
            (),
            f"execution release target macos changes {field}: "
            f"{source[0][field]!r} != {changed!r}",
        )
    write_manifests(current_rows=[*current, current[0]])
    refuses(root, (), "duplicate release target: macos")
    write_manifests(source_rows=[*source, source[0]])
    refuses(root, (), "duplicate release target: macos")
    write_manifests(current_rows=[{**current[0], "runner": ""}, *current[1:]])
    refuses(root, (), "release target is missing required string fields")

    write_manifests()
    completed = invoke(root, (), "stable", with_execution_manifest=False)
    assert completed.returncode == 2
    assert "--execution-manifest" in completed.stderr
    assert not (root / "github-output").exists()
    assert not (root / "retained").exists()
    assert (root / "targets.json").read_bytes() == source_bytes


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "targets.json").write_text(json.dumps(TARGETS), encoding="utf-8")
        (root / "execution-targets.json").write_text(
            json.dumps(TARGETS), encoding="utf-8"
        )

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
        assert matrix == {
            "include": [
                {**TARGETS["include"][0], "inventory_only": True},
                TARGETS["include"][1],
            ]
        }
        assert retained == sorted((linux_binary, linux_mcpb))

        stable_assets = (
            linux_binary,
            linux_mcpb,
            "tracedecay-v1.2.3-linux.spdx.json",
            "tracedecay-v1.2.3-windows.zip",
            "tracedecay-v1.2.3-windows.mcpb",
            "tracedecay-v1.2.3-windows.spdx.json",
            "SHA256SUMS",
            "install.sh",
        )
        build_required, matrix, retained = run(root, stable_assets)
        assert build_required == "false"
        assert matrix == {"include": []}
        assert retained == sorted(
            (
                linux_mcpb,
                linux_binary,
                "tracedecay-v1.2.3-linux.spdx.json",
                "tracedecay-v1.2.3-windows.mcpb",
                "tracedecay-v1.2.3-windows.zip",
                "tracedecay-v1.2.3-windows.spdx.json",
            )
        )

        refuses(root, (linux_binary, "SHA256SUMS"), premature_metadata("SHA256SUMS"))
        refuses(root, (linux_binary, "install.sh"), premature_metadata("install.sh"))
        build_required, matrix, retained = run(
            root,
            (
                linux_binary,
                linux_mcpb,
                "tracedecay-v1.2.3-windows.zip",
                "tracedecay-v1.2.3-windows.mcpb",
                "SHA256SUMS",
                "install.sh",
            ),
        )
        assert build_required == "true"
        assert matrix == {
            "include": [
                {**TARGETS["include"][0], "inventory_only": True},
                {**TARGETS["include"][1], "inventory_only": True},
            ]
        }
        assert retained == sorted((linux_binary, linux_mcpb,
            "tracedecay-v1.2.3-windows.zip", "tracedecay-v1.2.3-windows.mcpb"))
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
        assert matrix == {
            "include": [
                {**TARGETS["include"][0], "inventory_only": True},
                TARGETS["include"][1],
            ]
        }
        assert retained == sorted((beta_linux, beta_linux_mcpb))

        beta_assets = (
            beta_linux,
            beta_linux_mcpb,
            "tracedecay-beta-v1.2.3-linux.spdx.json",
            "tracedecay-beta-v1.2.3-windows.zip",
            "tracedecay-beta-v1.2.3-windows.mcpb",
            "tracedecay-beta-v1.2.3-windows.spdx.json",
            "SHA256SUMS",
            "install.sh",
        )
        build_required, matrix, retained = run(root, beta_assets, profile="beta")
        assert build_required == "false"
        assert matrix == {"include": []}
        assert retained == sorted(
            (
                beta_linux_mcpb,
                beta_linux,
                "tracedecay-beta-v1.2.3-linux.spdx.json",
                "tracedecay-beta-v1.2.3-windows.mcpb",
                "tracedecay-beta-v1.2.3-windows.zip",
                "tracedecay-beta-v1.2.3-windows.spdx.json",
            )
        )

        refuses(root, (beta_linux, "install.sh"), premature_metadata("install.sh"), profile="beta")
        runner_authority_cases(root)

    print("release recovery planner tests passed")


if __name__ == "__main__":
    main()
