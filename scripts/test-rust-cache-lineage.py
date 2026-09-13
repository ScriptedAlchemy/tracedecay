#!/usr/bin/env python3
"""Behavioral tests for rust-cache restore lineage and prefix-restore drops."""

from __future__ import annotations

import importlib.util
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parent.parent
LINEAGE_PATH = ROOT / "scripts/rust_cache_lineage.py"
CHECKER_PATH = ROOT / "scripts/check-rust-cache-lineage.py"
DROP_SCRIPT = ROOT / "scripts/drop-prefix-restored-rust-artifacts.sh"
WORKFLOWS = ROOT / ".github/workflows"


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
        old = "v0-rust-hotpath-coverage-slice-Linux-x64-a68045c9-26efddad"
        new = "v1-rust-hotpath-coverage-slice-Linux-x64-a68045c9-6447760f"
        self.assertEqual(self.lineage.lineage_of(old), "hotpath-coverage-slice-Linux-x64")
        self.assertEqual(self.lineage.lineage_of(new), "hotpath-coverage-slice-Linux-x64")
        self.assertEqual(self.lineage.generation_of(old), 0)
        self.assertEqual(self.lineage.generation_of(new), 1)

    def test_distinct_shared_keys_are_distinct_lineages(self) -> None:
        self.assertNotEqual(
            self.lineage.lineage_of("v1-rust-hotpath-coverage-slice-Linux-x64-a68045c9-6447760f"),
            self.lineage.lineage_of("v1-rust-hotpath-profile-Linux-x64-a68045c9-6447760f"),
        )

    def test_non_rust_cache_keys_are_not_lineages(self) -> None:
        self.assertIsNone(
            self.lineage.lineage_of(
                "node-cache-Linux-x64-npm-972714cd765b98492fe55266d9c2802cf04ffeb75647975a1ea26a7d48e75edb"
            )
        )
        self.assertIsNone(self.lineage.lineage_of("v0-rust-slice-tests-Linux-x64-a68045c9"))


class DropPrefixRestoredArtifactsTests(unittest.TestCase):
    def _seed(self, directory: Path) -> Path:
        rmeta = directory / "debug" / "deps" / "libhotpath-abc123.rmeta"
        rmeta.parent.mkdir(parents=True)
        rmeta.write_bytes(b"rmeta")
        rmeta.chmod(0o444)
        self.assertFalse(os.access(rmeta, os.W_OK))
        return rmeta

    def test_exact_hit_keeps_read_only_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            target = Path(scratch) / "target"
            rmeta = self._seed(target)
            completed = subprocess.run(
                [str(DROP_SCRIPT), "true", str(target)],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertTrue(rmeta.is_file())
            self.assertFalse(os.access(rmeta, os.W_OK))
            self.assertEqual(stat.S_IMODE(rmeta.stat().st_mode), 0o444)
            self.assertIn("exact rust-cache hit", completed.stdout)

    def test_prefix_restore_drops_the_compiler_tree(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            target = Path(scratch) / "target"
            rmeta = self._seed(target)
            completed = subprocess.run(
                [str(DROP_SCRIPT), "false", str(target)],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertFalse(rmeta.exists())
            self.assertFalse(target.exists())
            self.assertIn("dropping prefix-restored", completed.stdout)

    def test_empty_cache_hit_is_treated_as_incompatible(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            target = Path(scratch) / "target"
            self._seed(target)
            subprocess.run([str(DROP_SCRIPT), "", str(target)], check=True, capture_output=True)
            self.assertFalse(target.exists())


class HostedWorkflowLineageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.checker = load_module(CHECKER_PATH, "check_rust_cache_lineage")

    def test_canonical_workflows_pass(self) -> None:
        self.assertEqual(self.checker.main(), 0)

    def _rewrite(self, filename: str, old: str, new: str) -> None:
        path = WORKFLOWS / filename
        text = path.read_text(encoding="utf-8")
        if old not in text:
            self.fail(f"{filename} does not contain {old!r}")
        mutated = text.replace(old, new, 1)
        self.assertNotEqual(mutated, text)
        path.write_text(mutated, encoding="utf-8")

    def test_rejects_restoring_the_v0_prefix(self) -> None:
        original = (WORKFLOWS / "hotpath-coverage.yml").read_text(encoding="utf-8")
        try:
            self._rewrite("hotpath-coverage.yml", "prefix-key: v1-rust", "prefix-key: v0-rust")
            with self.assertRaises(SystemExit):
                self.checker.main()
        finally:
            (WORKFLOWS / "hotpath-coverage.yml").write_text(original, encoding="utf-8")

    def test_rejects_dropping_the_prefix_restore_guard(self) -> None:
        path = WORKFLOWS / "hotpath-runtime-core.yml"
        original = path.read_text(encoding="utf-8")
        try:
            self._rewrite(
                "hotpath-runtime-core.yml",
                'scripts/drop-prefix-restored-rust-artifacts.sh "${{ steps.rust-cache.outputs.cache-hit }}"',
                "echo skip-drop",
            )
            with self.assertRaises(SystemExit):
                self.checker.main()
        finally:
            path.write_text(original, encoding="utf-8")

    def test_rejects_sharing_a_cache_lineage_across_lanes(self) -> None:
        path = WORKFLOWS / "hotpath-profile.yml"
        original = path.read_text(encoding="utf-8")
        try:
            self._rewrite(
                "hotpath-profile.yml",
                "shared-key: hotpath-profile",
                "shared-key: hotpath-runtime-core",
            )
            with self.assertRaises(SystemExit):
                self.checker.main()
        finally:
            path.write_text(original, encoding="utf-8")

    def test_rejects_base_profile_writing_into_the_restored_target(self) -> None:
        path = WORKFLOWS / "hotpath-profile.yml"
        original = path.read_text(encoding="utf-8")
        try:
            self._rewrite("hotpath-profile.yml", "CARGO_TARGET_DIR: target-base", "CARGO_TARGET_DIR: target")
            with self.assertRaises(SystemExit):
                self.checker.main()
        finally:
            path.write_text(original, encoding="utf-8")

    def test_rejects_leaving_incremental_compilation_on(self) -> None:
        path = WORKFLOWS / "hotpath-coverage.yml"
        original = path.read_text(encoding="utf-8")
        try:
            self._rewrite("hotpath-coverage.yml", 'CARGO_INCREMENTAL: "0"', 'CARGO_INCREMENTAL: "1"')
            with self.assertRaises(SystemExit):
                self.checker.main()
        finally:
            path.write_text(original, encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
