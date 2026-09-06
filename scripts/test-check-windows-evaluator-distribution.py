#!/usr/bin/env python3
"""Regression tests for Windows nextest evaluator distribution in ci.yml."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import ModuleType


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHECKER_PATH = REPOSITORY_ROOT / "scripts/check-windows-evaluator-distribution.py"
WORKFLOW_PATH = REPOSITORY_ROOT / ".github/workflows/ci.yml"


def load_checker() -> ModuleType:
    spec = importlib.util.spec_from_file_location(
        "windows_evaluator_distribution", CHECKER_PATH
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load policy checker from {CHECKER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WindowsEvaluatorDistributionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.checker = load_checker()
        self.workflow = WORKFLOW_PATH.read_text(encoding="utf-8")

    def assert_rejected(self, workflow: str) -> None:
        self.assertNotEqual(workflow, self.workflow, "mutation must change the workflow")
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "ci.yml"
            path.write_text(workflow, encoding="utf-8")
            self.checker.WORKFLOW_PATH = path
            with self.assertRaises(SystemExit):
                self.checker.main()

    def test_accepts_canonical_workflow(self) -> None:
        self.checker.WORKFLOW_PATH = WORKFLOW_PATH
        self.checker.main()

    def test_rejects_package_only_search_eval_build(self) -> None:
        mutated = self.workflow.replace(
            "      - name: Build workspace binaries for the Windows test archive\n"
            "        shell: pwsh\n"
            "        run: cargo build --workspace --bins --locked --features tracedecay/test-helpers\n",
            "      - name: Build workspace binaries for the Windows test archive\n"
            "        shell: pwsh\n"
            "        run: cargo build -p tracedecay-search-eval --locked --bins\n",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_missing_support_artifact_upload(self) -> None:
        mutated = self.workflow.replace("windows-evaluator-bins", "windows-nextest-archive")
        self.assert_rejected(mutated)

    def test_rejects_missing_shard_restore(self) -> None:
        mutated = self.workflow.replace(
            "package-windows-evaluator-bins.py restore",
            "echo skip-restore",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_missing_shard_preflight(self) -> None:
        mutated = self.workflow.replace(
            "package-windows-evaluator-bins.py preflight",
            "echo skip-preflight",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_unbound_search_eval_overrides(self) -> None:
        mutated = self.workflow.replace(
            "TRACEDECAY_SEARCH_EVAL_TEST_BIN",
            "TRACEDECAY_SEARCH_EVAL_MISSING_BIN",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_preflight_after_windows_tests(self) -> None:
        mutated = self.workflow.replace(
            "      - name: Preflight Windows evaluator binaries\n",
            "      - name: Preflight Windows evaluator binaries later\n",
            1,
        )
        # Keep the preflight command but move the named step after tests by
        # dropping the original step heading so order detection fails.
        self.assert_rejected(mutated)


if __name__ == "__main__":
    unittest.main()
