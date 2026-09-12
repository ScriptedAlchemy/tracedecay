"""Behavioral contracts for catalog-sweep response accounting."""

from __future__ import annotations

import importlib.util
import json
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

    def test_nested_string_fact_identity_is_consumable_by_rollback(self) -> None:
        """The retained fact contract nests an opaque string id inside fact envelopes."""
        runner = load_runner()
        content = "catalog sweep temporary isolated fact"
        response = {
            "result": {
                "content": [{
                    "type": "text",
                    "text": json.dumps({
                        "outcome": {
                            "outcome": "effect",
                            "value": {
                                "payload": {
                                    "result": {
                                        "fact": {
                                            "fact": {
                                                "fact_id": "fact.v1.fixture",
                                                "content": content,
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }),
                }]
            }
        }

        self.assertEqual(runner.fact_id_with_content(response, content), "fact.v1.fixture")

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

class ExpectedHermeticDenialTests(unittest.TestCase):
    @staticmethod
    def policy(runner, name):
        return runner.ToolPolicy(name=name, availability="available", effect="read", deadline_ms=1_000)

    @staticmethod
    def client(text):
        class Client:
            def call_tool(self, _name, _arguments, _deadline_ms):
                return {"result": {"isError": True, "content": [{"type": "text", "text": text}]}}, 3

        return Client()

    @staticmethod
    def definition(name):
        return {"name": name, "inputSchema": {"type": "object", "properties": {}, "required": []}}

    def test_exact_expected_denial_is_the_passing_hermetic_verdict(self) -> None:
        """A declared non-producible surface passes only on its exact typed denial."""
        runner = load_runner()
        name = "tracedecay_test_results"
        self.assertIn(name, runner.EXPECTED_HERMETIC_DENIALS)
        kind, code = runner.EXPECTED_HERMETIC_DENIALS[name]

        row = runner._read_tool_row(
            self.client(f'{{"problem":{{"kind":"{kind}","code":"{code}"}}}}'),
            self.definition(name),
            self.policy(runner, name),
            fixture={},
        )

        self.assertEqual(row["verdict"], "PASS")
        self.assertTrue(row["expected_denial"])
        self.assertEqual(row["problem_code"], code)

    def test_a_different_typed_problem_stays_a_failure(self) -> None:
        """The expected-denial verdict is exact; it is not a blanket allowlist."""
        runner = load_runner()
        name = "tracedecay_test_results"

        row = runner._read_tool_row(
            self.client('{"problem":{"kind":"unavailable","code":"store.offline"}}'),
            self.definition(name),
            self.policy(runner, name),
            fixture={},
        )

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "store.offline")

    def test_hermetic_success_supersedes_a_stale_denial_entry(self) -> None:
        """A tool that gains a hermetic success path fails until its entry is removed."""
        runner = load_runner()
        name = "tracedecay_test_results"

        class Client:
            def call_tool(self, _name, _arguments, _deadline_ms):
                return {"result": {"content": [{"type": "text", "text": '{"results":[]}'}]}}, 3

        row = runner._read_tool_row(
            Client(), self.definition(name), self.policy(runner, name), fixture={}
        )

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "tool_sweep.expected_denial_superseded")

    def test_mutation_denial_probe_passes_exactly_and_needs_no_rollback(self) -> None:
        """A control mutation denied before admission proves its typed deny path."""
        runner = load_runner()
        name = "tracedecay_context_scout_pause"
        self.assertIn(name, runner.EXPECTED_HERMETIC_DENIALS)
        kind, code = runner.EXPECTED_HERMETIC_DENIALS[name]
        policy = runner.ToolPolicy(
            name=name, availability="available", effect="administrative", deadline_ms=1_000
        )

        row = runner.execute_effect(
            self.client(f'{{"problem":{{"kind":"{kind}","code":"{code}"}}}}'),
            self.definition(name),
            policy,
            fixture={},
            policies={},
        )

        self.assertEqual(row["verdict"], "PASS")
        self.assertTrue(row["expected_denial"])
        self.assertEqual(row["rollback"], "not_required")

    def test_mutation_denial_with_a_different_problem_stays_a_failure(self) -> None:
        runner = load_runner()
        name = "tracedecay_context_scout_pause"
        policy = runner.ToolPolicy(
            name=name, availability="available", effect="administrative", deadline_ms=1_000
        )

        row = runner.execute_effect(
            self.client('{"problem":{"kind":"conflict","code":"revision_mismatch"}}'),
            self.definition(name),
            policy,
            fixture={},
            policies={},
        )

        self.assertEqual(row["verdict"], "FAIL")
        self.assertNotIn("rollback", row)

    def test_session_outcome_error_framing_is_a_typed_problem(self) -> None:
        """The session-tool `outcome`/`error.code` framing is consumable as a typed problem."""
        runner = load_runner()
        response = {
            "result": {
                "isError": True,
                "content": [{
                    "type": "text",
                    "text": (
                        '{"outcome":"unavailable","error":{"code":"refresh_service_unavailable",'
                        '"message":"no session-temporal refresh authority"}}'
                    ),
                }],
            }
        }

        kind, code = runner.response_problem_code(response)

        self.assertEqual(kind, "unavailable")
        self.assertEqual(code, "refresh_service_unavailable")

    def test_git_preview_consumes_only_minted_hunk_preview_input(self) -> None:
        """The stage preview rides the git_hunks producer instead of invented state."""
        runner = load_runner()

        arguments = runner.git_preview_arguments(
            {
                "preview_input_id": "preview.fixture",
                "selected_hunk_digests": '["sha256:' + "a" * 64 + '"]',
            }
        )
        self.assertEqual(arguments["operation"], "stage_hunks")
        self.assertEqual(arguments["preview_input_id"], "preview.fixture")
        with self.assertRaises(runner.SweepError):
            runner.git_preview_arguments({})

    def test_code_navigation_consumes_the_code_query_node_identity(self) -> None:
        """Code navigation must consume the symbol-search producer's identity, not the graph node."""
        runner = load_runner()
        definition = {
            "name": "tracedecay_code_declaration",
            "inputSchema": {
                "type": "object",
                "properties": {"node_id": {"type": "string"}},
                "required": ["node_id"],
            },
        }

        arguments = runner.materialize_tool_arguments(
            definition,
            {
                "node_id": "function:graph",
                "code_navigation_node_ids": {
                    "tracedecay_code_declaration": "sym:code",
                },
            },
        )
        self.assertEqual(arguments, {"node_id": "sym:code", "format": "json"})
        with self.assertRaises(runner.SweepError):
            runner.materialize_tool_arguments(definition, {"node_id": "function:graph"})

    def test_each_navigation_consumer_uses_its_matching_search_result(self) -> None:
        runner = load_runner()
        identities = {
            name: f"symbol:{index}"
            for index, name in enumerate(runner.CODE_NAVIGATION_NODE_NAMES, 1)
        }
        for name, node_id in identities.items():
            arguments = runner.materialize_tool_arguments(
                {
                    "name": name,
                    "inputSchema": {
                        "type": "object",
                        "properties": {"node_id": {"type": "string"}},
                        "required": ["node_id"],
                    },
                },
                {"code_navigation_node_ids": identities},
            )
            self.assertEqual(arguments, {"node_id": node_id, "format": "json"}, name)

    def test_graph_file_consumers_use_the_seeded_source_file(self) -> None:
        runner = load_runner()
        schema = {
            "type": "object",
            "properties": {"files": {"type": "array", "items": {"type": "string"}}},
            "required": ["files"],
        }
        for name in ("tracedecay_affected", "tracedecay_diff_context"):
            arguments = runner.materialize_tool_arguments(
                {"name": name, "inputSchema": schema}, {"file": "src/lib.rs"}
            )
            self.assertEqual(arguments, {"files": ["src/lib.rs"], "format": "json"})

    def test_configuration_consumers_use_the_current_read_revision(self) -> None:
        runner = load_runner()
        fixture = {
            "configuration_key": "work.topology_policy.v1",
            "configuration_revision": "configuration.fixture.v1",
            "configuration_topology_policy": {
                "review_topology": {
                    "allowed": ["no_review", "independent_review", "standard_pull_requests"]
                }
            },
            "configuration_rollback_target_revision": "configuration.fixture.previous",
        }
        placeholder = {"type": "object", "properties": {}, "required": []}

        get = runner.materialize_tool_arguments(
            {"name": "tracedecay_configuration_get", "inputSchema": placeholder}, fixture
        )
        protected = runner.materialize_tool_arguments(
            {"name": "tracedecay_configuration_protected_preview", "inputSchema": placeholder},
            fixture,
        )
        rollback = runner.materialize_tool_arguments(
            {"name": "tracedecay_configuration_rollback_preview", "inputSchema": placeholder},
            fixture,
        )

        self.assertEqual(get["key"], "work.topology_policy.v1")
        self.assertEqual(protected["expected_revision"], "configuration.fixture.v1")
        self.assertEqual(
            protected["change"],
            {
                "kind": "replace_work_topology_policy",
                "value": {
                    "review_topology": {"allowed": ["no_review", "independent_review"]}
                },
            },
        )
        self.assertEqual(
            rollback["target_revision_id"], "configuration.fixture.previous"
        )

    def test_topology_metrics_materializes_a_valid_bounded_horizon(self) -> None:
        runner = load_runner()
        arguments = runner.materialize_tool_arguments(
            {
                "name": "tracedecay_work_topology_metrics",
                "inputSchema": {"type": "object", "properties": {}, "required": []},
            },
            {},
        )
        self.assertLess(arguments["horizon"]["since_micros"], arguments["horizon"]["until_micros"])
        self.assertGreater(arguments["max_events"], 0)

    def test_automation_view_consumes_the_list_producer_run_id(self) -> None:
        runner = load_runner()
        arguments = runner.materialize_tool_arguments(
            {
                "name": "tracedecay_automation_run_view",
                "inputSchema": {"type": "object", "properties": {}, "required": []},
            },
            {"automation_run_id": "automation.run.fixture"},
        )
        self.assertEqual(
            arguments, {"run_id": "automation.run.fixture", "format": "json"}
        )

    def test_lcm_expand_consumes_the_captured_message_store_id(self) -> None:
        runner = load_runner()
        arguments = runner.materialize_tool_arguments(
            {
                "name": "tracedecay_lcm_expand",
                "inputSchema": {"type": "object", "properties": {}, "required": []},
            },
            {"session_id": "session.fixture", "lcm_store_id": 41},
        )
        self.assertEqual(arguments["provider"], "codex")
        self.assertEqual(arguments["session_id"], "session.fixture")
        self.assertEqual(arguments["target"], {"kind": "raw_message", "store_id": 41})

    def test_session_refresh_status_consumes_the_begin_producer_handle(self) -> None:
        runner = load_runner()
        arguments = runner.materialize_tool_arguments(
            {
                "name": "tracedecay_session_refresh_status",
                "inputSchema": {"type": "object", "properties": {}, "required": []},
            },
            {
                "session_id": "session.fixture",
                "session_refresh_handle": "srh_fixture",
            },
        )
        self.assertEqual(arguments["handle"], "srh_fixture")
        self.assertEqual(arguments["scope"], {"kind": "profile"})
        self.assertEqual(arguments["session"], {"id": "session.fixture"})

    def test_work_readers_consume_the_shared_lifecycle_identities(self) -> None:
        runner = load_runner()
        fixture = {
            "work_selection": {"selection": "profile_owned_no_git"},
            "work_task_id": "task.fixture",
            "work_run_id": "run.fixture",
            "work_attempt_id": "attempt.fixture",
            "work_initial_version": {"graph_version": 1},
            "work_admitted_version": {"graph_version": 3},
            "work_generate_arguments": {"task_id": "task.fixture"},
            "work_status_arguments": {
                "task_id": "task.fixture",
                "run_id": "run.fixture",
                "attempt_id": "attempt.fixture",
            },
            "work_prepare_create_arguments": {"change": {"change": "create_task"}},
            "work_placement_arguments": {
                "task_id": "task.fixture",
                "run_id": "run.fixture",
                "target": {"kind": "clean_in_place"},
            },
            "work_duplicate_arguments": {
                "first_attempt": {"attempt_id": "attempt.fixture"},
                "second_attempt": {"attempt_id": "attempt.fixture.second"},
                "verdict": "not_duplicate",
            },
        }
        placeholder = {"type": "object", "properties": {}, "required": []}

        status = runner.materialize_tool_arguments(
            {"name": "tracedecay_work_attempt_status", "inputSchema": placeholder}, fixture
        )
        compared = runner.materialize_tool_arguments(
            {"name": "tracedecay_work_compare_proposal", "inputSchema": placeholder}, fixture
        )
        evidence = runner.materialize_tool_arguments(
            {"name": "tracedecay_work_retrieve_evidence", "inputSchema": placeholder}, fixture
        )
        placement = runner.materialize_tool_arguments(
            {"name": "tracedecay_work_placement_status", "inputSchema": placeholder}, fixture
        )
        duplicate = runner.materialize_tool_arguments(
            {
                "name": "tracedecay_work_prepare_duplicate_adjudication",
                "inputSchema": placeholder,
            },
            fixture,
        )

        self.assertEqual(status["attempt_id"], "attempt.fixture")
        self.assertEqual(compared["old_version"], {"graph_version": 1})
        self.assertEqual(compared["new_version"], {"graph_version": 3})
        self.assertEqual(evidence["verified_version"], {"graph_version": 3})
        self.assertEqual(evidence["temporal"], {"kind": "current"})
        self.assertEqual(placement, {
            "task_id": "task.fixture",
            "run_id": "run.fixture",
            "format": "json",
        })
        self.assertEqual(
            duplicate["second_attempt"]["attempt_id"], "attempt.fixture.second"
        )


class NegotiatedSurfaceTests(unittest.TestCase):
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

    def test_work_mutation_replay_stays_bound_to_the_shared_attempt(self) -> None:
        runner = load_runner()
        fixture = {
            "work_attempt_id": "attempt.fixture",
            "work_status_arguments": {
                "task_id": "task.fixture",
                "run_id": "run.fixture",
                "attempt_id": "attempt.fixture",
            },
            "work_effect_arguments": {
                "tracedecay_work_create": {"mutation_id": "mutation.fixture"},
            },
        }

        def call(tool, arguments, _deadline_ms):
            self.assertEqual(tool, "tracedecay_work_attempt_status")
            self.assertEqual(arguments, fixture["work_status_arguments"])
            return self.response('{"identity":{"attempt_id":"attempt.fixture"}}')

        prepared = runner.prepare_journey(
            "tracedecay_work_create", object(), fixture, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments, {"mutation_id": "mutation.fixture"})
        note = prepared.cleanup(self.response('{"replayed":true}'))
        self.assertIn("producer/effect/replay/status", note)

    def test_target_specific_work_journey_precedes_the_shared_replay(self) -> None:
        runner = load_runner()
        shared = runner.prepare_journey(
            "tracedecay_work_create",
            object(),
            {
                "work_attempt_id": "attempt.fixture",
                "work_status_arguments": {},
                "work_effect_arguments": {
                    "tracedecay_work_create": {"mutation_id": "mutation.fixture"},
                },
            },
            lambda _tool: 1_000,
            lambda *_args: self.response('{"identity":{"attempt_id":"attempt.fixture"}}'),
        )
        fixture = {
            "work_effect_journey": shared,
            "work_effect_arguments": {
                "tracedecay_work_create": {"mutation_id": "stale.fallback"},
            },
        }

        selected = runner.prepare_journey(
            "tracedecay_work_pause_run",
            object(),
            fixture,
            lambda _tool: 1_000,
            lambda *_args: {},
        )

        self.assertIs(selected, shared)
        self.assertEqual(selected.arguments["mutation_id"], "mutation.fixture")

    def test_attempt_recovery_journey_requires_typed_sets_and_rechecks(self) -> None:
        runner = load_runner()
        journeys = sys.modules[runner.prime_work_lifecycle.__module__]
        calls = []

        def call(tool, arguments, _deadline_ms):
            calls.append((tool, dict(arguments)))
            return self.response('{"recovery_required":[],"cancelled":[]}')

        prepared = journeys._prepare_work_effect_journey(
            "tracedecay_work_resume_attempts",
            {},
            call,
            lambda _tool: 1_000,
            {},
            {},
        )
        note = prepared.cleanup(self.response('{"recovery_required":[],"cancelled":[]}'))

        self.assertEqual(prepared.settlement, "contained")
        self.assertEqual(calls[0][0], "tracedecay_work_resume_attempts")
        self.assertIn("idempotent rescan", note)

    def test_pause_journey_resumes_and_reads_the_durable_control(self) -> None:
        runner = load_runner()
        journeys = sys.modules[runner.prime_work_lifecycle.__module__]
        calls = []

        def call(tool, arguments, _deadline_ms):
            calls.append((tool, dict(arguments)))
            if tool == "tracedecay_work_resume_run":
                return self.response('{"state":"running","authority":2}')
            if tool == "tracedecay_work_run_control":
                return self.response('{"state":"controlled","control":{"state":"running"}}')
            raise AssertionError(tool)

        prepared = journeys._prepare_work_effect_journey(
            "tracedecay_work_pause_run",
            {"work_task_id": "task.fixture", "work_run_id": "run.fixture"},
            call,
            lambda _tool: 1_000,
            {},
            {},
        )
        note = prepared.cleanup(self.response('{"state":"paused","authority":1}'))

        self.assertEqual([tool for tool, _ in calls], [
            "tracedecay_work_resume_run", "tracedecay_work_run_control"
        ])
        self.assertEqual(calls[0][1]["expected_authority_version"], 1)
        self.assertIn("durable run-control", note)

    def test_git_apply_consumes_preview_and_verifies_its_inverse(self) -> None:
        runner = load_runner()
        calls = []

        def call(tool, arguments, _deadline_ms):
            calls.append((tool, dict(arguments)))
            if tool == "tracedecay_git_hunks":
                scope = arguments["scope"]
                if scope == "staged" and sum(
                    1 for called, _ in calls if called == "tracedecay_git_apply"
                ) >= 2:
                    return self.response('{"hunks":[]}')
                return self.response(
                    '{"preview_input_id":"preview.input.' + scope + '","hunks":'
                    '[{"digest":"sha256:' + scope + '","hunk":{}}]}'
                )
            if tool == "tracedecay_git_preview":
                operation = arguments["operation"]
                self.assertEqual(
                    arguments["preview_input_id"],
                    "preview.input.working_tree" if operation == "stage_hunks" else "preview.input.staged",
                )
                return self.response(
                    '{"outcome":"preview","preview_id":"preview.' + operation + '",'
                    '"preview_digest":"sha256:' + operation + '"}'
                )
            self.assertEqual(tool, "tracedecay_git_apply")
            inverse = arguments["preview_id"] == "preview.unstage_hunks"
            return self.response(
                '{"outcome":"effect","effect_id":"'
                + ("effect.inverse" if inverse else "effect.stage") + '"}'
            )

        prepared = runner.prepare_journey(
            "tracedecay_git_apply", object(), {}, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments["preview_id"], "preview.stage_hunks")
        note = prepared.cleanup(
            self.response('{"outcome":"effect","effect_id":"effect.stage"}')
        )

        self.assertIn("inverse verified", note)
        self.assertEqual(
            [args["operation"] for tool, args in calls if tool == "tracedecay_git_preview"],
            ["stage_hunks", "unstage_hunks"],
        )

    def test_configuration_set_consumes_revision_and_restores_the_default(self) -> None:
        runner = load_runner()
        baseline = {"kind": "boolean", "value": False}
        changed = {"kind": "boolean", "value": True}
        state = {"revision": "configuration.r1", "value": baseline}
        calls = []

        def effect(receipt, base, result):
            return self.response(json.dumps({
                "outcome": "effect",
                "value": {"payload": {
                    "receipt_id": receipt,
                    "base_revision_id": base,
                    "result_revision_id": result,
                }},
            }))

        def setting():
            return self.response(json.dumps({"payload": {
                "key": "diagnostics.prewarm.v1",
                "revision_id": state["revision"],
                "effective_value": state["value"],
            }}))

        def call(tool, arguments, _deadline_ms):
            calls.append((tool, dict(arguments)))
            if tool == "tracedecay_configuration_get":
                return setting()
            if tool == "tracedecay_configuration_set":
                if state["revision"] == "configuration.r1":
                    state.update(revision="configuration.r2", value=changed)
                return effect("receipt.set", "configuration.r1", "configuration.r2")
            self.assertEqual(tool, "tracedecay_configuration_unset")
            state.update(revision="configuration.r3", value=baseline)
            return effect("receipt.unset", "configuration.r2", "configuration.r3")

        prepared = runner.prepare_journey(
            "tracedecay_configuration_set",
            object(),
            {
                "configuration_scalar_key": "diagnostics.prewarm.v1",
                "configuration_scalar_value": baseline,
                "project_id": "project.fixture",
            },
            lambda _tool: 1_000,
            call,
        )
        self.assertEqual(prepared.arguments["expected_revision"], "configuration.r1")
        self.assertEqual(prepared.arguments["value"], changed)
        note = prepared.cleanup(effect("receipt.set", "configuration.r1", "configuration.r2"))

        self.assertIn("inverse verified", note)
        self.assertEqual(state["value"], baseline)
        self.assertEqual(
            [tool for tool, _ in calls],
            [
                "tracedecay_configuration_get",
                "tracedecay_configuration_set",
                "tracedecay_configuration_get",
                "tracedecay_configuration_unset",
                "tracedecay_configuration_get",
            ],
        )

    def test_protected_configuration_apply_rolls_back_through_its_plan(self) -> None:
        runner = load_runner()
        policy = {
            "review_topology": {
                "allowed": ["no_review", "independent_review", "standard_pull_requests"]
            }
        }
        changed = {
            "review_topology": {"allowed": ["no_review", "independent_review"]}
        }
        state = {"revision": "configuration.r1", "policy": policy}

        def setting():
            return self.response(json.dumps({"payload": {
                "key": "work.topology_policy.v1",
                "revision_id": state["revision"],
                "effective_value": {"kind": "work_topology_policy", "value": state["policy"]},
            }}))

        def plan(plan_id, base):
            return self.response(json.dumps({
                "plan_id": plan_id,
                "base_revision_id": base,
                "operation_digest": "sha256:" + "4" * 64,
            }))

        def effect(receipt, base, result):
            return self.response(json.dumps({
                "outcome": "effect",
                "value": {"payload": {
                    "receipt_id": receipt,
                    "base_revision_id": base,
                    "result_revision_id": result,
                }},
            }))

        def call(tool, arguments, _deadline_ms):
            if tool == "tracedecay_configuration_get":
                return setting()
            if tool == "tracedecay_configuration_protected_preview":
                self.assertEqual(arguments["change"]["value"], changed)
                return plan("plan.protected", "configuration.r1")
            if tool == "tracedecay_configuration_protected_apply":
                return effect("receipt.protected", "configuration.r1", "configuration.r2")
            if tool == "tracedecay_configuration_rollback_preview":
                self.assertEqual(arguments["target_revision_id"], "configuration.r1")
                return plan("plan.rollback", "configuration.r2")
            self.assertEqual(tool, "tracedecay_configuration_rollback_apply")
            state.update(revision="configuration.r3", policy=policy)
            return effect("receipt.rollback", "configuration.r2", "configuration.r3")

        prepared = runner.prepare_journey(
            "tracedecay_configuration_protected_apply",
            object(),
            {"configuration_key": "work.topology_policy.v1"},
            lambda _tool: 1_000,
            call,
        )
        self.assertEqual(prepared.arguments["plan_id"], "plan.protected")
        state.update(revision="configuration.r2", policy=changed)
        note = prepared.cleanup(
            effect("receipt.protected", "configuration.r1", "configuration.r2")
        )

        self.assertIn("rollback verified", note)
        self.assertEqual(state, {"revision": "configuration.r3", "policy": policy})

    @staticmethod
    def response(payload: str):
        return {"result": {"content": [{"type": "text", "text": payload}]}}


    def test_fact_reads_consume_one_seeded_connected_graph(self) -> None:
        runner = load_runner()
        fixture = {}
        produced = iter(("fact.v1.alpha", "fact.v1.related"))

        def call(tool, arguments, _deadline_ms):
            self.assertEqual(tool, "tracedecay_fact_store_add")
            fact_id = next(produced)
            return self.response(json.dumps({
                "fact": {"fact": {
                    "fact_id": fact_id,
                    "content": arguments["content"],
                    "entities": arguments["entities"],
                }},
            }))

        runner.prime_fact_read_lifecycle(fixture, call, lambda _tool: 1_000)

        self.assertEqual(
            fixture["fact_read_arguments"]["tracedecay_fact_store_get"]["fact_id"],
            "fact.v1.alpha",
        )
        response = self.response(json.dumps({"facts": [
            {"fact_id": "fact.v1.alpha", "content": fixture["fact_content"]},
            {
                "fact_id": "fact.v1.related",
                "content": fixture["fact_related_content"],
            },
        ]}))
        runner.validate_fact_read_response(
            "tracedecay_fact_store_related", response, fixture
        )
        with self.assertRaisesRegex(Exception, "shared entity"):
            runner.validate_fact_read_response(
                "tracedecay_fact_store_related",
                self.response(json.dumps({"facts": [{
                    "fact_id": "fact.v1.alpha",
                    "content": fixture["fact_content"],
                }]})),
                fixture,
            )

    @classmethod
    def refresh_response(cls, family: str, payload: dict[str, object]):
        return cls.response(json.dumps({
            "contract": {
                "schema_id": "schema.application.retained.session-refresh.result",
                "schema_revision": 1,
            },
            "request_id": "request.fixture",
            "scope": {"project_id": "profile.fixture"},
            "outcome": {"outcome": family, "value": {"payload": payload}},
        }))


    def test_fact_feedback_journey_requires_a_real_trust_change(self) -> None:
        """Helpful feedback must move the seeded fact's trust, then remove the fact."""
        runner = load_runner()
        state = {"trust_millionths": 500_000, "removed": False}

        def commit(disposition, event_id):
            return {
                "fact_id": "fact.v1.fixture",
                "disposition": disposition,
                "last_event_id": event_id,
                "committed_event_ids": [event_id],
            }

        def call(tool, arguments, _deadline_ms):
            if tool == "tracedecay_fact_store_add":
                self.assertEqual(arguments["source_label"], "catalog_sweep")
                self.assertNotIn("source", arguments)
                return self.response(
                    '{"result":{"fact":{"fact":{"fact_id":"fact.v1.fixture","content":"'
                    + arguments["content"] + '"}}}}'
                )
            if tool == "tracedecay_fact_store_get":
                if state["removed"]:
                    return self.response(json.dumps({
                        "fact": {"kind": "unavailable", "status": {
                            "fact_id": "fact.v1.fixture", "payload_access": "deleted",
                        }},
                    }))
                return self.response(
                    '{"fact":{"fact_id":"fact.v1.fixture","trust_score_millionths":'
                    + str(state["trust_millionths"]) + "}}"
                )
            if tool == "tracedecay_fact_store_remove":
                state["removed"] = True
                return self.response(json.dumps({
                    "outcome": "removed",
                    "commit": commit("committed", "fact-event.v1.remove"),
                }))
            if tool == "tracedecay_fact_feedback":
                return self.response(json.dumps({
                    "commit": commit("idempotent_replay", "fact-event.v1.feedback"),
                }))
            raise AssertionError(tool)

        prepared = runner.prepare_journey(
            "tracedecay_fact_feedback", object(), {}, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments["fact_id"], "fact.v1.fixture")
        self.assertEqual(prepared.arguments["action"], "helpful")
        self.assertEqual(prepared.arguments["source_label"], "catalog_sweep")

        with self.assertRaises(Exception):
            prepared.cleanup(self.response('{"status":"recorded"}'))
        self.assertFalse(state["removed"])

        state["trust_millionths"] = 550_000
        note = prepared.cleanup(
            self.response(json.dumps({
                "outcome": "effect",
                "value": {"payload": {
                    "feedback": {
                        "fact_id": "fact.v1.fixture",
                        "action": "helpful",
                        "old_trust_millionths": 500_000,
                        "new_trust_millionths": 550_000,
                    },
                    "commit": commit("committed", "fact-event.v1.feedback"),
                }},
            }))
        )
        self.assertIn("trust", note)
        self.assertTrue(state["removed"])

    def test_fact_remove_journey_proves_tombstone_and_idempotent_replay(self) -> None:
        runner = load_runner()
        event_id = "fact-event.v1.remove"

        def removal(disposition):
            return self.response(json.dumps({
                "outcome": "removed",
                "commit": {
                    "fact_id": "fact.v1.remove",
                    "disposition": disposition,
                    "last_event_id": event_id,
                    "committed_event_ids": [event_id],
                },
            }))

        def call(tool, arguments, _deadline_ms):
            if tool == "tracedecay_fact_store_add":
                return self.response(json.dumps({"fact": {"fact": {
                    "fact_id": "fact.v1.remove",
                    "content": arguments["content"],
                }}}))
            if tool == "tracedecay_fact_store_get":
                return self.response(json.dumps({"fact": {
                    "kind": "unavailable",
                    "status": {
                        "fact_id": "fact.v1.remove",
                        "payload_access": "deleted",
                    },
                }}))
            if tool == "tracedecay_fact_store_list":
                return self.response('{"facts":[]}')
            self.assertEqual(tool, "tracedecay_fact_store_remove")
            return removal("idempotent_replay")

        prepared = runner.prepare_journey(
            "tracedecay_fact_store_remove", object(), {}, lambda _tool: 1_000, call
        )
        note = prepared.cleanup(removal("committed"))

        self.assertEqual(prepared.settlement, "irreversible_verified")
        self.assertIn("tombstone/default-absence/replay", note)

    def test_fact_supersede_journey_retires_default_but_preserves_exact_history(self) -> None:
        runner = load_runner()
        fact_ids = iter(("fact.v1.retired", "fact.v1.successor"))
        calls = []

        def call(tool, arguments, _deadline_ms):
            calls.append((tool, dict(arguments)))
            if tool == "tracedecay_fact_store_add":
                fact_id = next(fact_ids)
                return self.response(json.dumps({
                    "result": {"fact": {"fact": {
                        "fact_id": fact_id,
                        "content": arguments["content"],
                    }}},
                }))
            if tool == "tracedecay_fact_store_list":
                return self.response(json.dumps({"facts": [{
                    "fact_id": "fact.v1.successor",
                    "content": "catalog sweep fact after supersession",
                }]}))
            if tool == "tracedecay_fact_store_get":
                self.assertEqual(arguments["fact_id"], "fact.v1.retired")
                return self.response(json.dumps({"fact": {
                    "kind": "superseded",
                    "superseded_by": "fact.v1.successor",
                    "fact": {
                        "fact_id": "fact.v1.retired",
                        "content": "catalog sweep fact before supersession",
                    },
                }}))
            self.assertEqual(tool, "tracedecay_fact_store_supersede")
            return self.response(json.dumps({
                "outcome": "superseded",
                "superseded_by": "fact.v1.successor",
                "commit": {
                    "fact_id": "fact.v1.retired",
                    "disposition": "idempotent_replay",
                    "last_event_id": "fact-event.v1.retirement",
                    "committed_event_ids": ["fact-event.v1.retirement"],
                },
            }))

        prepared = runner.prepare_journey(
            "tracedecay_fact_store_supersede", object(), {}, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments, {
            "fact_id": "fact.v1.retired",
            "superseded_by": "fact.v1.successor",
            "format": "json",
        })
        note = prepared.cleanup(self.response(json.dumps({
            "outcome": "superseded",
            "superseded_by": "fact.v1.successor",
            "commit": {
                "fact_id": "fact.v1.retired",
                "disposition": "committed",
                "last_event_id": "fact-event.v1.retirement",
                "committed_event_ids": ["fact-event.v1.retirement"],
            },
        })))

        self.assertIn("exact-get/replay", note)
        self.assertEqual(
            [tool for tool, _arguments in calls],
            [
                "tracedecay_fact_store_add",
                "tracedecay_fact_store_add",
                "tracedecay_fact_store_list",
                "tracedecay_fact_store_get",
                "tracedecay_fact_store_supersede",
            ],
        )

    def test_fact_curate_journey_consumes_its_durable_run(self) -> None:
        runner = load_runner()

        def call(tool, arguments, _deadline_ms):
            if tool == "tracedecay_fact_store_add":
                return self.response(json.dumps({"fact": {"fact": {
                    "fact_id": "fact.v1.curator",
                    "content": arguments["content"],
                }}}))
            self.assertEqual(tool, "tracedecay_automation_run_view")
            self.assertEqual(arguments["run_id"], "automation.run.curator")
            return self.response(json.dumps({
                "run_id": "automation.run.curator",
                "task": "memory_curator",
                "status": "succeeded",
            }))

        prepared = runner.prepare_journey(
            "tracedecay_fact_store_curate", object(), {}, lambda _tool: 1_000, call
        )
        note = prepared.cleanup(self.response(json.dumps({
            "run_id": "automation.run.curator",
            "task": "memory_curator",
        })))

        self.assertEqual(prepared.settlement, "isolated")
        self.assertIn("retained terminal succeeded", note)

    def test_memory_status_journey_counts_the_seeded_fact(self) -> None:
        """The repaired status must truthfully count the seeded fact before rollback."""
        runner = load_runner()

        def call(tool, arguments, _deadline_ms):
            if tool == "tracedecay_fact_store_add":
                return self.response(
                    '{"fact":{"fact_id":3,"content":"' + arguments["content"] + '"}}'
                )
            if tool == "tracedecay_fact_store_remove":
                return self.response(json.dumps({
                    "outcome": "removed",
                    "commit": {
                        "fact_id": 3,
                        "disposition": "committed",
                        "last_event_id": "fact-event.v1.remove",
                        "committed_event_ids": ["fact-event.v1.remove"],
                    },
                }))
            self.assertEqual(tool, "tracedecay_fact_store_get")
            return self.response(json.dumps({
                "fact": {"kind": "unavailable", "status": {
                    "fact_id": 3, "payload_access": "deleted",
                }},
            }))

        prepared = runner.prepare_journey(
            "tracedecay_memory_status", object(), {}, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments, {"format": "json"})

        with self.assertRaises(Exception):
            prepared.cleanup(self.response('{"status":"ok","memory":{"fact_count":0}}'))
        note = prepared.cleanup(self.response('{"status":"ok","memory":{"fact_count":1}}'))
        self.assertIn("counted", note)

    def test_run_affected_tests_journey_proves_zero_coverage_retains_nothing(self) -> None:
        """A truthful zero-coverage run passes only while test_results stays unavailable."""
        runner = load_runner()
        retained = {"unavailable": True}

        def call(tool, _arguments, _deadline_ms):
            self.assertEqual(tool, "tracedecay_test_results")
            if retained["unavailable"]:
                raise RuntimeError(
                    "tracedecay_test_results journey call failed: application.retrieval.unavailable"
                )
            return self.response('{"results":[{"test":"phantom","passed":true}]}')

        prepared = runner.prepare_journey(
            "tracedecay_run_affected_tests",
            object(),
            {"file": "src/lib.rs"},
            lambda _tool: 1_000,
            call,
        )
        self.assertEqual(prepared.arguments["changed_paths"], ["src/lib.rs"])

        zero = '{"passed":0,"failed":0,"results":[],"note":"no tests cover the changed paths (1 file(s))"}'
        note = prepared.cleanup(self.response(zero))
        self.assertIn("no managed result retained", note)

        retained["unavailable"] = False
        with self.assertRaises(Exception):
            prepared.cleanup(self.response(zero))

    def test_session_refresh_journey_requires_a_durable_terminal_receipt(self) -> None:
        """The refresh rollback is a receipt-backed durable cancel, verified terminal,
        bound to the daemon's mounted profile without exposing internal ids."""
        runner = load_runner()
        state = {"terminal": False}
        fixture = {"root": "/fixture/root", "session_id": "session.sweep"}

        def call(tool, arguments, _deadline_ms):
            self.assertEqual(arguments["scope"], {"kind": "profile"})
            self.assertEqual(arguments["session"], {"id": "session.sweep"})
            self.assertEqual(arguments["handle"], "srh_fixture")
            if tool == "tracedecay_session_refresh_cancel":
                state["terminal"] = True
                return self.refresh_response(
                    "effect",
                    {"outcome": "cancelled", "receipt": {
                        "operation_id": "refresh.op", "state": "cancelled",
                    }},
                )
            self.assertEqual(tool, "tracedecay_session_refresh_status")
            if not state["terminal"]:
                return self.refresh_response(
                    "evidence", {"outcome": "running", "progress": {"operation_id": "refresh.op"}}
                )
            return self.refresh_response(
                "evidence",
                {"outcome": "cancelled", "receipt": {
                    "operation_id": "refresh.op", "state": "cancelled",
                }},
            )

        prepared = runner.prepare_journey(
            "tracedecay_session_refresh_begin", object(), fixture, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments["scope"], {"kind": "profile"})
        self.assertEqual(prepared.arguments["session"]["id"], "session.sweep")
        self.assertNotIn("action", prepared.arguments)
        self.assertNotIn("handle", prepared.arguments)

        with self.assertRaises(Exception):
            prepared.cleanup(self.refresh_response("effect", {"outcome": "running"}))

        note = prepared.cleanup(
            self.refresh_response(
                "effect",
                {"outcome": "started", "handle": "srh_fixture", "operation_id": "refresh.op"},
            )
        )
        self.assertIn("terminal", note)

    def test_session_refresh_cancel_journey_begins_then_settles_the_receipt(self) -> None:
        """The cancel journey begins its own refresh and proves the receipt stays terminal."""
        runner = load_runner()
        fixture = {"root": "/fixture/root", "session_id": "session.sweep"}

        def call(tool, arguments, _deadline_ms):
            if tool == "tracedecay_session_refresh_begin":
                self.assertNotIn("handle", arguments)
                return self.refresh_response(
                    "effect",
                    {"outcome": "started", "handle": "srh_fixture", "operation_id": "refresh.op"},
                )
            self.assertEqual(tool, "tracedecay_session_refresh_status")
            self.assertEqual(arguments["handle"], "srh_fixture")
            return self.refresh_response(
                "evidence",
                {"outcome": "cancelled", "receipt": {
                    "operation_id": "refresh.op", "state": "cancelled",
                }},
            )

        prepared = runner.prepare_journey(
            "tracedecay_session_refresh_cancel", object(), fixture, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments["handle"], "srh_fixture")
        self.assertEqual(prepared.arguments["scope"]["kind"], "profile")

        with self.assertRaises(Exception):
            prepared.cleanup(self.refresh_response(
                "effect", {"outcome": "running", "progress": {"operation_id": "refresh.op"}}
            ))

        note = prepared.cleanup(
            self.refresh_response(
                "effect",
                {"outcome": "cancelled", "receipt": {
                    "operation_id": "refresh.op", "state": "cancelled",
                }},
            )
        )
        self.assertIn("terminal", note)

    def test_journaled_rollback_consumes_the_move_receipt_and_restores_preimages(self) -> None:
        """source_edit_rollback's journey mints identities from a real move receipt."""
        runner = load_runner()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "src").mkdir()
            source = root / "src/lib.rs"
            moved = "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }\n"
            source.write_text(moved)
            relocated = root / "src/relocated.rs"
            relocated.write_text("pub fn relocation_marker() -> i32 { 0 }\n")
            original = {"src/lib.rs": source.read_text(), "src/relocated.rs": relocated.read_text()}
            fixture = {
                "root": str(root),
                "file": "src/lib.rs",
                "qualified_name": "src/lib.rs::sweep_anchor",
            }
            digest = "sha256:" + "b" * 64

            def call(tool, arguments, _deadline_ms):
                self.assertEqual(tool, "tracedecay_move_symbol")
                if arguments.get("dry_run") is True:
                    return self.response(f'{{"expected_state":"{digest}"}}')
                source.write_text("")
                relocated.write_text(original["src/relocated.rs"] + moved)
                return self.response(
                    '{"success":true,"effect_id":"effect.move",'
                    f'"input_digest":"{digest}","committed_state":"{digest}"}}'
                )

            prepared = runner.prepare_journey(
                "tracedecay_source_edit_rollback", object(), fixture, lambda _tool: 1_000, call
            )
            self.assertIsNotNone(prepared)
            self.assertEqual(prepared.arguments["effect_id"], "effect.move")
            self.assertEqual(prepared.arguments["original_input_digest"], digest)
            self.assertEqual(prepared.arguments["expected_state"], digest)
            self.assertIs(prepared.arguments["confirm"], True)

            rollback_receipt = self.response(
                '{"success":true,"reconciled":true,"effect_id":"effect.rollback"}'
            )
            with self.assertRaises(Exception):
                # The workspace still holds the moved bytes: rollback must not
                # claim preimage restoration.
                prepared.cleanup(rollback_receipt)
            source.write_text(original["src/lib.rs"])
            relocated.write_text(original["src/relocated.rs"])
            note = prepared.cleanup(rollback_receipt)
            self.assertIn("preimage restoration verified", note)

    def test_move_journey_rolls_back_from_the_effect_receipt(self) -> None:
        runner = load_runner()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "src").mkdir()
            source = root / "src/lib.rs"
            moved = "pub fn sweep_anchor() -> i32 { 7 }\n"
            source.write_text(moved)
            relocated = root / "src/relocated.rs"
            relocated.write_text("pub fn relocation_marker() -> i32 { 0 }\n")
            original_source = source.read_text()
            original_relocated = relocated.read_text()
            fixture = {
                "root": str(root),
                "file": "src/lib.rs",
                "symbol": "sweep_anchor",
                "qualified_name": "src/lib.rs::sweep_anchor",
            }
            digest = "sha256:" + "c" * 64
            calls = []

            def call(tool, arguments, _deadline_ms):
                calls.append((tool, dict(arguments)))
                if tool == "tracedecay_move_symbol" and arguments.get("dry_run") is True:
                    return self.response(f'{{"expected_state":"{digest}"}}')
                if tool == "tracedecay_move_symbol":
                    return self.response(
                        '{"success":true,"replayed":true,"effect_id":"effect.move",'
                        f'"input_digest":"{digest}","committed_state":"{digest}"}}'
                    )
                self.assertEqual(tool, "tracedecay_source_edit_rollback")
                self.assertEqual(arguments["effect_id"], "effect.move")
                source.write_text(original_source)
                relocated.write_text(original_relocated)
                return self.response(
                    '{"success":true,"reconciled":true,"effect_id":"effect.rollback"}'
                )

            prepared = runner.prepare_journey(
                "tracedecay_move_symbol", object(), fixture, lambda _tool: 1_000, call
            )
            source.write_text("")
            relocated.write_text(original_relocated + moved)
            response = self.response(
                '{"success":true,"effect_id":"effect.move",'
                f'"input_digest":"{digest}","committed_state":"{digest}"}}'
            )
            note = prepared.cleanup(response)

        self.assertIn("journaled rollback", note)
        self.assertNotIn("tracedecay_by_qualified_name", [tool for tool, _ in calls])

    def test_rename_apply_copies_the_preview_capability_verbatim(self) -> None:
        runner = load_runner()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("pub fn sweep_anchor() -> i32 { 7 }\n")
            (root / "src/relocated.rs").write_text("pub fn relocation_marker() -> i32 { 0 }\n")
            fixture = {
                "root": str(root),
                "file": "src/lib.rs",
                "symbol": "sweep_anchor",
                "qualified_name": "src/lib.rs::sweep_anchor",
                "node_id": "function:fixture",
            }
            accepted = {
                "preview_id": "sha256:" + "0" * 64,
                "preview_digest": "sha256:" + "1" * 64,
                "plan_digest": "sha256:" + "2" * 64,
                "graph_revision": "sha256:" + "3" * 64,
                "repository_revision": "repository.fixture.v1",
            }
            calls = []

            def call(tool, arguments, _deadline_ms):
                calls.append((tool, dict(arguments)))
                if tool == "tracedecay_rename_preview":
                    return self.response(
                        '{"node":{"id":"function:fixture",'
                        '"qualified_name":"src/lib.rs::sweep_anchor","kind":"function",'
                        '"file":"src/lib.rs","name":"sweep_anchor"}}'
                    )
                self.assertEqual(tool, "tracedecay_rename_symbol")
                self.assertIs(arguments["dry_run"], True)
                self.assertNotIn("accepted_preview", arguments)
                return self.response(json.dumps({
                    **accepted,
                    "expected_state": accepted["preview_digest"],
                }))

            prepared = runner.prepare_journey(
                "tracedecay_rename_symbol", object(), fixture, lambda _tool: 1_000, call
            )

        self.assertEqual(prepared.arguments["accepted_preview"], accepted)
        self.assertEqual(prepared.arguments["expected_state"], accepted["preview_digest"])
        self.assertTrue(prepared.arguments["verify"])
        self.assertEqual([tool for tool, _ in calls], [
            "tracedecay_rename_preview", "tracedecay_rename_symbol",
        ])

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


class FixturePrimingRetryTests(unittest.TestCase):
    @staticmethod
    def response(payload, *, is_error=False):
        return {
            "result": {
                "_meta": {"duration_us": 5},
                "isError": is_error,
                "content": [{"type": "text", "text": payload}],
            }
        }

    @classmethod
    def application_response(cls, family, payload):
        return cls.response(json.dumps({
            "contract": {
                "schema_id": f"schema.application.retained.session-refresh-{family}.result",
                "schema_revision": 1,
            },
            "request_id": "request.fixture",
            "scope": {"project_id": "profile.fixture"},
            "outcome": {"outcome": family, "value": {"payload": payload}},
        }))

    @staticmethod
    def project_route_error(reason_code, *, retryable):
        return {
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32603,
                "message": "tool project route failed: fixture authority is warming",
                "data": {
                    "tool": "tracedecay_by_qualified_name",
                    "reason_code": reason_code,
                    "retryable": retryable,
                    "detail": "fixture authority is warming",
                },
            },
        }

    @classmethod
    def client(cls, qualified_name_responses):
        responses = {
            "tracedecay_node": cls.response(
                '{"node":{"qualified_name":"sweep_anchor","kind":"function"}}'
            ),
            "tracedecay_read": cls.response('{"handle":"rh_fixture"}'),
            "tracedecay_retrieve": cls.response("catalog sweep handle source"),
            "tracedecay_code_symbol_search": cls.response(
                '{"items":['
                '{"name":"sweep_anchor","kind":"function","node_id":"sym:anchor"},'
                '{"name":"sweep_peer","kind":"function","node_id":"sym:peer"},'
                '{"name":"sweep_typed","kind":"function","node_id":"sym:typed"},'
                '{"name":"SweepType","kind":"struct","node_id":"sym:type"}'
                ']}'
            ),
            "tracedecay_git_hunks": cls.response(
                '{"preview_input_id":"preview.fixture","hunks":'
                '[{"digest":"sha256:fixture","hunk":{}}]}'
            ),
            "tracedecay_configuration_list": cls.response(
                '{"payload":[{"key":"work.topology_policy.v1"},'
                '{"key":"diagnostics.prewarm.v1"}]}'
            ),
            "tracedecay_configuration_get": cls.response(
                '{"payload":{"key":"work.topology_policy.v1",'
                '"revision_id":"configuration.fixture.v1",'
                '"effective_value":{"kind":"work_topology_policy",'
                '"value":{"review_topology":{"allowed":'
                '["no_review","independent_review","standard_pull_requests"]}}}}}'
            ),
            "tracedecay_automation_run_list": cls.response(
                '{"runs":[{"run_id":"automation.run.fixture"}]}'
            ),
            "tracedecay_lcm_load_session": cls.response(
                '{"messages":[{"store_id":41,"content":"catalog sweep captured LCM message"}]}'
            ),
            "tracedecay_session_refresh_begin": cls.application_response(
                "effect",
                {
                    "outcome": "started",
                    "handle": "srh_fixture",
                    "operation_id": "refresh.operation.fixture",
                },
            ),
            "tracedecay_session_refresh_status": cls.application_response(
                "evidence",
                {
                    "tool": "tracedecay_session_refresh_status",
                    "outcome": "complete",
                    "receipt": {
                        "operation_id": "refresh.operation.fixture",
                        "state": "complete",
                    },
                },
            ),
            "tracedecay_active_project": cls.response(
                '{"project_id":"project.fixture"}'
            ),
            "tracedecay_configuration_set": cls.response(
                '{"outcome":"effect","value":{"payload":'
                '{"result_revision_id":"configuration.fixture.seeded"}}}'
            ),
            "tracedecay_configuration_unset": cls.response(
                '{"outcome":"effect","value":{"payload":'
                '{"result_revision_id":"configuration.fixture.restored"}}}'
            ),
            "tracedecay_work_create": cls.response('{"replayed":false}'),
            "tracedecay_work_generate_proposal": cls.response(
                '{"proposal":{"id":"proposal.fixture"},'
                '"verified_graph_version":{"graph_version":1}}'
            ),
            "tracedecay_work_accept_proposal": cls.response(
                '{"replayed":false,"verified_graph_version":{"graph_version":2}}'
            ),
            "tracedecay_work_admit_execution": cls.response(
                '{"mutation":{"replayed":false,"verified_graph_version":'
                '{"graph_version":3}},"execution_snapshot":{"snapshot":"fixture"}}'
            ),
            "tracedecay_work_placement_preflight": cls.response('{"blockers":[]}'),
            "tracedecay_work_admit_placement": cls.response(
                '{"identity":{"task_id":"task.fixture","run_id":"run.fixture"}}'
            ),
            "tracedecay_work_start_attempt": cls.response(
                '{"identity":{"attempt_id":"attempt.fixture"}}'
            ),
            "tracedecay_work_attempt_status": cls.response(
                '{"identity":{"attempt_id":"attempt.fixture"}}'
            ),
            "tracedecay_work_cancel_attempt": cls.response(
                '{"identity":{"attempt_id":"attempt.fixture"}}'
            ),
        }

        class Client:
            def __init__(self):
                self.calls = []
                self.qualified_name_responses = list(qualified_name_responses)

            def call_tool(self, name, arguments, _deadline_ms):
                self.calls.append((name, arguments))
                if name == "tracedecay_by_qualified_name":
                    return self.qualified_name_responses.pop(0), 3
                if name == "tracedecay_work_prepare_graph_mutation":
                    change = arguments["change"]["change"]
                    return cls.response(json.dumps({
                        "request": {"fixture_mutation": change},
                    })), 3
                if name == "tracedecay_work_prepare_duplicate_adjudication":
                    return cls.response(json.dumps({
                        **arguments,
                        "command_id": "command.duplicate.fixture",
                    })), 3
                if name in {
                    "tracedecay_work_admit_placement",
                    "tracedecay_work_start_attempt",
                    "tracedecay_work_attempt_status",
                    "tracedecay_work_cancel_attempt",
                }:
                    response = json.loads(
                        responses[name]["result"]["content"][0]["text"]
                    )
                    identity = response["identity"]
                    identity.update({
                        "task_id": arguments["task_id"],
                        "run_id": arguments["run_id"],
                    })
                    if "attempt_id" in arguments:
                        identity["attempt_id"] = arguments["attempt_id"]
                    return cls.response(json.dumps(response)), 3
                if (
                    name == "tracedecay_configuration_get"
                    and arguments["key"] == "diagnostics.prewarm.v1"
                ):
                    return cls.response(
                        '{"payload":{"key":"diagnostics.prewarm.v1",'
                        '"revision_id":"configuration.fixture.v1",'
                        '"effective_value":{"kind":"boolean","value":false}}}'
                    ), 3
                return responses[name], 3

        return Client()

    @staticmethod
    def policies(runner):
        names = (
            "tracedecay_by_qualified_name",
            "tracedecay_node",
            "tracedecay_read",
            "tracedecay_retrieve",
            "tracedecay_code_symbol_search",
            "tracedecay_git_hunks",
            "tracedecay_automation_run_list",
            "tracedecay_lcm_load_session",
            "tracedecay_session_refresh_begin",
            "tracedecay_session_refresh_status",
            "tracedecay_active_project",
            "tracedecay_configuration_set",
            "tracedecay_configuration_unset",
            "tracedecay_configuration_list",
            "tracedecay_configuration_get",
            "tracedecay_work_prepare_graph_mutation",
            "tracedecay_work_create",
            "tracedecay_work_generate_proposal",
            "tracedecay_work_accept_proposal",
            "tracedecay_work_admit_execution",
            "tracedecay_work_placement_preflight",
            "tracedecay_work_admit_placement",
            "tracedecay_work_start_attempt",
            "tracedecay_work_attempt_status",
            "tracedecay_work_cancel_attempt",
            "tracedecay_work_prepare_duplicate_adjudication",
        )
        return {
            name: runner.ToolPolicy(name, "available", "read", 1_000)
            for name in names
        }

    def test_graph_warming_retries_into_the_real_fixture_identity(self) -> None:
        """Cold graph admission must not prevent every catalog journey from starting."""
        runner = load_runner()
        runner.MOUNT_RETRY_DELAY_S = 0.001
        warming = self.project_route_error(
            "code-graph-unavailable", retryable=True
        )
        ready = self.response('{"node_id":"function:fixture"}')
        client = self.client([warming, ready])
        fixture = {
            "symbol": "sweep_anchor",
            "qualified_name": "src/lib.rs::sweep_anchor",
            "session_id": "session.fixture",
            "lcm_message": "catalog sweep captured LCM message",
            "root": "/fixture/root",
            "commit": "a" * 40,
        }

        runner.prime_fixture_values(client, fixture, self.policies(runner))

        qualified_name_calls = [
            arguments for name, arguments in client.calls
            if name == "tracedecay_by_qualified_name"
        ]
        self.assertEqual(
            qualified_name_calls,
            [
                {"qualified_name": "src/lib.rs::sweep_anchor"},
                {"qualified_name": "src/lib.rs::sweep_anchor"},
            ],
        )
        self.assertEqual(fixture["node_id"], "function:fixture")
        self.assertEqual(
            fixture["code_navigation_node_ids"],
            {
                "tracedecay_code_callees": "sym:peer",
                "tracedecay_code_callers": "sym:anchor",
                "tracedecay_code_declaration": "sym:anchor",
                "tracedecay_code_references": "sym:anchor",
                "tracedecay_code_type_definition": "sym:typed",
                "tracedecay_code_type_hierarchy": "sym:type",
            },
        )
        self.assertNotIn("preview_input_id", fixture)
        self.assertNotIn("tracedecay_git_hunks", [name for name, _ in client.calls])
        self.assertEqual(fixture["automation_run_id"], "automation.run.fixture")
        self.assertEqual(fixture["lcm_store_id"], 41)
        self.assertEqual(fixture["session_refresh_handle"], "srh_fixture")
        self.assertEqual(
            fixture["session_refresh_operation_id"],
            "refresh.operation.fixture",
        )
        self.assertEqual(fixture["project_id"], "project.fixture")
        self.assertIn(
            ("tracedecay_active_project", {"format": "json"}),
            client.calls,
        )
        self.assertEqual(fixture["configuration_scalar_value"], {"kind": "boolean", "value": False})
        self.assertEqual(fixture["configuration_revision"], "configuration.fixture.restored")

    def test_delayed_automation_and_lcm_producers_are_consumed(self) -> None:
        runner = load_runner()
        client = self.client([self.response('{"node_id":"function:fixture"}')])
        original = client.call_tool
        attempts = {"tracedecay_automation_run_list": 0, "tracedecay_lcm_load_session": 0}

        def delayed(name, arguments, deadline_ms):
            if name in attempts:
                attempts[name] += 1
                if attempts[name] == 1:
                    if name.endswith("run_list"):
                        return self.response('{"runs":[]}'), 3
                    return self.response(
                        '{"problem":{"kind":"unavailable",'
                        '"code":"application.retained.authority-unavailable"}}',
                        is_error=True,
                    ), 3
            return original(name, arguments, deadline_ms)

        client.call_tool = delayed
        fixture = {
            "symbol": "sweep_anchor",
            "qualified_name": "src/lib.rs::sweep_anchor",
            "session_id": "session.fixture",
            "lcm_message": "catalog sweep captured LCM message",
            "root": "/fixture/root",
            "commit": "a" * 40,
        }
        runner.MOUNT_RETRY_DELAY_S = 0.001

        runner.prime_fixture_values(client, fixture, self.policies(runner))

        self.assertEqual(attempts, {
            "tracedecay_automation_run_list": 2,
            "tracedecay_lcm_load_session": 2,
        })
        self.assertEqual(fixture["automation_run_id"], "automation.run.fixture")
        self.assertEqual(fixture["lcm_store_id"], 41)

    def test_effect_preparation_skips_read_only_automation_and_lcm_producers(self) -> None:
        runner = load_runner()
        client = self.client([self.response('{"node_id":"function:fixture"}')])
        fixture = {
            "symbol": "sweep_anchor",
            "qualified_name": "src/lib.rs::sweep_anchor",
            "session_id": "session.fixture",
            "lcm_message": "catalog sweep captured LCM message",
            "root": "/fixture/root",
            "commit": "a" * 40,
        }

        runner.prime_fixture_values(
            client, fixture, self.policies(runner), "tracedecay_str_replace"
        )

        names = {name for name, _arguments in client.calls}
        self.assertNotIn("tracedecay_automation_run_list", names)
        self.assertNotIn("tracedecay_lcm_load_session", names)
        self.assertNotIn("tracedecay_session_refresh_begin", names)
        self.assertEqual(fixture["work_admitted_version"], {"graph_version": 3})
        self.assertEqual(
            fixture["work_status_arguments"]["attempt_id"],
            fixture["work_attempt_id"],
        )
        self.assertEqual(
            fixture["configuration_rollback_target_revision"],
            "configuration.fixture.seeded",
        )
        self.assertEqual(fixture["configuration_revision"], "configuration.fixture.restored")

    def test_code_navigation_failure_does_not_block_git_work_or_workflow_groups(self) -> None:
        runner = load_runner()
        runner.CODE_INDEX_READY_TIMEOUT_S = 0
        client = self.client([self.response('{"node_id":"function:fixture"}')])
        original_call = client.call_tool

        def missing_code_node(name, arguments, deadline_ms):
            if name == "tracedecay_code_symbol_search":
                return self.response('{"nodes":[]}'), 3
            return original_call(name, arguments, deadline_ms)

        client.call_tool = missing_code_node
        fixture = {
            "symbol": "sweep_anchor",
            "qualified_name": "src/lib.rs::sweep_anchor",
            "session_id": "session.fixture",
            "lcm_message": "catalog sweep captured LCM message",
            "root": "/fixture/root",
            "commit": "a" * 40,
        }
        policies = self.policies(runner)
        policies["tracedecay_workflow_validate_definition"] = runner.ToolPolicy(
            "tracedecay_workflow_validate_definition", "available", "read", 1_000
        )
        original_workflow = runner.prime_workflow_lifecycle

        def mark_workflow_group(*args, **kwargs):
            fixture["workflow_group_reached"] = True

        runner.prime_workflow_lifecycle = mark_workflow_group
        try:
            runner.prime_fixture_values(client, fixture, policies)
        finally:
            runner.prime_workflow_lifecycle = original_workflow

        self.assertIn(
            "navigation identities",
            fixture["priming_errors"]["code_navigation"]["message"],
        )
        self.assertNotIn("preview_input_id", fixture)
        self.assertTrue(fixture["work_attempt_id"].startswith("attempt.tool-sweep."))
        self.assertTrue(fixture["workflow_group_reached"])

    def test_non_retryable_graph_failure_does_not_block_independent_groups(self) -> None:
        """A terminal graph failure only withholds graph-dependent identities."""
        runner = load_runner()
        terminal = self.project_route_error(
            "code-graph-unavailable", retryable=False
        )
        client = self.client([terminal])
        fixture = {
            "symbol": "sweep_anchor",
            "qualified_name": "src/lib.rs::sweep_anchor",
            "session_id": "session.fixture",
            "lcm_message": "catalog sweep captured LCM message",
            "root": "/fixture/root",
            "commit": "a" * 40,
        }

        runner.prime_fixture_values(client, fixture, self.policies(runner))

        qualified_name_calls = [
            name for name, _arguments in client.calls
            if name == "tracedecay_by_qualified_name"
        ]
        self.assertEqual(len(qualified_name_calls), 1)
        self.assertIn("code-graph-unavailable", fixture["priming_errors"]["graph"]["message"])
        self.assertNotIn("node_id", fixture)
        self.assertEqual(fixture["handle"], "rh_fixture")
        self.assertEqual(
            fixture["code_navigation_node_ids"],
            {
                "tracedecay_code_callees": "sym:peer",
                "tracedecay_code_callers": "sym:anchor",
                "tracedecay_code_declaration": "sym:anchor",
                "tracedecay_code_references": "sym:anchor",
                "tracedecay_code_type_definition": "sym:typed",
                "tracedecay_code_type_hierarchy": "sym:type",
            },
        )
        self.assertNotIn("preview_input_id", fixture)
        self.assertTrue(fixture["work_attempt_id"].startswith("attempt.tool-sweep."))


class WorkflowLifecycleTests(unittest.TestCase):
    SHA_A = "sha256:" + "a" * 64
    SHA_B = "sha256:" + "b" * 64
    SHA_C = "sha256:" + "c" * 64

    class Runtime:
        def __init__(self, owner):
            self.owner = owner
            self.definitions = {}
            self.dispositions = {}
            self.runs = {}
            self.handoffs = {}
            self.calls = []
            self.actor = "actor.tool-sweep"
            self.scope = {
                "project_id": "project.fixture",
                "repository_id": "repository.fixture",
                "worktree_id": "worktree.fixture",
                "reference": None,
                "scope_digest": self.owner.SHA_A,
            }

        def response(self, payload):
            return {"payload": payload}

        def call(self, name, arguments, _deadline_ms):
            self.calls.append((name, arguments))
            if name == "tracedecay_configuration_get":
                return {
                    "authority": {"policy": {"digest": self.owner.SHA_A}},
                    "payload": {"effective_behavior_digest": self.owner.SHA_B},
                }
            if name == "tracedecay_workflow_validate_definition":
                return self.response({"definition": arguments["definition"]})
            if name == "tracedecay_workflow_register_definition":
                definition = arguments["definition"]
                key = (definition["definition_id"], definition["definition_version"])
                self.definitions[key] = definition
                self.dispositions.setdefault(key, {"state": "candidate", "revision": 1})
                return self.response(definition)
            if name == "tracedecay_workflow_get_definition":
                return self.response(self.definitions[(arguments["definition_id"], arguments["definition_version"])])
            if name == "tracedecay_workflow_list_definitions":
                return self.response(list(self.definitions.values()))
            if name == "tracedecay_workflow_definition_history":
                return self.response([
                    definition for (identity, _version), definition in self.definitions.items()
                    if identity == arguments["definition_id"]
                ])
            if name == "tracedecay_workflow_diff_definition":
                return self.response({
                    "definition_id": arguments["definition_id"],
                    "from_version": arguments["from_version"],
                    "to_version": arguments["to_version"],
                    "changed_steps": ["step.tool-sweep.inspect"],
                })
            if name in {
                "tracedecay_workflow_activate_definition",
                "tracedecay_workflow_retire_definition",
                "tracedecay_workflow_reject_definition",
            }:
                state, revision = {
                    "tracedecay_workflow_activate_definition": ("active", 3),
                    "tracedecay_workflow_retire_definition": ("retired", 4),
                    "tracedecay_workflow_reject_definition": ("rejected", 2),
                }[name]
                key = (arguments["definition_id"], arguments["definition_version"])
                self.dispositions[key] = {"state": state, "revision": revision}
                return {
                    "payload": {
                        "definition_id": key[0], "definition_version": key[1],
                        "state": state, "revision": revision,
                    },
                    "receipt": {"actor": self.actor, "scope": self.scope},
                }
            if name == "tracedecay_workflow_handoff_issue":
                grant = {
                    "scope": arguments["scope"],
                    "token_digest": self.owner.SHA_B,
                    "issued_at": 10,
                    "expires_at": 60_000_010,
                    "frontier": arguments["frontier"],
                    "frontier_digest": self.owner.SHA_C,
                }
                self.handoffs[arguments["secret"]] = grant
                return self.response(grant)
            if name == "tracedecay_workflow_handoff_redeem":
                grant = self.handoffs.pop(arguments["secret"])
                return self.response({
                    "scope": arguments["expected_scope"],
                    "frontier": grant["frontier"],
                    "frontier_digest": grant["frontier_digest"],
                    "redeemed_at": 20,
                })
            if name == "tracedecay_workflow_start_run":
                run = {"run_id": arguments["run_id"], "status": "running", "sequence": 1}
                self.runs[arguments["run_id"]] = run
                return self.response(run)
            if name == "tracedecay_workflow_get_run":
                return self.response(self.runs[arguments["run_id"]])
            if name in {
                "tracedecay_workflow_pause_run",
                "tracedecay_workflow_resume_run",
                "tracedecay_workflow_cancel_run",
            }:
                state, increment = {
                    "tracedecay_workflow_pause_run": ("paused", 1),
                    "tracedecay_workflow_resume_run": ("running", 1),
                    "tracedecay_workflow_cancel_run": ("cancelled", 2),
                }[name]
                run = self.runs[arguments["run_id"]]
                self.assert_sequence(run, arguments["expected_sequence"])
                run = {**run, "status": state, "sequence": run["sequence"] + increment}
                self.runs[arguments["run_id"]] = run
                return self.response(run)
            raise AssertionError(name)

        @staticmethod
        def assert_sequence(run, expected):
            if run["sequence"] != expected:
                raise AssertionError((run, expected))

        def probe(self, name, arguments, _deadline_ms):
            self.calls.append((name, arguments))
            return {
                "diagnostic": {
                    "code": "workflow.catalog.pin_mismatch",
                    "message": (
                        f"pinned_catalog_digest expected {self.owner.SHA_C}, "
                        f"observed {arguments['definition']['pinned_catalog_digest']}"
                    ),
                }
            }

    @staticmethod
    def fixture():
        return {
            "project_id": "project.fixture",
            "configuration_key": "work.topology_policy.v1",
            "work_execution_snapshot": {
                "route": {"provider_id": "provider.fixture", "route_id": "route.fixture"},
                "backend": "codex_cli",
                "model": "fixture-model",
            },
            "work_task_id": "task.fixture",
            "work_admitted_version": {"graph_version": 4},
            "work_attempt_frontier": {
                "identity": {
                    "task_id": "task.fixture",
                    "run_id": "run.fixture",
                    "attempt_id": "attempt.fixture",
                },
                "state": "cancelled",
                "evidence_digest": None,
            },
        }

    def test_shared_lifecycle_consumes_public_pins_and_reaches_terminal_states(self) -> None:
        runner = load_runner()
        runtime = self.Runtime(self)
        fixture = self.fixture()

        runner.prime_workflow_lifecycle(fixture, runtime.call, runtime.probe, lambda _name: 1_000)

        run = runtime.runs[fixture["workflow_run_id"]]
        self.assertEqual(run["status"], "cancelled")
        self.assertEqual(
            runtime.dispositions[(fixture["workflow_definition_id"], 1)]["state"],
            "retired",
        )
        self.assertEqual(
            runtime.dispositions[(fixture["workflow_definition_id"], 2)]["state"],
            "rejected",
        )
        self.assertEqual(
            fixture["workflow_definition_v1"]["pinned_catalog_digest"], self.SHA_C
        )
        self.assertEqual(
            set(fixture["workflow_read_arguments"]),
            {
                "tracedecay_workflow_validate_definition",
                "tracedecay_workflow_get_definition",
                "tracedecay_workflow_list_definitions",
                "tracedecay_workflow_definition_history",
                "tracedecay_workflow_diff_definition",
                "tracedecay_workflow_get_run",
            },
        )

    def test_pause_effect_is_contained_through_resume_cancel_and_retire(self) -> None:
        runner = load_runner()
        runtime = self.Runtime(self)
        fixture = self.fixture()
        name = "tracedecay_workflow_pause_run"
        runner.prime_workflow_lifecycle(
            fixture, runtime.call, runtime.probe, lambda _name: 1_000, name
        )
        prepared = fixture["workflow_effect_journey"]

        response = runtime.call(name, prepared.arguments, 1_000)
        note = prepared.cleanup(response)

        effect_run = runtime.runs[prepared.arguments["run_id"]]
        self.assertEqual(prepared.settlement, "contained")
        self.assertEqual(effect_run["status"], "cancelled")
        self.assertIn("retired", note)

    def test_every_workflow_effect_has_a_contained_real_journey(self) -> None:
        runner = load_runner()
        for name in sorted(runner.WORKFLOW_LIFECYCLE_EFFECTS):
            with self.subTest(name=name):
                runtime = self.Runtime(self)
                fixture = self.fixture()
                runner.prime_workflow_lifecycle(
                    fixture, runtime.call, runtime.probe, lambda _name: 1_000, name
                )
                prepared = fixture["workflow_effect_journey"]

                response = runtime.call(name, prepared.arguments, 1_000)
                note = prepared.cleanup(response)

                self.assertEqual(prepared.settlement, "contained")
                self.assertTrue(note)


class MountRetryTests(unittest.TestCase):
    """Reads honor typed retryable-unavailable states within one bounded budget."""

    @staticmethod
    def policy(runner, name):
        return runner.ToolPolicy(name=name, availability="available", effect="read", deadline_ms=1_000)

    @staticmethod
    def definition(name):
        return {"name": name, "inputSchema": {"type": "object", "properties": {}, "required": []}}

    @staticmethod
    def scripted_client(responses_by_tool):
        class Client:
            def __init__(self):
                self.calls = []

            def call_tool(self, name, arguments, _deadline_ms):
                self.calls.append((name, arguments))
                queue = responses_by_tool[name]
                response = queue.pop(0) if len(queue) > 1 else queue[0]
                return response, 3

        return Client()

    @staticmethod
    def text_response(text, *, is_error=False):
        return {
            "result": {
                "_meta": {"duration_us": 5},
                "isError": is_error,
                "content": [{"type": "text", "text": text}],
            }
        }

    def test_retryable_unavailable_settles_to_the_real_success(self) -> None:
        """A mounting authority's typed unavailable retries into its real payload."""
        runner = load_runner()
        runner.MOUNT_RETRY_DELAY_S = 0.001
        name = "tracedecay_feedback_advisory_cycle"
        self.assertNotIn(name, runner.EXPECTED_HERMETIC_DENIALS)
        unavailable = self.text_response(
            '{"problem":{"kind":"unavailable","code":"feedback.advisory-cycle.unavailable"}}',
            is_error=True,
        )
        success = self.text_response('{"cycle":{"outcome":"evidence"},"finding_handles":[]}')
        client = self.scripted_client({name: [unavailable, unavailable, success]})

        row = runner._read_tool_row(client, self.definition(name), self.policy(runner, name), fixture={})

        self.assertEqual(row["verdict"], "PASS")
        self.assertEqual(len(client.calls), 3)

    def test_navigation_requires_nonempty_symbol_evidence(self) -> None:
        runner = load_runner()
        name = "tracedecay_code_declaration"
        definition = {
            "name": name,
            "inputSchema": {
                "type": "object",
                "properties": {"node_id": {"type": "string"}},
                "required": ["node_id"],
            },
        }
        fixture = {"code_navigation_node_ids": {name: "symbol:fixture"}}

        empty = self.scripted_client(
            {name: [self.text_response('{"payload":{"items":[]}}')]}
        )
        row = runner._read_tool_row(
            empty, definition, self.policy(runner, name), fixture
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "tool_sweep.navigation_evidence_empty")

        populated = self.scripted_client(
            {name: [self.text_response('{"payload":{"items":[{"node_id":"symbol:fixture"}]}}')]}
        )
        row = runner._read_tool_row(
            populated, definition, self.policy(runner, name), fixture
        )
        self.assertEqual(row["verdict"], "PASS")

    def test_persistent_unavailable_still_fails_after_the_budget(self) -> None:
        """The retry is falsifiable: an authority that never mounts stays a FAIL."""
        runner = load_runner()
        runner.MOUNT_RETRY_BUDGET_S = 0.01
        runner.MOUNT_RETRY_DELAY_S = 0.001
        name = "tracedecay_feedback_advisory_cycle"
        unavailable = self.text_response(
            '{"problem":{"kind":"unavailable","code":"feedback.advisory-cycle.unavailable"}}',
            is_error=True,
        )
        client = self.scripted_client({name: [unavailable]})

        row = runner._read_tool_row(client, self.definition(name), self.policy(runner, name), fixture={})

        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(row["problem_code"], "feedback.advisory-cycle.unavailable")
        self.assertGreater(len(client.calls), 1)

    def test_expected_denials_are_terminal_and_never_retried(self) -> None:
        """An expected hermetic denial is the terminal contract; no retry burns time on it."""
        runner = load_runner()
        name = "tracedecay_test_results"
        kind, code = runner.EXPECTED_HERMETIC_DENIALS[name]
        denial = self.text_response(f'{{"problem":{{"kind":"{kind}","code":"{code}"}}}}', is_error=True)
        client = self.scripted_client({name: [denial]})

        row = runner._read_tool_row(client, self.definition(name), self.policy(runner, name), fixture={})

        self.assertEqual(row["verdict"], "PASS")
        self.assertTrue(row["expected_denial"])
        self.assertEqual(len(client.calls), 1)

    def test_branch_search_no_longer_claims_the_superseded_denial(self) -> None:
        runner = load_runner()
        self.assertNotIn("tracedecay_branch_search", runner.EXPECTED_HERMETIC_DENIALS)

    def test_multi_root_probes_reach_the_exact_daemon_denial(self) -> None:
        """Materialized multi-root bodies parse, so the typed owner denial is exact."""
        runner = load_runner()
        for name in (
            "tracedecay_multi_root_scope_set_compare_and_swap",
            "tracedecay_multi_root_execute",
        ):
            kind, code = runner.EXPECTED_HERMETIC_DENIALS[name]
            self.assertEqual((kind, code), ("unavailable", "multi_root.daemon_unavailable"))
            denial = self.text_response(
                f'{{"problem":{{"kind":"{kind}","code":"{code}"}}}}', is_error=True
            )
            client = self.scripted_client({name: [denial]})
            row = runner._read_tool_row(
                client, self.definition(name), self.policy(runner, name), fixture={}
            )
            self.assertEqual(row["verdict"], "PASS", name)
            self.assertTrue(row["expected_denial"], name)
            self.assertEqual(len(client.calls), 1, name)
            arguments = client.calls[0][1]
            self.assertEqual(arguments["scope_set_id"], "tool-sweep-scope-set.v1", name)

    def test_multi_root_read_no_longer_claims_the_superseded_daemon_denial(self) -> None:
        runner = load_runner()
        name = "tracedecay_multi_root_scope_set_read"
        self.assertNotIn(name, runner.EXPECTED_HERMETIC_DENIALS)
        arguments = runner.materialize_tool_arguments(
            {
                "name": name,
                "inputSchema": {
                    "type": "object",
                    "properties": {"scope_set_id": {"type": "string"}},
                    "required": ["scope_set_id"],
                },
            },
            {},
        )
        self.assertEqual(arguments, {"scope_set_id": "tool-sweep-scope-set.v1"})

    def test_expired_preview_is_reminted_from_the_live_producer(self) -> None:
        """An expired stage-preview cursor re-mints through git_hunks, never a blind replay."""
        runner = load_runner()
        runner.MOUNT_RETRY_DELAY_S = 0.001
        name = "tracedecay_git_preview"
        expired = self.text_response(
            '{"problem":{"kind":"failed","code":"git_index.expired_preview"}}', is_error=True
        )
        minted = self.text_response(
            '{"preview_input_id":"preview.fresh","hunks":[{"digest":"d1","hunk":{}}]}'
        )
        success = self.text_response('{"operation":"stage_hunks","staged":1}')
        client = self.scripted_client({name: [expired, success], "tracedecay_git_hunks": [minted]})
        fixture = {"preview_input_id": "preview.stale", "selected_hunk_digests": '["d0"]'}
        policies = {
            "tracedecay_git_hunks": self.policy(runner, "tracedecay_git_hunks"),
            name: self.policy(runner, name),
        }

        row = runner._read_tool_row(
            client, self.definition(name), self.policy(runner, name), fixture, policies=policies
        )

        self.assertEqual(row["verdict"], "PASS")
        self.assertEqual(fixture["preview_input_id"], "preview.fresh")
        replayed = [arguments for tool, arguments in client.calls if tool == name]
        self.assertEqual(len(replayed), 2)


if __name__ == "__main__":
    unittest.main()
