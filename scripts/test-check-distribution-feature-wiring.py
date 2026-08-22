#!/usr/bin/env python3
"""Regression tests for distribution feature ownership validation."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import subprocess
import sys
import tempfile


VALIDATOR = Path(__file__).with_name("check-distribution-feature-wiring.py")

ROOT_MANIFEST = """[package]
name = "tracedecay"
version = "0.1.0-beta.34"

[dependencies]
tracedecay-semantic = { version = "0.1.0" }
tracedecay-usecases = { version = "0.1.0" }

[features]
full = []
token-counting = []
test-transport = []
semantic-fastembed = [
    "tracedecay-semantic/semantic-fastembed",
    "tracedecay-usecases/semantic-fastembed",
]
"""

SEMANTIC_MANIFEST = """[package]
name = "tracedecay-semantic"
version = "0.1.0"

[dependencies]
fastembed = { version = "=5.17.3", optional = true, default-features = false }
hf-hub = { version = "0.5", optional = true, default-features = false }

[features]
semantic-fastembed = [
    "dep:fastembed",
    "dep:hf-hub",
    "fastembed/ort-download-binaries-rustls-tls",
    "fastembed/hf-hub-rustls-tls",
]
"""


@dataclass(frozen=True)
class FixtureResult:
    returncode: int
    stdout: str
    stderr: str


def run_fixture(
    root_source: str = ROOT_MANIFEST,
    root_packaged: str = ROOT_MANIFEST,
    semantic_source: str = SEMANTIC_MANIFEST,
    semantic_packaged: str = SEMANTIC_MANIFEST,
) -> FixtureResult:
    with tempfile.TemporaryDirectory() as temporary_directory:
        root = Path(temporary_directory)
        manifests = {
            "root-source.toml": root_source,
            "root-packaged.toml": root_packaged,
            "semantic-source.toml": semantic_source,
            "semantic-packaged.toml": semantic_packaged,
        }
        for name, contents in manifests.items():
            root.joinpath(name).write_text(contents, encoding="utf-8")
        completed = subprocess.run(
            [
                sys.executable,
                str(VALIDATOR),
                "--root-source",
                str(root / "root-source.toml"),
                "--root-packaged",
                str(root / "root-packaged.toml"),
                "--semantic-source",
                str(root / "semantic-source.toml"),
                "--semantic-packaged",
                str(root / "semantic-packaged.toml"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        return FixtureResult(completed.returncode, completed.stdout, completed.stderr)


def main() -> int:
    extracted_owner = run_fixture()
    if extracted_owner.returncode != 0:
        raise SystemExit(extracted_owner.stderr)

    semantic_without_runtime = SEMANTIC_MANIFEST.replace(
        '    "fastembed/ort-download-binaries-rustls-tls",\n', ""
    )
    missing_runtime = run_fixture(
        semantic_source=semantic_without_runtime,
        semantic_packaged=semantic_without_runtime,
    )
    if missing_runtime.returncode == 0:
        raise SystemExit("semantic owner without the bundled ORT feature was accepted")
    if "tracedecay-semantic semantic-fastembed must enable" not in missing_runtime.stderr:
        raise SystemExit("missing semantic runtime failed for an unexpected reason")

    root_with_direct_owner = ROOT_MANIFEST.replace(
        '    "tracedecay-semantic/semantic-fastembed",\n', '    "dep:fastembed",\n'
    )
    root_direct_owner = run_fixture(
        root_source=root_with_direct_owner,
        root_packaged=root_with_direct_owner,
    )
    if root_direct_owner.returncode == 0:
        raise SystemExit("root package reclaiming FastEmbed ownership was accepted")
    if "root semantic-fastembed must forward" not in root_direct_owner.stderr:
        raise SystemExit("root ownership drift failed for an unexpected reason")

    root_with_shadow_owner = ROOT_MANIFEST.replace(
        "[dependencies]\n",
        "[dependencies]\nfastembed = { version = \"=5.17.3\", optional = true }\n",
    ).replace(
        'semantic-fastembed = [\n',
        'semantic-fastembed = [\n    "dep:fastembed",\n',
    )
    root_shadow_owner = run_fixture(
        root_source=root_with_shadow_owner,
        root_packaged=root_with_shadow_owner,
    )
    if root_shadow_owner.returncode == 0:
        raise SystemExit("root package retaining shadow FastEmbed ownership was accepted")
    if "root package must not own fastembed" not in root_shadow_owner.stderr:
        raise SystemExit("root shadow ownership failed for an unexpected reason")

    root_with_aliased_shadow_owner = ROOT_MANIFEST.replace(
        "[dependencies]\n",
        "[dependencies]\nfastembed-shadow = { package = \"fastembed\", version = \"=5.17.3\", optional = true }\n",
    ).replace(
        "[features]\n",
        '[features]\nshadow-fastembed = ["dep:fastembed-shadow"]\n',
    )
    root_aliased_shadow_owner = run_fixture(
        root_source=root_with_aliased_shadow_owner,
        root_packaged=root_with_aliased_shadow_owner,
    )
    if root_aliased_shadow_owner.returncode == 0:
        raise SystemExit("renamed root FastEmbed dependency was accepted")
    if "root package must not own fastembed" not in root_aliased_shadow_owner.stderr:
        raise SystemExit("renamed root ownership failed for an unexpected reason")

    print("distribution feature wiring fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
