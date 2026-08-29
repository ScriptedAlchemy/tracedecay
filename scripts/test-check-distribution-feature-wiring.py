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
lite = ["tracedecay-code-index/lite", "tracedecay-code-index-runtime/lite"]
medium = ["tracedecay-code-index/medium"]
full = ["tracedecay-code-index/full", "tracedecay-code-index-runtime/full"]
hotpath = []
hotpath-alloc = ["hotpath"]
hotpath-cpu = ["hotpath"]
hotpath-mcp = ["hotpath"]
lang-dart = ["tracedecay-code-index/lang-dart"]
lang-markdown = ["tracedecay-code-index/lang-markdown"]
token-counting = []
test-transport = []
semantic-fastembed = [
    "tracedecay-semantic/semantic-fastembed",
    "tracedecay-usecases/semantic-fastembed",
    "tracedecay-code-index-runtime/semantic-fastembed",
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

EXTRACTION_BUILD_MANIFEST = """[package]
name = "tracedecay-code-extraction"
version = "0.1.0"
edition = "2024"

[features]
default = []
lang-dart = ["dep:dart-grammar"]
lang-markdown = ["dep:markdown-grammar"]

[dependencies]
dart-grammar = { path = "dart-grammar", optional = true }
markdown-grammar = { path = "markdown-grammar", optional = true }
"""

EXTRACTION_BUILD_LIB = """#[cfg(feature = "lang-dart")]
pub fn dart() -> dart_grammar::Language {
    dart_grammar::language()
}

#[cfg(feature = "lang-markdown")]
pub fn markdown() -> markdown_grammar::Language {
    markdown_grammar::language()
}
"""

GRAMMAR_MANIFEST = """[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[lib]
path = "lib.rs"
"""

GRAMMAR_LIB = """pub struct Language;

pub fn language() -> Language {
    Language
}
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

CLI_MANIFEST = """[package]
name = "tracedecay-cli"
version = "0.1.0"

[dependencies]
hotpath = { version = "0.24", optional = true }
regex = { version = "1", optional = true }
tracedecay = { version = "0.1.0" }

[features]
default = ["production"]
production = ["tracedecay/production"]
hotpath = [
    "dep:regex",
    "tracedecay/hotpath",
    "hotpath/hotpath",
    "hotpath/tokio",
    "hotpath/axum-0-8",
    "hotpath/ureq-3",
]
hotpath-alloc = [
    "hotpath",
    "tracedecay/hotpath-alloc",
    "hotpath/hotpath-alloc",
]
hotpath-cpu = [
    "hotpath",
    "tracedecay/hotpath-cpu",
    "hotpath/hotpath-cpu",
]
hotpath-mcp = ["hotpath", "hotpath/hotpath-mcp"]
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
    cli_source: str = CLI_MANIFEST,
    cli_packaged: str = CLI_MANIFEST,
    extraction_build_manifest: str | None = None,
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
            "cli-source.toml": cli_source,
            "cli-packaged.toml": cli_packaged,
        }
        for name, contents in manifests.items():
            root.joinpath(name).write_text(contents, encoding="utf-8")
        command = [
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
            "--cli-source",
            str(root / "cli-source.toml"),
            "--cli-packaged",
            str(root / "cli-packaged.toml"),
        ]
        if extraction_build_manifest is not None:
            extraction = root / "extraction-build"
            extraction.joinpath("src").mkdir(parents=True)
            extraction.joinpath("Cargo.toml").write_text(
                extraction_build_manifest, encoding="utf-8"
            )
            extraction.joinpath("src/lib.rs").write_text(
                EXTRACTION_BUILD_LIB, encoding="utf-8"
            )
            for grammar in ("dart-grammar", "markdown-grammar"):
                dependency = extraction / grammar
                dependency.mkdir()
                dependency.joinpath("Cargo.toml").write_text(
                    GRAMMAR_MANIFEST.format(name=grammar), encoding="utf-8"
                )
                dependency.joinpath("lib.rs").write_text(
                    GRAMMAR_LIB, encoding="utf-8"
                )
            command.extend(
                ["--check-extraction-manifest", str(extraction / "Cargo.toml")]
            )
        completed = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
        )
        return FixtureResult(completed.returncode, completed.stdout, completed.stderr)


def main() -> int:
    extracted_owner = run_fixture()
    if extracted_owner.returncode != 0:
        raise SystemExit(extracted_owner.stderr)

    cli_without_cpu = CLI_MANIFEST.replace(
        'hotpath-cpu = [\n    "hotpath",\n    "tracedecay/hotpath-cpu",\n    "hotpath/hotpath-cpu",\n]\n',
        "",
    )
    missing_cli_cpu = run_fixture(
        cli_source=cli_without_cpu,
        cli_packaged=cli_without_cpu,
    )
    if missing_cli_cpu.returncode == 0:
        raise SystemExit("CLI without the Hotpath CPU release feature was accepted")
    if "tracedecay-cli is missing required features" not in missing_cli_cpu.stderr:
        raise SystemExit("missing CLI CPU feature failed for an unexpected reason")

    cli_with_miswired_mcp = CLI_MANIFEST.replace(
        'hotpath-mcp = ["hotpath", "hotpath/hotpath-mcp"]',
        'hotpath-mcp = ["hotpath"]',
    )
    miswired_cli_mcp = run_fixture(
        cli_source=cli_with_miswired_mcp,
        cli_packaged=cli_with_miswired_mcp,
    )
    if miswired_cli_mcp.returncode == 0:
        raise SystemExit("CLI with a mountless Hotpath MCP feature was accepted")
    if "tracedecay-cli hotpath-mcp must enable" not in miswired_cli_mcp.stderr:
        raise SystemExit("miswired CLI MCP feature failed for an unexpected reason")

    root_without_runtime_lite = ROOT_MANIFEST.replace(
        'lite = ["tracedecay-code-index/lite", "tracedecay-code-index-runtime/lite"]',
        'lite = ["tracedecay-code-index/lite"]',
    )
    missing_runtime_lite = run_fixture(
        root_source=root_without_runtime_lite,
        root_packaged=root_without_runtime_lite,
    )
    if missing_runtime_lite.returncode == 0:
        raise SystemExit("root lite without code-index-runtime forwarding was accepted")
    if "root lite must forward only" not in missing_runtime_lite.stderr:
        raise SystemExit("missing runtime lite forward failed for an unexpected reason")

    root_with_local_tier_members = ROOT_MANIFEST.replace(
        'full = ["tracedecay-code-index/full", "tracedecay-code-index-runtime/full"]',
        'full = ["tracedecay-code-index/full", "tracedecay-code-index-runtime/full", "lang-markdown"]',
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

    isolated_languages = run_fixture(
        extraction_build_manifest=EXTRACTION_BUILD_MANIFEST
    )
    if isolated_languages.returncode != 0:
        raise SystemExit(isolated_languages.stderr)

    extraction_with_miswired_language = EXTRACTION_BUILD_MANIFEST.replace(
        'lang-dart = ["dep:dart-grammar"]',
        'lang-dart = ["dep:markdown-grammar"]',
    )
    miswired_language = run_fixture(
        extraction_build_manifest=extraction_with_miswired_language
    )
    if miswired_language.returncode == 0:
        raise SystemExit("miswired isolated extraction language was accepted")
    if "lang-dart does not compile in isolation" not in miswired_language.stderr:
        raise SystemExit("miswired extraction language failed for an unexpected reason")

    print("distribution feature wiring fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
