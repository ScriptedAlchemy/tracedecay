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
tracedecay-code-index = { version = "0.1.0" }
tracedecay-semantic = { version = "0.1.0" }
tracedecay-usecases = { version = "0.1.0" }

[features]
lite = ["tracedecay-code-index/lite"]
medium = ["tracedecay-code-index/medium"]
full = ["tracedecay-code-index/full"]
lang-dart = ["tracedecay-code-index/lang-dart"]
lang-markdown = ["tracedecay-code-index/lang-markdown"]
token-counting = []
test-transport = []
semantic-fastembed = [
    "tracedecay-semantic/semantic-fastembed",
    "tracedecay-usecases/semantic-fastembed",
]
"""

CODE_INDEX_MANIFEST = """[package]
name = "tracedecay-code-index"
version = "0.1.0"

[dependencies]
tracedecay-code-extraction = { version = "0.1.0" }

[features]
lite = ["tracedecay-code-extraction/lite", "lang-markdown"]
medium = ["tracedecay-code-extraction/medium"]
full = ["tracedecay-code-extraction/full", "lang-markdown"]
lang-dart = ["tracedecay-code-extraction/lang-dart"]
lang-markdown = ["tracedecay-code-extraction/lang-markdown"]
"""

EXTRACTION_MANIFEST = """[package]
name = "tracedecay-code-extraction"
version = "0.1.0"

[features]
lite = ["lang-markdown"]
medium = ["lang-dart"]
full = ["medium", "lang-markdown"]
lang-dart = []
lang-markdown = []
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
    code_index_source: str = CODE_INDEX_MANIFEST,
    code_index_packaged: str = CODE_INDEX_MANIFEST,
    extraction_source: str = EXTRACTION_MANIFEST,
    extraction_packaged: str = EXTRACTION_MANIFEST,
    semantic_source: str = SEMANTIC_MANIFEST,
    semantic_packaged: str = SEMANTIC_MANIFEST,
) -> FixtureResult:
    with tempfile.TemporaryDirectory() as temporary_directory:
        root = Path(temporary_directory)
        manifests = {
            "root-source.toml": root_source,
            "root-packaged.toml": root_packaged,
            "code-index-source.toml": code_index_source,
            "code-index-packaged.toml": code_index_packaged,
            "extraction-source.toml": extraction_source,
            "extraction-packaged.toml": extraction_packaged,
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
                "--code-index-source",
                str(root / "code-index-source.toml"),
                "--code-index-packaged",
                str(root / "code-index-packaged.toml"),
                "--extraction-source",
                str(root / "extraction-source.toml"),
                "--extraction-packaged",
                str(root / "extraction-packaged.toml"),
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

    root_with_local_tier_members = ROOT_MANIFEST.replace(
        'full = ["tracedecay-code-index/full"]',
        'full = ["tracedecay-code-index/full", "lang-markdown"]',
    )
    duplicated_tier = run_fixture(
        root_source=root_with_local_tier_members,
        root_packaged=root_with_local_tier_members,
    )
    if duplicated_tier.returncode == 0:
        raise SystemExit("root package duplicating extraction tier membership was accepted")
    if "root full must forward only" not in duplicated_tier.stderr:
        raise SystemExit("duplicated tier membership failed for an unexpected reason")

    code_index_with_missing_alias = CODE_INDEX_MANIFEST.replace(
        'lang-dart = ["tracedecay-code-extraction/lang-dart"]\n', ""
    )
    missing_alias = run_fixture(
        code_index_source=code_index_with_missing_alias,
        code_index_packaged=code_index_with_missing_alias,
    )
    if missing_alias.returncode == 0:
        raise SystemExit("code-index package missing a language alias was accepted")
    if "code-index language features differ" not in missing_alias.stderr:
        raise SystemExit("missing language alias failed for an unexpected reason")

    extraction_with_new_language = EXTRACTION_MANIFEST.replace(
        "lang-markdown = []\n", "lang-markdown = []\nlang-rustdoc = []\n"
    )
    unforwarded_language = run_fixture(
        extraction_source=extraction_with_new_language,
        extraction_packaged=extraction_with_new_language,
    )
    if unforwarded_language.returncode == 0:
        raise SystemExit("new extraction language without public aliases was accepted")
    if "root language features differ" not in unforwarded_language.stderr:
        raise SystemExit("unforwarded language failed for an unexpected reason")

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
