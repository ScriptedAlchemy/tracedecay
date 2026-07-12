#!/usr/bin/env python3
"""Conformance tests for the discovery-only Claude skill wrapper."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


class ClaudeSkillWrapperTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(__file__).resolve().parents[4]
        self.canonical = self.root / ".codex/skills/executing-tracedecay-v2-plan"
        self.wrapper = self.root / ".claude/skills/executing-tracedecay-v2-plan"

    def test_wrapper_delegates_to_canonical_skill_only(self) -> None:
        body = (self.wrapper / "SKILL.md").read_text(encoding="utf-8")
        canonical_skill = ".codex/skills/executing-tracedecay-v2-plan/SKILL.md"
        canonical_scripts = ".codex/skills/executing-tracedecay-v2-plan/scripts/"

        self.assertIn(canonical_skill, body)
        self.assertIn("read", body.lower())
        self.assertIn("completely", body.lower())
        self.assertIn(canonical_scripts, body)
        self.assertIn("sole procedural authority", body)
        self.assertLess(len(body.encode("utf-8")), 3_000)

        script_paths = re.findall(r"(?:python3\s+)(\.[^\s\\]+/scripts/[^\s\\]+)", body)
        self.assertTrue(script_paths)
        self.assertTrue(all(path.startswith(canonical_scripts) for path in script_paths))

    def test_wrapper_has_no_implementation_tree(self) -> None:
        self.assertFalse((self.wrapper / "scripts").exists())
        self.assertEqual(
            sorted(path.relative_to(self.wrapper).as_posix() for path in self.wrapper.rglob("*")),
            ["SKILL.md"],
        )
        self.assertTrue((self.canonical / "scripts/plan_inventory.py").is_file())
        self.assertTrue((self.canonical / "scripts/plan_execution.py").is_file())


if __name__ == "__main__":
    unittest.main()
