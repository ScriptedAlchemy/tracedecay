#!/usr/bin/env python3
"""Behavioral tests for release component inventory scope and artifact digests."""

from __future__ import annotations

import importlib.util
import json
import tarfile
import tempfile
import zipfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "release_inventory", Path(__file__).with_name("generate-release-inventory.py")
)
assert SPEC is not None and SPEC.loader is not None
INVENTORY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INVENTORY)


def write_package(path: Path, name: str, version: str, license_value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f'[package]\nname = "{name}"\nversion = "{version}"\nlicense = "{license_value}"\n',
        encoding="utf-8",
    )


def make_archive(path: Path, payload: bytes) -> None:
    with tarfile.open(path, "w:gz") as archive:
        entry = tarfile.TarInfo("tracedecay")
        entry.size = len(payload)
        archive.addfile(entry, __import__("io").BytesIO(payload))


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        source = root / "source"
        output_base = root / "bazel"
        dashboard = root / "app-dist"
        dashboard.mkdir(parents=True)
        (dashboard / "index.js").write_bytes(b"bundled npm")
        (dashboard / "archivo-latin.woff2").write_bytes(b"font bytes")
        (source / "crates/tracedecay-cli").mkdir(parents=True)
        (source / "crates/tracedecay").mkdir(parents=True)
        (source / "patches/tokensave-medium-treesitters").mkdir(parents=True)
        (source / "scripts/bazel/patches").mkdir(parents=True)
        (source / "scripts/bazel/patches/rust.patch").write_text("adaptation\n")
        (source / "MODULE.bazel").write_text(
            'patches = ["//scripts:bazel/patches/rust.patch"]\n', encoding="utf-8"
        )
        write_package(
            source / "crates/tracedecay-cli/Cargo.toml",
            "tracedecay-cli",
            "1.2.3",
            "MIT",
        )
        write_package(source / "crates/tracedecay/Cargo.toml", "tracedecay", "1.2.3", "MIT")
        write_package(
            source / "patches/tokensave-medium-treesitters/Cargo.toml",
            "tokensave-medium-treesitters",
            "0.1.0",
            "NOASSERTION",
        )
        (source / "Cargo.lock").write_text(
            """version = 4

[[package]]
name = "tracedecay-cli"
version = "1.2.3"
dependencies = ["tracedecay"]

[[package]]
name = "tracedecay"
version = "1.2.3"
dependencies = ["asupersync 0.6.0 (git+https://github.com/vendor/asupersync?rev=abc#abc)", "serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "asupersync"
version = "0.6.0"
source = "git+https://github.com/vendor/asupersync?rev=abc#abc"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0123456789abcdef"

[[package]]
name = "tokensave-medium-treesitters"
version = "0.1.0"
""",
            encoding="utf-8",
        )
        (output_base / "external/c__asupersync-0.6.0").mkdir(parents=True)
        (output_base / "external/c__serde-1.0.0").mkdir(parents=True)
        write_package(
            output_base / "external/c__asupersync-0.6.0/Cargo.toml",
            "asupersync",
            "0.6.0",
            "MIT OR Apache-2.0",
        )
        write_package(
            output_base / "external/c__serde-1.0.0/Cargo.toml",
            "serde",
            "1.0.0",
            "MIT OR Apache-2.0",
        )
        graph = root / "graph.dot"
        graph.write_text(
            '\n'.join(
                (
                    '"//crates/tracedecay-cli:tracedecay" -> "//crates/tracedecay:tracedecay";',
                    '"//crates/tracedecay:tracedecay" -> "@@c__serde-1.0.0//:serde";',
                    '"//crates/tracedecay:tracedecay" -> "@@c__asupersync-0.6.0//:asupersync";',
                    '"//crates/tracedecay:tracedecay" -> "//patches/tokensave-medium-treesitters:tokensave-medium-treesitters";',
                    '"//crates/tracedecay:tracedecay" -> "//scripts:build_script_build";',
                    '"//scripts:build_script_build" -> "@@c__build-tool-1.0.0//:build_tool";',
                )
            ),
            encoding="utf-8",
        )
        kinds = root / "kinds.txt"
        kinds.write_text(
            """rust_binary rule //crates/tracedecay-cli:tracedecay
rust_library rule //crates/tracedecay:tracedecay
rust_library rule @@c__serde-1.0.0//:serde
rust_library rule @@c__asupersync-0.6.0//:asupersync
rust_library rule //patches/tokensave-medium-treesitters:tokensave-medium-treesitters
cargo_build_script rule //scripts:build_script_build
rust_library rule @@c__build-tool-1.0.0//:build_tool
""",
            encoding="utf-8",
        )
        scope = root / "scope.json"
        INVENTORY.scope_inventory(
            source,
            scope,
            "x86_64-linux",
            "a" * 40,
            dashboard,
            graph,
            kinds,
            output_base,
        )
        scope_bytes = scope.read_bytes()
        assert scope_bytes == INVENTORY.canonical_json(json.loads(scope_bytes))
        scoped = json.loads(scope_bytes)
        rust = {component["name"]: component for component in scoped["rust_components"]}
        assert {"tracedecay", "serde", "asupersync", "tokensave-medium-treesitters"} <= set(rust)
        assert "build-tool" not in rust
        assert "https://github.com/vendor/asupersync" in rust["asupersync"]["downloadLocation"]
        assert scoped["adapted_sources"][0]["path"] == "scripts/bazel/patches/rust.patch"

        (source / "pnpm-lock.yaml").write_text(
            """lockfileVersion: '9.0'
importers:
  dashboard:
    dependencies:
      '@fontsource-variable/archivo':
        version: 5.3.0
      react:
        version: 19.0.0
    devDependencies:
      playwright:
        version: 1.0.0
packages:
  '@fontsource-variable/archivo@5.3.0':
    resolution:
      integrity: sha512-YWJj
  react@19.0.0:
    resolution:
      integrity: sha512-cmVhY3Q=
  playwright@1.0.0:
    resolution:
      integrity: sha512-cGxheXdyaWdodA==
snapshots:
  '@fontsource-variable/archivo@5.3.0': {}
  react@19.0.0: {}
  playwright@1.0.0: {}
""",
            encoding="utf-8",
        )
        binary = b"release-binary"
        archive = root / "tracedecay-v1.2.3-x86_64-linux.tar.gz"
        make_archive(archive, binary)
        mcpb = root / "tracedecay-v1.2.3-x86_64-linux.mcpb"
        with zipfile.ZipFile(mcpb, "w") as bundle:
            bundle.writestr("manifest.json", "{}")
            bundle.writestr("server/tracedecay", binary)
        inventory = root / "inventory.spdx.json"
        arguments = (
            source,
            scope,
            archive,
            mcpb,
            "v1.2.3",
            "a" * 40,
            1_700_000_000,
            "ScriptedAlchemy/tracedecay",
            inventory,
        )
        INVENTORY.release_inventory(*arguments)
        first = inventory.read_bytes()
        INVENTORY.release_inventory(*arguments)
        assert inventory.read_bytes() == first

        document = json.loads(first)
        assert document["spdxVersion"] == "SPDX-2.3"
        packages = {package["name"]: package for package in document["packages"]}
        assert "react" in packages
        assert "@fontsource-variable/archivo" in packages
        assert "playwright" not in packages
        assert packages["asupersync"]["sourceInfo"].startswith("git+https://github.com/vendor/")
        assert packages["tokensave-medium-treesitters"]["licenseDeclared"] == "NOASSERTION"
        assert any("tokensave-medium-treesitters" in item["comment"] for item in document["annotations"])
        assert any(item["fileName"].endswith("archivo-latin.woff2") for item in document["files"])
        artifact_checksums = {
            package["name"]: package["checksums"][0]["checksumValue"]
            for package in document["packages"]
            if package["SPDXID"] in {"SPDXRef-Archive", "SPDXRef-MCPB"}
        }
        assert artifact_checksums[archive.name] == INVENTORY.sha256(archive)
        assert artifact_checksums[mcpb.name] == INVENTORY.sha256(mcpb)

        (dashboard / "index.js").write_bytes(b"changed bundled npm")
        INVENTORY.scope_inventory(
            source,
            scope,
            "x86_64-linux",
            "a" * 40,
            dashboard,
            graph,
            kinds,
            output_base,
        )
        assert scope.read_bytes() != scope_bytes

    print("release component inventory tests passed")


if __name__ == "__main__":
    main()
