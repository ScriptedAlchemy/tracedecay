#!/usr/bin/env python3
import argparse
import json
import stat
import zipfile
from pathlib import Path


PLATFORMS = {
    "aarch64-macos": ("darwin", "tracedecay"),
    "x86_64-linux": ("linux", "tracedecay"),
    "aarch64-linux": ("linux", "tracedecay"),
    "x86_64-windows": ("win32", "tracedecay.exe"),
}
ZIP_TIMESTAMP = (2025, 1, 1, 0, 0, 0)


def fail(message: str) -> None:
    raise RuntimeError(f"MCPB acceptance: {message}")


def manifest(version: str, platform: str) -> dict[str, object]:
    try:
        compatibility, binary_name = PLATFORMS[platform]
    except KeyError:
        fail(f"unsupported release platform {platform!r}")
    entry_point = f"server/{binary_name}"
    return {
        "manifest_version": "0.3",
        "name": "tracedecay",
        "version": version,
        "description": "Semantic code intelligence for 50+ languages through 164 MCP tools.",
        "author": {
            "name": "ScriptedAlchemy",
            "url": "https://github.com/ScriptedAlchemy",
        },
        "repository": "https://github.com/ScriptedAlchemy/tracedecay",
        "homepage": "https://github.com/ScriptedAlchemy/tracedecay",
        "license": "MIT",
        "tools_generated": True,
        "server": {
            "type": "binary",
            "entry_point": entry_point,
            "mcp_config": {
                "command": f"${{__dirname}}/{entry_point}",
                "args": ["serve"],
                "env": {},
            },
        },
        "compatibility": {"platforms": [compatibility]},
    }


def archive_entry(name: str, *, executable: bool = False) -> zipfile.ZipInfo:
    entry = zipfile.ZipInfo(name, ZIP_TIMESTAMP)
    mode = stat.S_IFREG | (0o755 if executable else 0o644)
    entry.external_attr = mode << 16
    entry.compress_type = zipfile.ZIP_DEFLATED
    return entry


def build_bundle(binary: Path, output: Path, version: str, platform: str) -> None:
    if not binary.is_file() or binary.stat().st_size == 0:
        fail(f"release binary is missing or empty: {binary}")
    try:
        _, binary_name = PLATFORMS[platform]
    except KeyError:
        fail(f"unsupported release platform {platform!r}")
    if binary.name != binary_name:
        fail(f"{platform} bundle requires binary named {binary_name!r}")
    output.parent.mkdir(parents=True, exist_ok=True)
    rendered_manifest = (
        json.dumps(manifest(version, platform), indent=2, sort_keys=True) + "\n"
    ).encode()
    with zipfile.ZipFile(output, "w") as archive:
        archive.writestr(archive_entry("manifest.json"), rendered_manifest)
        archive.writestr(
            archive_entry(f"server/{binary_name}", executable=True),
            binary.read_bytes(),
        )
    verify_bundle(output, version, platform)


def verify_bundle(bundle: Path, version: str, platform: str) -> None:
    if not bundle.is_file() or bundle.stat().st_size == 0:
        fail(f"bundle is missing or empty: {bundle}")
    try:
        _, binary_name = PLATFORMS[platform]
    except KeyError:
        fail(f"unsupported release platform {platform!r}")
    with zipfile.ZipFile(bundle) as archive:
        expected = {"manifest.json", f"server/{binary_name}"}
        if set(archive.namelist()) != expected:
            fail(f"{bundle} inventory is not exactly {sorted(expected)}")
        packaged_binary = archive.read(f"server/{binary_name}")
        if not packaged_binary:
            fail(f"{bundle} contains an empty server binary")
        packaged_manifest = json.loads(archive.read("manifest.json"))
    if packaged_manifest != manifest(version, platform):
        fail(f"{bundle} manifest does not match canonical release metadata")


def main() -> None:
    parser = argparse.ArgumentParser()
    subcommands = parser.add_subparsers(dest="action", required=True)
    build = subcommands.add_parser("build")
    build.add_argument("--binary", required=True, type=Path)
    build.add_argument("--output", required=True, type=Path)
    build.add_argument("--version", required=True)
    build.add_argument("--platform", required=True, choices=sorted(PLATFORMS))
    verify = subcommands.add_parser("verify")
    verify.add_argument("--bundle", required=True, type=Path)
    verify.add_argument("--version", required=True)
    verify.add_argument("--platform", required=True, choices=sorted(PLATFORMS))
    arguments = parser.parse_args()
    if arguments.action == "build":
        build_bundle(
            arguments.binary,
            arguments.output,
            arguments.version,
            arguments.platform,
        )
    else:
        verify_bundle(arguments.bundle, arguments.version, arguments.platform)


if __name__ == "__main__":
    main()
