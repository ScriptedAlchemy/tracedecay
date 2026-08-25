#!/usr/bin/env python3
"""Regression tests for historical-tag release profile resolution."""

from __future__ import annotations

from dataclasses import dataclass
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile


RESOLVER = Path(__file__).with_name("resolve-release-source-profile.py")


def load_resolver():
    spec = importlib.util.spec_from_file_location("release_profile_resolver", RESOLVER)
    if spec is None or spec.loader is None:
        raise SystemExit(f"could not load {RESOLVER}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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
        product.mkdir(parents=True)
        product.joinpath("Cargo.toml").write_text(manifest, encoding="utf-8")
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
    resolver = load_resolver()
    modern_features = {
        "production": [],
        "hotpath": [],
        "hotpath-alloc": [],
        "hotpath-cpu": [],
        "hotpath-mcp": [],
    }
    linux_features = resolver.production_release_features(
        modern_features, "x86_64-unknown-linux-gnu"
    )
    if linux_features != ("production",):
        raise SystemExit(f"unexpected Linux release features: {linux_features!r}")

    macos_features = resolver.production_release_features(
        modern_features, "aarch64-apple-darwin"
    )
    if macos_features != ("production",):
        raise SystemExit(f"unexpected macOS release features: {macos_features!r}")

    windows_features = resolver.production_release_features(
        modern_features, "x86_64-pc-windows-msvc"
    )
    if windows_features != ("production",):
        raise SystemExit(f"unexpected Windows release features: {windows_features!r}")

    historical_production = resolver.production_release_features(
        {"production": []}, None
    )
    if historical_production != ("production",):
        raise SystemExit(
            f"unexpected historical production features: {historical_production!r}"
        )

    partial_hotpath = resolver.production_release_features(
        {"production": [], "hotpath": []}, "x86_64-unknown-linux-gnu"
    )
    if partial_hotpath != ("production",):
        raise SystemExit(
            f"unexpected partial-Hotpath production features: {partial_hotpath!r}"
        )

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
