"""Behavioral contracts for catalog-sweep response accounting."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
