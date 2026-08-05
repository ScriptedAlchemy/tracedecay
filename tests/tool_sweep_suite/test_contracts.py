#!/usr/bin/env python3
"""Focused contract and fixture coverage for the MCP tool sweep."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

import fixture as fixture_support
from fixture import prime_fixture_ledger
from test_support import load_runner


def _catalog_definition(
    name: str, effect: str = "read", *, availability: str = "available"
) -> dict[str, object]:
    read_only = effect in {"read", "preview"}
    dispatch_availability: dict[str, object] = {"state": availability}
    if availability == "unavailable":
        dispatch_availability.update(
            {"reason": "effect_journey_unverified", "retryable": False}
        )
    return {
        "name": name,
        "annotations": {"readOnlyHint": read_only},
        "_meta": {
            "tracedecay/dispatch": {
                "version": 1,
                "fingerprint": "a" * 64,
                "availability": dispatch_availability,
                "effect": effect,
                "read_only": read_only,
                "deadline": {"maximum_millis": 100},
                "idempotency": "not_provided",
                "inverse": {"mode": "not_applicable"}
                if read_only
                else {"mode": "unavailable", "reason": "no_verified_inverse"},
                "cancellation": {
                    "mode": "cooperative",
                    "points": ["before_read"],
                },
                "terminal_states": [
                    "completed",
                    "cancelled",
                    "deadline_exceeded",
                    "denied",
                    "failed",
                    "unavailable",
                ],
            }
        },
    }


class ExecutionPolicyTests(unittest.TestCase):
    def test_catalog_manifest_is_order_independent_and_round_trips_exact_tools(self) -> None:
        runner = load_runner()
        definitions = [
            {"name": "tracedecay_beta", "inputSchema": {"type": "object"}},
            {"name": "tracedecay_alpha", "inputSchema": {"type": "object"}},
        ]

        manifest = runner.catalog_manifest(definitions)

        self.assertEqual(manifest["tool_names"], ["tracedecay_alpha", "tracedecay_beta"])
        self.assertEqual(manifest, runner.catalog_manifest(list(reversed(definitions))))
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "catalog.json"
            runner.write_catalog_manifest(path, definitions)
            self.assertEqual(runner.load_catalog_manifest(path), manifest)

    def test_catalog_manifest_refuses_duplicate_or_tampered_tools(self) -> None:
        runner = load_runner()
        with self.assertRaisesRegex(runner.SweepError, "duplicate"):
            runner.catalog_manifest(
                [{"name": "tracedecay_same"}, {"name": "tracedecay_same"}]
            )
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "catalog.json"
            path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "tool_names": [],
                        "fingerprint": "bad",
                        "tools": [],
                    }
                )
            )
            with self.assertRaisesRegex(runner.SweepError, "fingerprint"):
                runner.load_catalog_manifest(path)

    def test_dispatch_metadata_is_required_for_every_discovered_tool(self) -> None:
        runner = load_runner()

        with self.assertRaisesRegex(runner.PolicyError, "dispatch metadata missing"):
            runner.dispatch_policy({"name": "tracedecay_search"})

    def test_catalog_dispatch_metadata_preserves_deadline_effect_and_cancellation(self) -> None:
        runner = load_runner()

        policy = runner.dispatch_policy(_catalog_definition("tracedecay_search"))

        self.assertEqual(policy.deadline_ms, 100)
        self.assertEqual(policy.effect, "read")
        self.assertEqual(
            policy.cancellation,
            {"mode": "cooperative", "points": ["before_read"]},
        )

    def test_dispatch_metadata_preserves_lifecycle_order(self) -> None:
        runner = load_runner()
        definition = _catalog_definition("tracedecay_effect", "source_edit")
        definition["_meta"]["tracedecay/dispatch"]["cancellation"] = {
            "mode": "cooperative",
            "points": [
                "before_admission",
                "before_effect",
                "effect_in_flight",
                "after_commit",
            ],
        }

        policy = runner.dispatch_policy(definition)

        self.assertEqual(
            policy.cancellation["points"],
            [
                "before_admission",
                "before_effect",
                "effect_in_flight",
                "after_commit",
            ],
        )
        definition["_meta"]["tracedecay/dispatch"]["cancellation"] = {
            "mode": "cooperative",
            "points": ["after_commit", "effect_in_flight", "before_effect"],
        }

        with self.assertRaisesRegex(runner.PolicyError, "not monotone"):
            runner.dispatch_policy(definition)

    def test_canonical_dispatch_metadata_carries_non_read_effects(self) -> None:
        runner = load_runner()
        policy = runner.dispatch_policy(
            _catalog_definition("tracedecay_dashboard", "administrative")
        )

        self.assertEqual(policy.effect, "administrative")
        self.assertEqual(policy.deadline_ms, 100)

    def test_unavailable_dispatch_preserves_its_typed_contract(self) -> None:
        runner = load_runner()
        policy = runner.dispatch_policy(
            _catalog_definition(
                "tracedecay_lcm_doctor",
                "administrative",
                availability="unavailable",
            )
        )

        self.assertEqual(policy.availability_state, "unavailable")
        self.assertEqual(policy.availability_reason, "effect_journey_unverified")
        self.assertEqual(policy.deadline_ms, 100)

    def test_legacy_execution_metadata_is_rejected(self) -> None:
        runner = load_runner()
        definition = _catalog_definition("tracedecay_search")
        definition["_meta"]["tracedecay/execution"] = {}

        with self.assertRaisesRegex(runner.PolicyError, "legacy execution"):
            runner.dispatch_policy(definition)


class CatalogCompletionTests(unittest.TestCase):
    def test_discovered_and_completed_sets_must_match_exactly(self) -> None:
        runner = load_runner()

        with self.assertRaisesRegex(runner.SweepError, "missing=tracedecay_b"):
            runner.require_exact_completion(
                {"tracedecay_a", "tracedecay_b"},
                {"tracedecay_a", "tracedecay_extra"},
            )


class TimingTests(unittest.TestCase):
    def test_nearest_rank_p95_and_max_use_all_warm_samples(self) -> None:
        runner = load_runner()

        p95, maximum = runner.timing_summary([4, 8, 15, 16, 23])

        self.assertEqual(p95, 23)
        self.assertEqual(maximum, 23)


class FixtureProducerTests(unittest.TestCase):
    def test_unavailable_feedback_producer_leaves_handle_unmaterialized(self) -> None:
        response = {
            "result": {
                "isError": True,
                "content": [
                    {
                        "type": "text",
                        "text": json.dumps(
                            {
                                "problem": {
                                    "kind": "unavailable",
                                    "code": "feedback.advisory-cycle.unavailable",
                                }
                            }
                        ),
                    }
                ],
            }
        }

        self.assertEqual(
            fixture_support._application_unavailable_code(response),
            fixture_support.UNAVAILABLE_FEEDBACK_PRODUCER_CODE,
        )

    def test_truncated_producer_response_retrieves_exact_body(self) -> None:
        calls: list[tuple[str, dict[str, object], int]] = []

        class Client:
            def call_tool(self, name, arguments, deadline_ms):
                calls.append((name, arguments, deadline_ms))
                return SimpleNamespace(
                    response={
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": '{"results":[{"node_id":"function:anchor"}]}',
                                }
                            ]
                        }
                    },
                    timed_out=False,
                )

        truncated = SimpleNamespace(
            response={
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": (
                                '{"truncated":true,"handle":"rh_search",'
                                '"preview":"{\\"results\\":["}'
                            ),
                        }
                    ]
                }
            },
            timed_out=False,
        )

        response = fixture_support._producer_response_value(Client(), truncated, 2_000)

        self.assertEqual(
            fixture_support._first_string(response, {"node_id"}),
            "function:anchor",
        )
        self.assertEqual(
            calls,
            [
                (
                    "tracedecay_retrieve",
                    {"handle": "rh_search", "format": "json"},
                    2_000,
                )
            ],
        )

    def test_node_lookup_mints_both_graph_authority_identities(self) -> None:
        calls: list[tuple[str, dict[str, object], int]] = []

        class Client:
            def call_tool(self, name, arguments, deadline_ms):
                calls.append((name, arguments, deadline_ms))
                response = {
                    "tracedecay_search": {
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": '{"results":[{"node_id":"function:anchor"}]}',
                                }
                            ]
                        }
                    },
                    "tracedecay_node": {
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": (
                                        '{"node_id":"function:anchor",'
                                        '"qualified_name":"src/lib.rs::sweep_anchor"}'
                                    ),
                                }
                            ]
                        }
                    },
                    "tracedecay_code_symbol_search": {
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": '{"results":[{"node_id":"code-node:anchor"}]}',
                                }
                            ]
                        }
                    },
                    "tracedecay_read": {
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": '{"handle":"response-handle"}',
                                }
                            ]
                        }
                    },
                    "tracedecay_feedback_advisory_cycle": {
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": (
                                        '{"request_handle":'
                                        '"feedback-request-handle"}'
                                    ),
                                }
                            ]
                        }
                    },
                }[name]
                return SimpleNamespace(response=response, timed_out=False)

        fixture = SimpleNamespace(
            root=Path("/tmp/tool-sweep-fixture"),
            source_file="src/lib.rs",
            source_dir="src",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            host_session_id="session-from-hook",
        )

        ledger = prime_fixture_ledger(Client(), fixture, lambda _tool: 1_000)

        self.assertEqual(ledger.node_id, "function:anchor")
        self.assertEqual(ledger.peer_node_id, "function:anchor")
        self.assertEqual(ledger.qualified_name, "src/lib.rs::sweep_anchor")
        self.assertEqual(ledger.code_node_id, "code-node:anchor")
        self.assertEqual(
            ledger.feedback_request_handle,
            "feedback-request-handle",
        )
        self.assertEqual(
            [name for name, _arguments, _deadline in calls],
            [
                "tracedecay_search",
                "tracedecay_node",
                "tracedecay_search",
                "tracedecay_code_symbol_search",
                "tracedecay_read",
                "tracedecay_feedback_advisory_cycle",
            ],
        )


class CancellationTests(unittest.TestCase):
    def test_cancellation_payload_preserves_the_negotiated_request_id(self) -> None:
        runner = load_runner()

        payload = runner.cancellation_notification(41)

        self.assertEqual(payload["method"], "notifications/cancelled")
        self.assertEqual(payload["params"]["requestId"], 41)


if __name__ == "__main__":
    unittest.main()
