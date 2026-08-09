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

    def test_denial_probes_exist_only_for_declared_denial_verdicts(self) -> None:
        """A probe may prove a deny path; it may never stand in for a producible input."""
        runner = load_runner()

        for name in runner.HERMETIC_DENIAL_PROBE_ARGUMENTS:
            self.assertIn(name, runner.EXPECTED_HERMETIC_DENIALS)

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
            definition, {"node_id": "function:graph", "code_node_id": "sym:code"}
        )
        self.assertEqual(arguments["node_id"], "sym:code")
        with self.assertRaises(runner.SweepError):
            runner.materialize_tool_arguments(definition, {"node_id": "function:graph"})


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

    @staticmethod
    def response(payload: str):
        return {"result": {"content": [{"type": "text", "text": payload}]}}

    def test_fact_feedback_journey_requires_a_real_trust_change(self) -> None:
        """Helpful feedback must move the seeded fact's trust, then remove the fact."""
        runner = load_runner()
        state = {"trust": 0.5, "removed": False}

        def call(tool, arguments, _deadline_ms):
            self.assertEqual(tool, "tracedecay_fact_store")
            action = arguments["action"]
            if action == "add":
                return self.response(
                    '{"fact":{"fact_id":7,"content":"' + arguments["content"] + '"}}'
                )
            if action == "get":
                return self.response(
                    '{"fact":{"fact_id":7,"trust_score":' + str(state["trust"]) + "}}"
                )
            if action == "remove":
                state["removed"] = True
                return self.response('{"removed":true}')
            raise AssertionError(action)

        prepared = runner.prepare_journey(
            "tracedecay_fact_feedback", object(), {}, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments["fact_id"], 7)
        self.assertEqual(prepared.arguments["action"], "helpful")

        with self.assertRaises(Exception):
            prepared.cleanup(self.response('{"status":"recorded"}'))
        self.assertFalse(state["removed"])

        state["trust"] = 0.55
        note = prepared.cleanup(self.response('{"status":"recorded"}'))
        self.assertIn("trust", note)
        self.assertTrue(state["removed"])

    def test_memory_status_journey_counts_the_seeded_fact(self) -> None:
        """The repaired status must truthfully count the seeded fact before rollback."""
        runner = load_runner()

        def call(tool, arguments, _deadline_ms):
            self.assertEqual(tool, "tracedecay_fact_store")
            if arguments["action"] == "add":
                return self.response(
                    '{"fact":{"fact_id":3,"content":"' + arguments["content"] + '"}}'
                )
            return self.response('{"removed":true}')

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
        """The refresh rollback is a receipt-backed durable cancel, verified terminal."""
        runner = load_runner()
        state = {"terminal": False}

        def call(_tool, arguments, _deadline_ms):
            if arguments["action"] == "cancel":
                state["terminal"] = True
                return self.response(
                    '{"outcome":"cancelled","receipt":{"operation_id":"refresh.op","state":"cancelled"}}'
                )
            self.assertEqual(arguments["action"], "status")
            if not state["terminal"]:
                return self.response('{"outcome":"running","progress":{"operation_id":"refresh.op"}}')
            return self.response(
                '{"outcome":"cancelled","receipt":{"operation_id":"refresh.op","state":"cancelled"}}'
            )

        prepared = runner.prepare_journey(
            "tracedecay_session_refresh", object(), {}, lambda _tool: 1_000, call
        )
        self.assertEqual(prepared.arguments["action"], "start")

        with self.assertRaises(Exception):
            prepared.cleanup(self.response('{"outcome":"running"}'))

        note = prepared.cleanup(
            self.response(
                '{"outcome":"started","handle":"srh_fixture","operation_id":"refresh.op"}'
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

    def test_expected_denial_is_reached_through_its_mounting_window(self) -> None:
        """Foreign retryable unavailability retries into the exact cataloged denial."""
        runner = load_runner()
        runner.MOUNT_RETRY_DELAY_S = 0.001
        name = "tracedecay_branch_search"
        kind, code = runner.EXPECTED_HERMETIC_DENIALS[name]
        self.assertEqual((kind, code), ("unavailable", "search_failed"))
        mounting = self.text_response(
            '{"status":"unavailable","reason":"search_capacity_unavailable","retryable":true}',
            is_error=True,
        )
        terminal = self.text_response(
            '{"status":"unavailable","reason":"search_failed","retryable":false}',
            is_error=True,
        )
        client = self.scripted_client({name: [mounting, terminal]})

        row = runner._read_tool_row(client, self.definition(name), self.policy(runner, name), fixture={})

        self.assertEqual(row["verdict"], "PASS")
        self.assertTrue(row["expected_denial"])
        self.assertEqual(row["problem_code"], code)
        self.assertEqual(len(client.calls), 2)

    def test_branch_diff_alone_carries_the_extended_mount_budget(self) -> None:
        """The slow code-index branch authority gets a bounded per-tool budget."""
        runner = load_runner()
        self.assertEqual(
            runner.MOUNT_RETRY_BUDGET_OVERRIDES_S,
            {"tracedecay_branch_diff": 180},
        )
        self.assertGreater(
            runner.MOUNT_RETRY_BUDGET_OVERRIDES_S["tracedecay_branch_diff"],
            runner.MOUNT_RETRY_BUDGET_S,
        )

    def test_multi_root_probes_reach_the_exact_daemon_denial(self) -> None:
        """Materialized multi-root bodies parse, so the typed owner denial is exact."""
        runner = load_runner()
        for name in (
            "tracedecay_multi_root_scope_set_read",
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

    def test_branch_diff_diffs_the_real_fixture_branch(self) -> None:
        """The runtime-required base/head come from the fixture's real branch."""
        runner = load_runner()
        arguments = runner.materialize_tool_arguments(
            self.definition("tracedecay_branch_diff"), {"branch": "main"}
        )
        self.assertEqual(
            arguments, {"base": "main", "head": "main", "format": "json"}
        )

    def test_status_reason_payloads_parse_as_typed_problems(self) -> None:
        """Branch surfaces render status+reason; the parser returns them typed."""
        runner = load_runner()
        kind, code = runner.response_problem_code(
            self.text_response(
                '{"status":"unavailable","reason":"search_capacity_unavailable","retryable":true}',
                is_error=True,
            )
        )
        self.assertEqual((kind, code), ("unavailable", "search_capacity_unavailable"))

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
