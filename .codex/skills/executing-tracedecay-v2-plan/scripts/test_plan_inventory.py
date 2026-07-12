#!/usr/bin/env python3
"""Focused regression tests for V2 plan heading inventory."""

from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

import plan_inventory


class PlanInventoryHeadingTests(unittest.TestCase):
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
            ],
        )
        self.assertEqual([record["line"] for record in records], [1, 3, 5, 7, 9])
        self.assertEqual(records[1]["checkboxes"], {"done": 1, "total": 1})
        expected_block = "\n".join(lines[2:4]).encode()
        self.assertEqual(records[1]["block_sha256"], hashlib.sha256(expected_block).hexdigest())

    def test_unrelated_headings_are_not_inventory_records(self) -> None:
        lines = [
            "### Discussion of PR 999Z",
            "not an authoritative PR slice",
            "### Companion notes for PR 999Z",
            "also not a supported heading",
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "plan.md"
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")

            records = plan_inventory.scan(path, root)

        self.assertEqual(records, [])


if __name__ == "__main__":
    unittest.main()