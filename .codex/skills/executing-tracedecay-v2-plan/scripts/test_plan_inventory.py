#!/usr/bin/env python3
"""Focused regression tests for V2 plan heading inventory."""

from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path
from typing import cast

import plan_inventory


class PlanInventoryHeadingTests(unittest.TestCase):
    def test_failure_matrix_ids_are_unique_and_contiguous(self) -> None:
        root = Path(__file__).resolve().parents[4]
        ids = plan_inventory.validate_failure_matrix(root)
        self.assertEqual(len(ids), 171)
        self.assertEqual(ids[0], "FM-001")
        self.assertEqual(ids[-1], "FM-171")

    def test_failure_matrix_validator_rejects_duplicate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "docs/plans/tracedecay-v2/14-historical-failure-regression-matrix.md"
            path.parent.mkdir(parents=True)
            path.write_text("| FM-001 | a |\n| FM-001 | duplicate |\n| FM-003 | gap |\n")
            with self.assertRaisesRegex(ValueError, "duplicate"):
                plan_inventory.validate_failure_matrix(root)

    def test_active_baseline_failure_matrix_rows_contain_required_pr_references(self) -> None:
        root = Path(__file__).resolve().parents[4]
        matrix = (root / "docs/plans/tracedecay-v2/14-historical-failure-regression-matrix.md").read_text()
        self.assertNotIn("FM-168", plan_inventory.BASELINE_FM_BINDINGS)
        for fm_id, bindings in plan_inventory.BASELINE_FM_BINDINGS.items():
            row = next(line for line in matrix.splitlines() if line.startswith(f"| {fm_id} |"))
            for binding in bindings:
                self.assertIn(binding, row)

    def test_supported_heading_forms_and_ids(self) -> None:
        lines = [
            "### PR 4E — Bare heading",
            "- [ ] bare body",
            "### Task 8: PR 28A/28B — Multi-ID task",
            "- [x] task body",
            "### Task 10A: PR 25G/30K — Alphanumeric task",
            "task body",
            "### Companion requirements for PR 13B/8A: Shared constraints",
            "companion body",
            "### PR 30B2 — Multi-suffix ID",
            "final body",
            "### PR 12.1: Dotted ID",
            "dotted body",
            "### Task 15: PR 31A–31Q — Letter range",
            "range body",
            "### PR 24E0–24E8 companion — Numeric suffix range",
            "range body",
            "### PR 35–37 — Numeric range",
            "range body",
            "### PR 24E series: Aggregate series heading",
            "series body",
            "### 14.1 PR 33R controller phases",
            "numbered section body",
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "plan.md"
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")

            records = plan_inventory.scan(path, root)

        self.assertEqual(
            [record["ids"] for record in records],
            [
                ["PR 4E"],
                ["PR 28A", "PR 28B"],
                ["PR 25G", "PR 30K"],
                ["PR 13B", "PR 8A"],
                ["PR 30B2"],
                ["PR 12.1"],
                [f"PR 31{letter}" for letter in "ABCDEFGHIJKLMNOPQ"],
                [f"PR 24E{value}" for value in range(9)],
                ["PR 35", "PR 36", "PR 37"],
                ["PR 24E"],
                ["PR 33R"],
            ],
        )
        self.assertEqual([record["line"] for record in records], [1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21])
        self.assertEqual(records[1]["checkboxes"], {"done": 1, "total": 1})
        expected_block = "\n".join(lines[2:4]).encode()
        self.assertEqual(records[1]["block_sha256"], hashlib.sha256(expected_block).hexdigest())

    def test_headings_without_canonical_ids_are_not_inventory_records(self) -> None:
        lines = [
            "### Rebuild semantic/live PR context",
            "not a declared PR slice",
            "### Workspace, branch, commit, and PR workflows",
            "also not a declared PR slice",
            "### Discussion of PR 999Z",
            "reference-only heading",
            "### C0 — Phase 0 architecture inventory and ownership lock (`PR 1`, `PR 3`)",
            "convergence phase, not declaration authority",
            "### C3 — One scope/query/search/graph path (`PR 8A`, `PR 11–16`)",
            "convergence phase, not declaration authority",
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "plan.md"
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")

            records = plan_inventory.scan(path, root)

        self.assertEqual(records, [])

    def test_every_declared_plan_pr_id_is_inventoried(self) -> None:
        root = Path(__file__).resolve().parents[4]
        headings = []
        for path in plan_inventory.plan_files(root):
            for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                match = plan_inventory.HEADING.match(line)
                if match and "PR" in match.group("heading"):
                    headings.append((path, line_number, match.group("heading")))

        non_declarations = [
            f"{path.relative_to(root)}:{line_number}: {heading}"
            for path, line_number, heading in headings
            if not plan_inventory.heading_ids(heading)
        ]
        self.assertEqual(
            non_declarations,
            [
                "docs/plans/tracedecay-v2/13-research-provenance-and-context-anchors.md:466: "
                "Rebuild semantic/live PR context",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:847: "
                "C0 — Phase 0 architecture inventory and ownership lock (`PR 1`, `PR 3`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:857: "
                "C1 — Pure canonical contracts (`PR 4`, `PR 4A`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:864: "
                "C2 — One evidence and storage path (`PR 5–10`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:871: "
                "C3 — One scope/query/search/graph path (`PR 8A`, `PR 11–16`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:878: "
                "C4 — Reconciled domain projections (`PR 17–22`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:884: "
                "C5 — One capability and policy runtime (`PR 22A`, `PR 23 series`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:892: "
                "C6 — One application layer and official interface (`PR 24 series`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:900: "
                "C7 — One product (`PR 25–32`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:907: "
                "C8 — Backfill, reconcile, cut over (`PR 33–36`)",
                "docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md:915: "
                "C9 — Delete V1 and close entropy budget (`PR 37`)",
                "docs/plans/tracedecay-v2/24-canonical-task-plan-graph-and-multi-agent-executor.md:2038: "
                "9.8 Workspace, branch, commit, and PR workflows",
            ],
        )

        declared = {
            pr_id
            for _, _, heading in headings
            for pr_id in plan_inventory.heading_ids(heading)
        }
        inventoried = {
            pr_id
            for path in plan_inventory.plan_files(root)
            for record in plan_inventory.scan(path, root)
            for pr_id in cast(list[str], record["ids"])
        }
        self.assertEqual(len(headings), 311)
        self.assertEqual(sum(bool(plan_inventory.heading_ids(heading)) for _, _, heading in headings), 299)
        self.assertEqual(declared - inventoried, set())
        for pr_id in ("PR 12.1", "PR 28E", "PR 31Q", "PR 35J"):
            self.assertIn(pr_id, inventoried)
        self.assertNotIn("PR 999Z", inventoried)


if __name__ == "__main__":
    unittest.main()