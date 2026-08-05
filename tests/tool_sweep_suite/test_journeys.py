#!/usr/bin/env python3
"""Focused unit coverage for real MCP effect journeys."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

from test_support import load_journeys, load_runner


class JourneyTests(unittest.TestCase):
    @staticmethod
    def _deadline_for(_tool: str) -> int:
        return 1_000

    @staticmethod
    def _runtime(runner):
        return SimpleNamespace(
            tool_error=runner.tool_is_error,
            typed_unavailable=runner.is_typed_unavailable,
            typed_denial=runner.is_typed_denial,
        )

    @staticmethod
    def _attempt(_runner, response, request_id=1):
        return SimpleNamespace(
            request_id=request_id,
            elapsed_ms=4,
            response=response,
            timed_out=False,
            transport_error=None,
        )

    def test_fact_store_cleanup_requires_the_added_fact_id_and_confirmed_remove(self) -> None:
        runner = load_runner()
        journeys = load_journeys()
        calls: list[tuple[str, dict[str, object], int]] = []

        class Client:
            def call_tool(self, name, arguments, deadline_ms):
                calls.append((name, arguments, deadline_ms))
                if arguments["action"] == "get":
                    response = {"result": {"content": [{"type": "text", "text": '{"fact":{"fact_id":17,"content":"tool-sweep temporary isolated-profile fact"}}'}]}}
                elif arguments["action"] == "remove":
                    response = {"result": {"content": [{"type": "text", "text": '{"removed":true}'}]}}
                else:
                    response = {"result": {"content": [{"type": "text", "text": '{"facts":[]}'}]}}
                return JourneyTests._attempt(
                    runner,
                    response,
                )

        policy = SimpleNamespace(deadline_ms=1_000)
        prepared = journeys.prepare_effect_journey(
            {"name": "tracedecay_fact_store"},
            policy,
            Client(),
            self._runtime(runner),
            SimpleNamespace(),
            self._deadline_for,
        )
        added = {"result": {"content": [{"type": "text", "text": '{"fact":{"fact_id":17,"content":"tool-sweep temporary isolated-profile fact"}}'}]}}
        cleanup = prepared.cleanup(added)

        self.assertEqual(cleanup, "fact add/get/remove/absence verified")
        self.assertEqual(
            calls,
            [
                ("tracedecay_fact_store", {"action": "remove", "fact_id": 17, "format": "json"}, 1_000),
                ("tracedecay_fact_store", {"action": "list", "limit": 5, "format": "json"}, 1_000),
            ],
        )

    def test_fact_store_cleanup_rejects_an_unconfirmed_inverse(self) -> None:
        runner = load_runner()
        journeys = load_journeys()

        class Client:
            def call_tool(self, _name, arguments, _deadline_ms):
                if arguments["action"] == "get":
                    response = {"result": {"content": [{"type": "text", "text": '{"fact":{"fact_id":17,"content":"tool-sweep temporary isolated-profile fact"}}'}]}}
                else:
                    response = {"result": {"content": [{"type": "text", "text": '{"removed":false}'}]}}
                return JourneyTests._attempt(
                    runner,
                    response,
                )

        prepared = journeys.prepare_effect_journey(
            {"name": "tracedecay_fact_store"},
            SimpleNamespace(deadline_ms=1_000),
            Client(),
            self._runtime(runner),
            SimpleNamespace(),
            self._deadline_for,
        )

        added = {"result": {"content": [{"type": "text", "text": '{"fact":{"fact_id":17,"content":"tool-sweep temporary isolated-profile fact"}}'}]}}
        self.assertIsNone(prepared.verify_success(added))
        with self.assertRaisesRegex(journeys.JourneyError, "did not confirm fact removal"):
            prepared.cleanup(added)

    def test_session_cleanup_requires_no_baseline_after_the_consuming_call(self) -> None:
        runner = load_runner()
        journeys = load_journeys()

        class Client:
            def __init__(self):
                self.calls = 0

            def call_tool(self, _name, _arguments, _deadline_ms):
                self.calls += 1
                response = (
                    {"result": {"content": [{"type": "text", "text": '{"signal_before":1}'}]}}
                    if self.calls == 1
                    else {"result": {"content": [{"type": "text", "text": '{"status":"no_baseline"}'}]}}
                )
                return JourneyTests._attempt(runner, response)

        prepared = journeys.prepare_effect_journey(
            {"name": "tracedecay_session_start"},
            SimpleNamespace(deadline_ms=1_000),
            Client(),
            self._runtime(runner),
            SimpleNamespace(),
            self._deadline_for,
        )

        self.assertEqual(prepared.cleanup(None), "session baseline removal verified")

    def test_effect_journeys_are_selected_by_real_reversible_coverage(self) -> None:
        journeys = load_journeys()
        for name in [
            "tracedecay_dashboard",
            "tracedecay_fact_store",
            "tracedecay_str_replace",
            "tracedecay_multi_str_replace",
            "tracedecay_insert_at",
            "tracedecay_ast_grep_rewrite",
            "tracedecay_replace_symbol",
            "tracedecay_insert_at_symbol",
            "tracedecay_move_symbol",
        ]:
            self.assertTrue(journeys.has_effect_journey(name), name)
        self.assertFalse(journeys.has_effect_journey("tracedecay_unknown"))

    def test_administrative_journeys_request_machine_readable_receipts(self) -> None:
        runner = load_runner()
        journeys = load_journeys()
        calls: list[tuple[str, dict[str, object]]] = []

        class Client:
            def call_tool(self, name, arguments, _deadline_ms):
                calls.append((name, arguments))
                status = (
                    "stopped" if name == "tracedecay_dashboard" else "no_baseline"
                )
                return JourneyTests._attempt(
                    runner,
                    {
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": f'{{"status":"{status}"}}',
                                }
                            ]
                        }
                    },
                )

        dashboard = journeys.prepare_effect_journey(
            {"name": "tracedecay_dashboard"},
            SimpleNamespace(deadline_ms=1_000),
            Client(),
            self._runtime(runner),
            SimpleNamespace(),
            self._deadline_for,
        )
        session = journeys.prepare_effect_journey(
            {"name": "tracedecay_session_start"},
            SimpleNamespace(deadline_ms=1_000),
            Client(),
            self._runtime(runner),
            SimpleNamespace(),
            self._deadline_for,
        )

        self.assertEqual(dashboard.arguments["format"], "json")
        self.assertEqual(session.arguments["format"], "json")
        dashboard.cleanup(None)
        self.assertEqual(calls[-1][1]["format"], "json")

    def test_source_edit_journey_previews_replays_and_restores_exact_bytes(self) -> None:
        runner = load_runner()
        journeys = load_journeys()
        original = "pub fn sweep_uncommitted() -> i32 { 7 }\n"
        changed = "pub fn sweep_changed() -> i32 { 7 }\n"

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            source = root / "src/lib.rs"
            source.write_text(original)
            calls: list[dict[str, object]] = []

            class Client:
                def call_tool(self, _name, arguments, _deadline_ms):
                    calls.append(arguments)
                    key = arguments.get("idempotency_key")
                    if key == "tool-sweep.tracedecay_str_replace.rollback":
                        source.write_text(original)
                        payload = {"success": True, "effect_id": "rollback-effect"}
                    elif key == "tool-sweep.tracedecay_str_replace.forward":
                        payload = {
                            "success": True,
                            "replayed": True,
                            "effect_id": "forward-effect",
                        }
                    else:
                        payload = {"success": True, "expected_state": "sha256:" + "a" * 64}
                    return JourneyTests._attempt(
                        runner,
                        {
                            "result": {
                                "content": [
                                    {
                                        "type": "text",
                                        "text": json.dumps(payload),
                                    }
                                ]
                            }
                        },
                    )

            prepared = journeys.prepare_effect_journey(
                {"name": "tracedecay_str_replace"},
                SimpleNamespace(deadline_ms=1_000),
                Client(),
                self._runtime(runner),
                SimpleNamespace(root=root, file="src/lib.rs"),
                self._deadline_for,
            )
            self.assertTrue(calls[0]["dry_run"])
            self.assertEqual(prepared.arguments["expected_state"], "sha256:" + "a" * 64)

            source.write_text(changed)
            applied = {
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": '{"success":true,"effect_id":"forward-effect"}',
                        }
                    ]
                }
            }
            self.assertIsNone(prepared.verify_success(applied))
            self.assertEqual(
                prepared.cleanup(applied),
                "source edit preview/apply/replay/inverse exact bytes verified",
            )
            self.assertEqual(source.read_text(), original)
            self.assertEqual(calls[-1]["format"], "json")

    def test_dashboard_cleanup_requires_a_stop_confirmation(self) -> None:
        runner = load_runner()
        journeys = load_journeys()

        class Client:
            def call_tool(self, _name, _arguments, _deadline_ms):
                return JourneyTests._attempt(
                    runner,
                    {"result": {"content": [{"type": "text", "text": '{"status":"not_running"}'}]}},
                )

        prepared = journeys.prepare_effect_journey(
            {"name": "tracedecay_dashboard"},
            SimpleNamespace(deadline_ms=1_000),
            Client(),
            self._runtime(runner),
            SimpleNamespace(),
            self._deadline_for,
        )

        with self.assertRaisesRegex(journeys.JourneyError, "did not confirm"):
            prepared.cleanup(None)


if __name__ == "__main__":
    unittest.main()
