#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import tomllib
import zipfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("build-mcpb.py")
SPEC = importlib.util.spec_from_file_location("build_mcpb", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def main() -> None:
    repository = SCRIPT.parent.parent
    package_version = tomllib.loads(
        (repository / "Cargo.toml").read_text(encoding="utf-8")
    )["package"]["version"]
    registry = json.loads((repository / "server.json").read_text(encoding="utf-8"))
    assert registry["version"] == package_version
    assert "164 MCP tools" in registry["description"]
    assert "50+ languages" in registry["description"]

    release_config = json.loads(
        (repository / "release-please-config.json").read_text(encoding="utf-8")
    )
    assert {
        "type": "json",
        "path": "server.json",
        "jsonpath": "$.version",
    } in release_config["packages"]["."]["extra-files"]

    targets = json.loads(
        (repository / ".github/release-targets.json").read_text(encoding="utf-8")
    )
    assert {entry["name"] for entry in targets["include"]} == {
        "aarch64-macos",
        "x86_64-linux",
        "aarch64-linux",
        "x86_64-windows",
    }

    release = (repository / ".github/workflows/release.yml").read_text(encoding="utf-8")
    beta_release = (repository / ".github/workflows/release-beta.yml").read_text(
        encoding="utf-8"
    )
    for workflow in [release, beta_release]:
        assert "release-targets.json" in workflow
        assert "build-mcpb.py verify" in workflow
    assert "test -s" in release
    assert "test \"$(jq '.packages | length' server.json)\" -gt 0" in release
    assert 'test("^[0-9a-f]{64}$")' in release

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        for platform, binary_name in [
            ("x86_64-linux", "tracedecay"),
            ("x86_64-windows", "tracedecay.exe"),
        ]:
            binary = root / platform / binary_name
            binary.parent.mkdir()
            binary.write_bytes(b"nonempty-binary")
            output = root / f"tracedecay-v0.0.67-{platform}.mcpb"

            MODULE.build_bundle(binary, output, "0.0.67", platform)
            MODULE.verify_bundle(output, "0.0.67", platform)

            assert output.stat().st_size > len(b"nonempty-binary")
            with zipfile.ZipFile(output) as archive:
                assert set(archive.namelist()) == {
                    "manifest.json",
                    f"server/{binary_name}",
                }
                manifest = json.loads(archive.read("manifest.json"))
                assert manifest["server"]["type"] == "binary"
                assert manifest["server"]["entry_point"] == f"server/{binary_name}"
                assert manifest["server"]["mcp_config"]["args"] == ["serve"]
                assert manifest["tools_generated"] is True

    print("MCPB build acceptance passed")


if __name__ == "__main__":
    main()
