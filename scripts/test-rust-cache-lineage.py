#!/usr/bin/env python3
"""Behavioral tests for rust-cache key lineage and the workspace-root guard."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parent.parent
LINEAGE_PATH = ROOT / "scripts/rust_cache_lineage.py"
CHECKER_PATH = ROOT / "scripts/check-rust-cache-lineage.py"
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


class WorkspaceRootTests(unittest.TestCase):
    """Drive the checker against a scratch copy of ci.yml."""

    def setUp(self) -> None:
        self.checker = load_module(CHECKER_PATH, "check_rust_cache_lineage")
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.workflows = Path(scratch.name)
        shutil.copyfile(WORKFLOWS / "ci.yml", self.workflows / "ci.yml")
        self.checker.WORKFLOWS = self.workflows

    def _assert_rejected_after(self, old: str, new: str, reason: str) -> None:
        self.assertEqual(self.checker.main(), 0)
        path = self.workflows / "ci.yml"
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            self.checker.main()
        self.assertIn(reason, stderr.getvalue())

    def test_rejects_rooting_rust_cache_at_the_vendored_checkout(self) -> None:
        self._assert_rejected_after(
            "crates -> ../target\n",
            ". -> target\n",
            "ci.yml rust-cache step must list workspace root 'crates -> ../target'",
        )

    def test_rejects_dropping_the_lockfile_key_root(self) -> None:
        self._assert_rejected_after(
            "            . -> target/rust-cache-lockfile-key\n",
            "",
            "ci.yml rust-cache step must list workspace root "
            "'. -> target/rust-cache-lockfile-key'",
        )

    def test_step_split_ignores_sibling_steps(self) -> None:
        text = (
            "steps:\n"
            "      - uses: Swatinem/rust-cache@v2\n"
            "        with:\n"
            "          shared-key: a\n"
            "\n"
            "      - run: echo 'crates -> ../target'\n"
        )
        (step,) = self.checker.rust_cache_steps(text)
        self.assertIn("shared-key: a", step)
        self.assertNotIn("crates -> ../target", step)


if __name__ == "__main__":
    unittest.main()
