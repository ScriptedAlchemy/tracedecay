#!/usr/bin/env python3
"""Regression tests for historical-tag release profile resolution."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import subprocess
import sys
import tempfile


RESOLVER = Path(__file__).with_name("resolve-release-source-profile.py")


@dataclass(frozen=True)
class FixtureResult:
    returncode: int
    stdout: str
    stderr: str
    github_output: str


def run_fixture(manifest: str) -> FixtureResult:
    with tempfile.TemporaryDirectory() as temporary_directory:
        source = Path(temporary_directory)
        # The resolver reads the product package manifest, not the workspace
        # root: `crates/tracedecay/Cargo.toml` is where the feature table lives.
        product = source.joinpath("crates", "tracedecay")
        product.joinpath("src").mkdir(parents=True)
        product.joinpath("Cargo.toml").write_text(manifest, encoding="utf-8")
        product.joinpath("src", "lib.rs").write_text("", encoding="utf-8")
        # The resolver walks the resolved dependency graph with `cargo tree
        # --locked`, so the fixture is a real workspace with a lockfile. It
        # has no dependencies, so the lockfile resolves offline.
        source.joinpath("Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/tracedecay"]\nresolver = "2"\n',
            encoding="utf-8",
        )
        subprocess.run(
            ["cargo", "generate-lockfile", "--offline"],
            cwd=source,
            check=True,
            capture_output=True,
        )
        output = source / "github-output.txt"
        completed = subprocess.run(
            [
                sys.executable,
                str(RESOLVER),
                "--source",
                str(source),
                "--github-output",
                str(output),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        return FixtureResult(
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            github_output=output.read_text(encoding="utf-8") if output.exists() else "",
        )


def main() -> int:
    production = run_fixture(
        """[package]
name = "tracedecay"
version = "0.1.0"
edition = "2024"

[features]
default = ["production"]
production = ["token-counting", "lite", "full"]
token-counting = []
lite = []
full = []
"""
    )
    if production.returncode != 0:
        raise SystemExit(production.stderr)
    if production.github_output != (
        "profile=production\n"
        "cargo_args=--no-default-features --features production\n"
        "cargo_features=production\n"
    ):
        raise SystemExit(
            f"unexpected production profile output: {production.github_output!r}"
        )

    contaminated_production = run_fixture(
        """[package]
name = "tracedecay"
version = "0.1.0"
edition = "2024"

[features]
default = ["production"]
production = ["token-counting", "lite", "full", "test-transport"]
token-counting = []
lite = []
full = []
test-transport = []
"""
    )
    if contaminated_production.returncode == 0:
        raise SystemExit("production test-transport contamination was accepted")

    legacy = run_fixture(
        """[package]
name = "tracedecay"
version = "0.0.67"
edition = "2021"

[features]
default = ["full", "token-counting"]
full = ["medium"]
medium = []
token-counting = []
test-transport = []
"""
    )
    if legacy.returncode != 0:
        raise SystemExit(legacy.stderr)
    if legacy.github_output != (
        "profile=legacy-default\ncargo_args=\ncargo_features=\n"
    ):
        raise SystemExit(
            f"unexpected historical profile output: {legacy.github_output!r}"
        )

    contaminated = run_fixture(
        """[package]
name = "tracedecay"
version = "0.0.1"
edition = "2021"

[features]
default = ["full"]
full = ["test-transport"]
test-transport = []
"""
    )
    if contaminated.returncode == 0:
        raise SystemExit("historical default test-transport contamination was accepted")
    if "default features enable test-transport" not in contaminated.stderr:
        raise SystemExit("historical contamination failed for an unexpected reason")

    print("historical release source profile fixtures passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
