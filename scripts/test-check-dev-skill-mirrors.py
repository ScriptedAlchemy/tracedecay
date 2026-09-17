#!/usr/bin/env python3
"""Behavioral tests for the dev-skill host-copy lever."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT_PATH = ROOT / "scripts/check-dev-skill-mirrors.py"


def load_script():
    spec = importlib.util.spec_from_file_location("check_dev_skill_mirrors", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class DevSkillMirrorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mirrors = load_script()

    def write_tree(self, root: Path) -> None:
        claude = root / ".claude/skills/demo"
        codex = root / ".codex/skills/demo"
        claude.mkdir(parents=True)
        codex.mkdir(parents=True)
        (claude / "SKILL.md").write_text("same\n", encoding="utf-8")
        (codex / "SKILL.md").write_text("same\n", encoding="utf-8")
        host_private = codex / "agents"
        host_private.mkdir()
        (host_private / "openai.yaml").write_text("codex-only\n", encoding="utf-8")
        (codex / "scripts").mkdir()
        (codex / "scripts/friction-scan.test.sh").write_text("#!/bin/sh\n", encoding="utf-8")

    def test_matching_shared_files_pass_with_host_private_extras(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_tree(root)
            self.assertEqual(self.mirrors.check(root), [])

    def test_byte_drift_fails(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_tree(root)
            (root / ".claude/skills/demo/SKILL.md").write_text("edited\n", encoding="utf-8")
            errors = self.mirrors.check(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("demo/SKILL.md", errors[0])

    def test_shared_file_missing_from_one_tree_fails(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_tree(root)
            extra = root / ".codex/skills/interface-cluster-development"
            extra.mkdir()
            (extra / "SKILL.md").write_text("only codex\n", encoding="utf-8")
            errors = self.mirrors.check(root)
            self.assertTrue(any("interface-cluster-development/SKILL.md" in error for error in errors))

    def test_sync_copies_shared_files_and_leaves_host_private_files(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_tree(root)
            source = root / ".codex/skills/interface-cluster-development"
            source.mkdir()
            (source / "SKILL.md").write_text("cluster\n", encoding="utf-8")
            (source / "references").mkdir()
            (source / "references/role-prompts.md").write_text("roles\n", encoding="utf-8")
            stale = root / ".claude/skills/demo/stale.md"
            stale.write_text("gone\n", encoding="utf-8")
            private = root / ".codex/skills/demo/agents/openai.yaml"
            actions = self.mirrors.sync(root, "codex")
            self.assertEqual(self.mirrors.check(root), [])
            self.assertEqual(
                (root / ".claude/skills/interface-cluster-development/SKILL.md").read_text(
                    encoding="utf-8"
                ),
                "cluster\n",
            )
            self.assertFalse(stale.exists())
            self.assertEqual(private.read_text(encoding="utf-8"), "codex-only\n")
            self.assertFalse((root / ".claude/skills/demo/agents/openai.yaml").exists())
            self.assertTrue(any(action.startswith("write claude/") for action in actions))
            self.assertTrue(any(action.startswith("delete claude/") for action in actions))

    def test_optional_agents_tree_is_included_when_present(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_tree(root)
            agents = root / ".agents/skills/demo"
            agents.mkdir(parents=True)
            (agents / "SKILL.md").write_text("diverged\n", encoding="utf-8")
            errors = self.mirrors.check(root)
            self.assertTrue(any("demo/SKILL.md" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
