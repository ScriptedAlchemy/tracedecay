#!/usr/bin/env python3
"""Behavioral tests for Bazel feature aliases and test input ownership."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parent / "bazel" / "gen_builds.py"


def load_gen_builds():
    spec = importlib.util.spec_from_file_location("gen_builds", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class DefaultAliasNormalizationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.gen = load_gen_builds()

    def test_adds_default_when_implied_features_are_already_on(self) -> None:
        table = {
            "default": ["production"],
            "production": ["lite", "full", "token-counting"],
            "lite": [],
            "full": [],
            "token-counting": [],
        }
        feats = {"production", "lite", "full", "token-counting"}
        self.assertEqual(
            self.gen.normalize_default_alias(feats, table),
            {"default", "production", "lite", "full", "token-counting"},
        )

    def test_leaves_lean_sets_without_default(self) -> None:
        table = {
            "default": ["production"],
            "production": ["lite"],
            "lite": [],
        }
        self.assertEqual(self.gen.normalize_default_alias(set(), table), set())
        self.assertEqual(self.gen.normalize_default_alias({"lite"}, table), {"lite"})

    def test_empty_default_stays_off(self) -> None:
        table = {"default": []}
        self.assertEqual(
            self.gen.normalize_default_alias({"lite"}, table),
            {"lite"},
        )

    def test_preserves_default_dependency_edges(self) -> None:
        table = {
            "default": ["production", "dep:optional-dep", "other/feat"],
            "production": ["lite"],
            "lite": [],
        }
        self.assertEqual(
            self.gen.normalize_default_alias({"production", "lite"}, table),
            {"production", "lite"},
        )

    def test_collapses_local_alias_with_resolved_dependency_edges(self) -> None:
        table = {
            "default": ["production"],
            "production": ["dep:optional-dep", "other/feat"],
        }
        self.assertEqual(
            self.gen.normalize_default_alias({"production"}, table),
            {"default", "production"},
        )


class TestResourceOwnershipTests(unittest.TestCase):
    def test_suite_owns_its_inputs_and_explicit_shared_modules(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            for name in ("owned/main.rs", "owned/input.json", "other/main.rs",
                         "common/mod.rs", "fixtures/sample.rs"):
                path = root / "tests" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture")
            package = {"manifest_path": str(root / "Cargo.toml")}
            target = {"src_path": str(root / "tests/owned/main.rs")}
            patterns = gen.test_resource_globs(package, target, (root / "tests/common",))
            # Match Bazel's recursive file globs, including files below each directory.
            inputs = {path.relative_to(root).as_posix() for pattern in patterns
                      for path in root.glob(pattern + "/*") if path.is_file()}
            self.assertIn("tests/owned/input.json", inputs)
            self.assertIn("tests/common/mod.rs", inputs)
            self.assertIn("tests/fixtures/sample.rs", inputs)
            self.assertNotIn("tests/other/main.rs", inputs)
            sources = {path for path, _ in gen.target_sources(target)}
            self.assertEqual(sources, {root / "tests/owned/main.rs"})


class SharedPackageOwnershipTests(unittest.TestCase):
    def test_nested_package_owns_embedded_fixture(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            package = root / "evals/agent_adoption"
            package.mkdir(parents=True)
            (root / "evals/BUILD.bazel").write_text("# generated parent")
            for build_name in ("BUILD.bazel", "BUILD"):
                build = package / build_name
                build.write_text("# handwritten package")
                self.assertEqual(
                    gen.owner_pkg(Path("evals/agent_adoption/fixture/Cargo.lock"), root),
                    ("evals/agent_adoption", "fixture"),
                )
                self.assertEqual(
                    gen.owner_pkg(Path("evals/agent_adoption/input.json"), root),
                    ("evals/agent_adoption", ""),
                )
                build.unlink()

    def test_unowned_fixtures_keep_shared_package_layout(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            self.assertEqual(
                gen.owner_pkg(Path("tests/fixtures/example.json"), root),
                ("tests", "fixtures"),
            )
            self.assertEqual(
                gen.owner_pkg(Path("crates/example/tests/input.json"), root),
                ("crates/example", "tests"),
            )


if __name__ == "__main__":
    unittest.main()
