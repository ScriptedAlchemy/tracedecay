#!/usr/bin/env python3
"""Behavioral tests for rust-cache key lineage."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parent.parent
LINEAGE_PATH = ROOT / "scripts/rust_cache_lineage.py"


def load_module(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class RustCacheKeyLineageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.lineage = load_module(LINEAGE_PATH, "rust_cache_lineage")

    def test_exact_and_prefix_generations_share_a_lane(self) -> None:
        old = "v0-rust-ci-test-full-Linux-Linux-x64-a68045c9-26efddad"
        new = "v1-rust-ci-test-full-Linux-Linux-x64-a68045c9-6447760f"
        self.assertEqual(self.lineage.lineage_of(old), "ci-test-full-Linux-Linux-x64")
        self.assertEqual(self.lineage.lineage_of(new), "ci-test-full-Linux-Linux-x64")
        self.assertEqual(self.lineage.generation_of(old), 0)
        self.assertEqual(self.lineage.generation_of(new), 1)

    def test_distinct_shared_keys_are_distinct_lineages(self) -> None:
        self.assertNotEqual(
            self.lineage.lineage_of("v1-rust-ci-test-full-Linux-Linux-x64-a68045c9-6447760f"),
            self.lineage.lineage_of("v1-rust-ci-dev-Linux-X64-Linux-x64-a68045c9-6447760f"),
        )

    def test_non_rust_cache_keys_are_not_lineages(self) -> None:
        self.assertIsNone(
            self.lineage.lineage_of(
                "node-cache-Linux-x64-npm-972714cd765b98492fe55266d9c2802cf04ffeb75647975a1ea26a7d48e75edb"
            )
        )
        self.assertIsNone(self.lineage.lineage_of("v0-rust-slice-tests-Linux-x64-a68045c9"))
        self.assertEqual(
            self.lineage.lineage_of("v0-rust-slice-tests-Linux-x64-a68045c9-6447760f"),
            "slice-tests-Linux-x64",
        )


if __name__ == "__main__":
    unittest.main()
