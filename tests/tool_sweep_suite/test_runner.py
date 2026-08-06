"""Behavioral contracts for catalog-sweep response accounting."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
import tempfile
import unittest


RUNNER = Path(__file__).with_name("runner.py")


def load_runner():
    spec = importlib.util.spec_from_file_location("tool_sweep_runner", RUNNER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ProblemCodeTests(unittest.TestCase):
    def test_problem_code_is_a_first_class_field_for_success_framed_unavailable(self) -> None:
        """A rendered unavailable result must not become an apparently clean response."""
        runner = load_runner()
        response = {
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": '{"problem":{"kind":"unavailable","code":"resource.authority_unavailable"}}',
                    }
                ]
            }
        }

        row = runner.response_row("resource", "tracedecay://health", response, 17, 30_000)

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "resource.authority_unavailable")
        self.assertEqual(row["deadline_ms"], 30_000)

    def test_prompt_denial_retains_its_typed_problem_code(self) -> None:
        """A prompt failure must keep policy diagnosis in the aggregate artifact."""
        runner = load_runner()
        response = {
            "error": {
                "data": {
                    "problem": {"kind": "denied", "code": "policy.prompt_denied"},
                }
            }
        }

        row = runner.response_row("prompt", "triage", response, 3, 30_000)

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "policy.prompt_denied")

    def test_declared_unavailable_does_not_accept_a_different_typed_failure(self) -> None:
        """An unavailable contract cannot be green merely because a denial has a code."""
        runner = load_runner()

        class Client:
            def call_tool(self, _name: str, _arguments: dict[str, object], _deadline_ms: int):
                return {"error": {"data": {"problem": {"kind": "denied", "code": "policy.denied"}}}}, 4

        row = runner._unavailable_tool_row(
            Client(), runner.ToolPolicy("tracedecay_unavailable", "unavailable", "read", 1_000)
        )

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "policy.denied")

    def test_direct_problem_shape_is_preserved_in_artifacts(self) -> None:
        """Both MCP error framings retain their typed diagnosis."""
        runner = load_runner()

        row = runner.response_row(
            "resource", "tracedecay://status", {"error": {"data": {"kind": "unavailable", "code": "store.offline"}}}, 2, 30_000
        )

        self.assertEqual(row["problem_code"], "store.offline")
        self.assertEqual(row["verdict"], "FAIL")

    def test_success_framed_markdown_not_found_is_not_coverage(self) -> None:
        """A human-friendly not-found message cannot masquerade as a completed journey."""
        runner = load_runner()

        row = runner.response_row(
            "tool",
            "tracedecay_node",
            {"result": {"content": [{"type": "text", "text": "## Node\n\nNode not found: fixture-node"}]}},
            2,
            30_000,
        )

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "tool_sweep.success_framed_not_found")

    def test_markdown_fact_identity_is_consumable_by_rollback(self) -> None:
        """The default Markdown renderer remains a valid producer for fact removal."""
        runner = load_runner()
        content = "catalog sweep temporary isolated fact"

        fact_id = runner.fact_id_with_content(
            {"result": {"content": [{"type": "text", "text": f"## Fact Store\n\n### Fact\n- #42 tool trust 0.500: {content}\n"}]}},
            content,
        )

        self.assertEqual(fact_id, 42)

    def test_indented_markdown_node_identity_is_consumable(self) -> None:
        """Nested Markdown fields from a live qualified-name producer retain their identity."""
        runner = load_runner()

        node_id = runner.first_value(
            {"result": {"content": [{"type": "text", "text": "  **node_id:** `function:fixture`\n"}]}},
            {"node_id"},
        )

        self.assertEqual(node_id, "function:fixture")

    def test_truncated_markdown_preview_exposes_its_retrieval_handle(self) -> None:
        """A source preview may defer its full expected state to tracedecay_retrieve."""
        runner = load_runner()

        handle = runner.response_handle(
            {"result": {"content": [{"type": "text", "text": "# Truncated Response\n\nRetrieve it with handle `rh_fixture`."}]}}
        )

        self.assertEqual(handle, "rh_fixture")

    def test_effect_unknown_markdown_exposes_reconciliation_identity(self) -> None:
        """Reconciliation consumes the daemon's original effect receipt, not a guessed journal."""
        runner = load_runner()

        identity = runner._reconciliation_identity(
            {"result": {"content": [{"type": "text", "text": "\n".join((
                "**effect_unknown:** true",
                "**effect_id:** `effect.source-edit.fixture`",
                "**idempotency_key:** fixture-key",
                "**input_digest:** sha256:" + "a" * 64,
            ))}]}}
        )

        self.assertEqual(
            identity,
            ("effect.source-edit.fixture", "sha256:" + "a" * 64, "fixture-key"),
        )

    def test_only_documented_session_absence_can_pass_a_journey_call(self) -> None:
        """The session rollback has an explicit absence state; generic not-found remains failure."""
        runner = load_runner()

        class Client:
            def call_tool(self, _name: str, _arguments: dict[str, object], _deadline_ms: int):
                return {
                    "result": {
                        "_meta": {"duration_us": 1},
                        "content": [{"type": "text", "text": "**status:** no_baseline\n**message:** No session baseline found."}],
                    }
                }, 1

        runner._journey_call(Client(), "tracedecay_session_end", {}, 1_000)
        with self.assertRaises(runner.SweepError):
            runner._journey_call(Client(), "tracedecay_node", {}, 1_000)


class NegotiatedSurfaceTests(unittest.TestCase):
    def test_initialize_capabilities_control_optional_surface_discovery(self) -> None:
        """Only server-negotiated resource/prompt endpoints are requested."""
        runner = load_runner()

        self.assertEqual(
            runner.negotiated_surfaces({"tools": {}, "resources": {}, "prompts": {}}),
            {"tools", "resources", "prompts"},
        )
        self.assertEqual(runner.negotiated_surfaces({"tools": {}, "resources": {}}), {"tools", "resources"})

    def test_resources_and_prompts_are_exercised_from_live_discovery(self) -> None:
        """A resource/prompt added to negotiation cannot be silently tool-only coverage."""
        runner = load_runner()

        class Client:
            def __init__(self) -> None:
                self.calls: list[tuple[str, object]] = []

            def read_resource(self, uri: str, deadline_ms: int):
                self.calls.append(("resource", (uri, deadline_ms)))
                return {"result": {"contents": [{"uri": uri, "text": "ready"}]}}, 9

            def get_prompt(self, name: str, arguments: dict[str, str], deadline_ms: int):
                self.calls.append(("prompt", (name, arguments, deadline_ms)))
                return {"result": {"messages": []}}, 11

        client = Client()
        rows = runner.exercise_discovered_surfaces(
            client,
            resources=[{"uri": "tracedecay://health"}],
            prompts=[{"name": "triage", "arguments": [{"name": "question", "required": True}]}],
            fixture={"question": "inspect sweep anchor"},
            deadline_ms=30_000,
        )

        self.assertEqual([row["kind"] for row in rows], ["resource", "prompt"])
        self.assertTrue(all(row["verdict"] == "PASS" for row in rows))
        self.assertEqual(
            client.calls,
            [
                ("resource", ("tracedecay://health", 30_000)),
                ("prompt", ("triage", {"question": "inspect sweep anchor"}, 30_000)),
            ],
        )


class DispatchMetadataTests(unittest.TestCase):
    @staticmethod
    def definition(*, availability=None, terminal_states=None):
        return {
            "name": "tracedecay_fixture_read",
            "annotations": {"readOnlyHint": True},
            "_meta": {
                "tracedecay/dispatch": {
                    "version": 1,
                    "fingerprint": "sha256:fixture",
                    "availability": availability or {"state": "available"},
                    "effect": "read",
                    "read_only": True,
                    "deadline": {"maximum_millis": 1_000},
                    "idempotency": "not_provided",
                    "inverse": {"mode": "not_applicable"},
                    "cancellation": {"mode": "cooperative", "points": ["during_read"]},
                    "terminal_states": terminal_states
                    or [
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

    def test_policy_consumes_the_complete_canonical_dispatch_contract(self) -> None:
        runner = load_runner()

        policy = runner.tool_policy(self.definition())

        self.assertEqual(policy.deadline_ms, 1_000)
        self.assertEqual(policy.fingerprint, "sha256:fixture")

    def test_policy_rejects_cancellation_terminal_drift(self) -> None:
        runner = load_runner()
        definition = self.definition(
            terminal_states=[
                "completed",
                "deadline_exceeded",
                "denied",
                "failed",
                "unavailable",
            ]
        )

        with self.assertRaises(runner.SweepError):
            runner.tool_policy(definition)

    def test_unavailable_policy_requires_the_canonical_nonretryable_reason(self) -> None:
        runner = load_runner()
        definition = self.definition(
            availability={
                "state": "unavailable",
                "reason": "effect_journey_unverified",
                "retryable": False,
            }
        )

        policy = runner.tool_policy(definition)

        self.assertEqual(policy.availability_reason, "effect_journey_unverified")


class DeadlineTests(unittest.TestCase):
    def test_post_deadline_settlement_cannot_become_a_passing_response(self) -> None:
        runner = load_runner()
        client = object.__new__(runner.McpClient)
        waits = iter([None, {"result": {"content": []}}])
        sent = []
        client._new_id = lambda: 1
        client._send = sent.append
        client._wait = lambda _request_id, _deadline_ms: next(waits)

        with self.assertRaises(runner.CallDeadlineExceeded) as raised:
            client.request("tools/call", {"name": "tracedecay_read"}, 10, cancel_on_timeout=True)

        self.assertTrue(raised.exception.cancellation_settled)
        self.assertEqual(sent[-1]["method"], "notifications/cancelled")
        row = runner._call_failure_row("tool", "tracedecay_read", 10, raised.exception)
        self.assertEqual(row["problem_code"], "tool_sweep.call_deadline_exceeded")


class MutationJourneyTests(unittest.TestCase):
    def test_unrecognised_negotiated_mutation_is_a_failure_not_a_skip(self) -> None:
        """A new mutable catalog entry needs a real rollback recipe before it can pass."""
        runner = load_runner()
        policy = runner.ToolPolicy(
            name="tracedecay_new_mutation", availability="available", effect="administrative", deadline_ms=2_000
        )

        row = runner.missing_effect_journey_row(policy)

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "tool_sweep.effect_journey_unavailable")

    def test_source_edit_journey_replays_receipt_and_restores_exact_source(self) -> None:
        runner = load_runner()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "src").mkdir()
            source = root / "src/lib.rs"
            source.write_text("pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }\n")
            (root / "src/relocated.rs").write_text("pub fn relocation_marker() -> i32 { 0 }\n")
            fixture = {
                "root": str(root),
                "file": "src/lib.rs",
                "qualified_name": "sweep_anchor",
            }
            calls = []

            def call(tool, arguments, _deadline_ms):
                calls.append((tool, dict(arguments)))
                if arguments.get("dry_run") is True:
                    return {
                        "result": {
                            "content": [{
                                "type": "text",
                                "text": '{"expected_state":"sha256:' + "a" * 64 + '"}',
                            }]
                        }
                    }
                if arguments.get("old_str") == "value: 8":
                    source.write_text(
                        "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }\n"
                    )
                    return {"result": {"content": [{"type": "text", "text": '{"success":true}'}]}}
                return {
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": '{"success":true,"replayed":true,"effect_id":"effect.fixture"}',
                        }]
                    }
                }

            prepared = runner.prepare_journey(
                "tracedecay_str_replace",
                object(),
                fixture,
                lambda _tool: 1_000,
                call,
            )
            self.assertIsNotNone(prepared)
            source.write_text(
                "pub fn sweep_anchor() -> SweepType { SweepType { value: 8 } }\n"
            )
            note = prepared.cleanup(
                {
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": '{"success":true,"effect_id":"effect.fixture"}',
                        }]
                    }
                }
            )

        self.assertEqual(note, "preview/apply/consumer/rollback verified")
        self.assertTrue(all(arguments["format"] == "json" for _, arguments in calls))


if __name__ == "__main__":
    unittest.main()
