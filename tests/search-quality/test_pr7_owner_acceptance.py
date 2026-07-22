#!/usr/bin/env python3
"""Tests for content-addressed PR7 owner acceptance."""

from __future__ import annotations

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
HARNESS = REPO / "benchmarks/pr7-memory/issue_receipt.py"


def load_harness():
    spec = importlib.util.spec_from_file_location("pr7_issue_receipt", HARNESS)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Pr7OwnerAcceptanceTest(unittest.TestCase):
    def test_snapshot_digest_binds_tracked_and_untracked_inputs(self) -> None:
        harness = load_harness()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            (root / "tracked.txt").write_text("tracked\n", encoding="utf-8")
            subprocess.run(["git", "add", "tracked.txt"], cwd=root, check=True)
            (root / "owner-evidence.json").write_text("first\n", encoding="utf-8")

            first = harness.content_addressed_snapshot(root)
            self.assertEqual(first["file_count"], 2)
            self.assertTrue(first["digest"].startswith("sha256:"))

            (root / "owner-evidence.json").write_text("second\n", encoding="utf-8")
            second = harness.content_addressed_snapshot(root)
            self.assertNotEqual(first["digest"], second["digest"])

    def test_dirty_snapshot_is_eligible_when_all_gates_pass(self) -> None:
        harness = load_harness()
        gates = {
            "exact": {"state": "executed_passed"},
            "migration": {"state": "executed_passed"},
        }

        self.assertEqual(harness.gate_blockers(gates), [])
        self.assertTrue(harness.acceptance_eligible(gates))


if __name__ == "__main__":
    unittest.main()
