#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import zipfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("build-mcpb.py")
SPEC = importlib.util.spec_from_file_location("build_mcpb", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        for platform, (_, binary_name) in sorted(MODULE.PLATFORMS.items()):
            binary = root / platform / binary_name
            binary.parent.mkdir()
            payload = f"nonempty-{platform}-binary".encode()
            binary.write_bytes(payload)
            output = root / f"tracedecay-v0.0.67-{platform}.mcpb"

            MODULE.build_bundle(binary, output, "0.0.67", platform)
            MODULE.verify_bundle(output, "0.0.67", platform)
            try:
                MODULE.verify_bundle(output, "0.0.68", platform)
            except RuntimeError as error:
                assert "manifest does not match" in str(error)
            else:
                raise AssertionError("verify_bundle accepted a mismatched version")

            assert output.stat().st_size > len(payload)
            with zipfile.ZipFile(output) as archive:
                assert set(archive.namelist()) == {
                    "manifest.json",
                    f"server/{binary_name}",
                }
                assert archive.read(f"server/{binary_name}") == payload
                manifest = json.loads(archive.read("manifest.json"))
                assert manifest["server"]["type"] == "binary"
                assert manifest["server"]["entry_point"] == f"server/{binary_name}"
                assert manifest["server"]["mcp_config"]["args"] == ["serve"]
                assert manifest["tools_generated"] is True

    print("MCPB build acceptance passed")


if __name__ == "__main__":
    main()
