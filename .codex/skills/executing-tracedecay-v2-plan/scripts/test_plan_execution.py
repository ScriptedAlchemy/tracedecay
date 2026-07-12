#!/usr/bin/env python3

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import plan_execution


class PlanExecutionTests(unittest.TestCase):
    def fixture(self) -> tuple[Path, dict]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        plan = root / "docs/plans/2026-07-09-tracedecay-brain-rewrite.md"
        plan.parent.mkdir(parents=True)
        plan.write_text("### PR 1 — First\n- [ ] one\n### PR 2 — Second\n- [ ] two\n", encoding="utf-8")
        records = plan_execution.inventory(root)
        graph = {"slices": []}
        for slice_id, sources in records.items():
            graph["slices"].append({
                "id": slice_id,
                "authority": sources[0]["path"],
                "source_hashes": {source["path"]: source["block_sha256"] for source in sources},
                "prerequisites": [] if slice_id == "PR 1" else ["PR 1"],
                "status": "integrated" if slice_id == "PR 1" else "not_started",
                "receipts": ({
                    "implementation_commit": "a", "review_verdict": "approved",
                    "test_receipts": ["ok"], "integration_commit": "b",
                } if slice_id == "PR 1" else {}),
            })
        return root, graph

    def test_valid_graph_selects_ready(self) -> None:
        root, graph = self.fixture()
        errors, entries = plan_execution.validate(root, graph)
        self.assertEqual(errors, [])
        self.assertEqual(plan_execution.next_ready(entries), ["PR 2"])

    def test_missing_slice_and_cycle_fail_closed(self) -> None:
        root, graph = self.fixture()
        graph["slices"] = graph["slices"][1:]
        graph["slices"][0]["prerequisites"] = ["PR 2"]
        errors, _ = plan_execution.validate(root, graph)
        self.assertTrue(any("missing graph slice: PR 1" in error for error in errors))
        self.assertTrue(any("self dependency" in error for error in errors))

    def test_stale_source_hash_and_receipts_fail_closed(self) -> None:
        root, graph = self.fixture()
        graph["slices"][0]["source_hashes"] = {}
        graph["slices"][0]["receipts"] = {}
        errors, _ = plan_execution.validate(root, graph)
        self.assertTrue(any("stale or incomplete source_hashes" in error for error in errors))
        self.assertTrue(any("lacks required receipts" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
