#!/usr/bin/env python3
"""Behavioral tests for the Codex hook-input renderer."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
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

    def write_records(self, records: list[dict[str, object]]) -> Path:
        handle = tempfile.NamedTemporaryFile(mode="w", suffix=".jsonl", delete=False)
        with handle:
            for record in records:
                handle.write(json.dumps(record) + "\n")
        self.addCleanup(Path(handle.name).unlink, missing_ok=True)
        return Path(handle.name)

    def test_current_response_and_user_item_form_one_prompt_pair(self) -> None:
        messages, users = self.renderer.parse_file(CURRENT_SHAPES)
        pairs = self.renderer.pair_submitted_users(messages, users)

        self.assertEqual(len(messages), 1)
        self.assertEqual(len(users), 1)
        self.assertEqual(pairs[messages[0]], users[0])

    def test_legacy_user_message_remains_supported(self) -> None:
        fixture = self.write_records(
            [{"type": "event_msg", "payload": {"type": "user_message", "message": "Legacy prompt."}}]
        )
        _, users = self.renderer.parse_file(fixture)

        self.assertEqual(users[-1].text, "Legacy prompt.")

    def test_duplicate_malformed_and_non_user_items_do_not_add_prompts(self) -> None:
        fixture = self.write_records(
            [
                {"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "id": "one", "content": [{"type": "text", "text": "Prompt."}]}}},
                {"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "id": "one", "content": [{"type": "text", "text": "Prompt."}]}}},
                {"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "id": "image", "content": [{"type": "image", "image_url": "redacted"}]}}},
                {"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "AgentMessage", "id": "agent", "content": [{"type": "text", "text": "Reply."}]}}},
            ]
        )
        _, users = self.renderer.parse_file(fixture)
        self.assertEqual([user.text for user in users], ["Prompt."])

    def test_exact_ordered_pairs_do_not_reuse_or_cross_prompt_events(self) -> None:
        fixture = self.write_records(
            [
                {"timestamp": "2026-09-03T21:08:01.539Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "First prompt."}]}},
                {"timestamp": "2026-09-03T21:08:01.540Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Second prompt."}]}},
                {"timestamp": "2026-09-03T21:08:01.541Z", "type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "id": "second", "content": [{"type": "text", "text": "Second prompt."}]}}},
                {"timestamp": "2026-09-03T21:08:01.542Z", "type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "UserMessage", "id": "first", "content": [{"type": "text", "text": "First prompt."}]}}},
            ]
        )
        messages, users = self.renderer.parse_file(fixture)
        pairs = self.renderer.pair_submitted_users(messages, users)

        self.assertEqual(pairs[messages[0]].text, "First prompt.")
        self.assertEqual(pairs[messages[1]].text, "Second prompt.")
        self.assertEqual(len(set(pairs.values())), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
