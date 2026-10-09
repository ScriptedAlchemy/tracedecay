#!/usr/bin/env python3
"""Behavioral tests for Bazel default-feature alias collapse."""

from __future__ import annotations

import importlib.util
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


if __name__ == "__main__":
    unittest.main()
