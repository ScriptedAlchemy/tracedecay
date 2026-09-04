#!/usr/bin/env python3
"""Behavioral tests for the Codex hook-input renderer."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from types import ModuleType


ROOT = Path(__file__).resolve().parent.parent
RENDERER_PATH = ROOT / "scripts/render-codex-hook-inputs.py"
CURRENT_SHAPES = (
    ROOT / "tests/fixtures/codex-hook-inputs/current-user-message-shapes.jsonl"
)


def load_renderer() -> ModuleType:
    spec = importlib.util.spec_from_file_location("codex_hook_input_renderer", RENDERER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load renderer from {RENDERER_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class CodexHookInputRendererTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.renderer = load_renderer()

    def test_current_response_and_user_item_form_one_prompt_pair(self) -> None:
        cases, stats, _ = self.renderer.build_cases(
            [CURRENT_SHAPES], include_developer=True
        )

        self.assertEqual(stats["model_messages"], 2)
        self.assertEqual(stats["submitted_user_messages"], 2)
        self.assertEqual(len(cases), 1)
        self.assertEqual(
            cases[0].model_user.text, "Find the callers of publish_generation."
        )
        self.assertEqual(
            cases[0].submitted_user.text, "Find the callers of publish_generation."
        )

    def test_legacy_user_message_remains_supported(self) -> None:
        _, users = self.renderer.parse_file(CURRENT_SHAPES)

        self.assertEqual(users[-1].text, "Legacy prompt stays supported.")

    def test_duplicate_malformed_and_non_user_items_do_not_add_prompts(self) -> None:
        _, users = self.renderer.parse_file(CURRENT_SHAPES)

        self.assertEqual(
            [user.text for user in users],
            [
                "Find the callers of publish_generation.",
                "Legacy prompt stays supported.",
            ],
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
