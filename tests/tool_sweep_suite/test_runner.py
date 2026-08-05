#!/usr/bin/env python3
"""Unit coverage for the catalog-driven MCP tool sweep runner."""

from __future__ import annotations

import json
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
from pathlib import Path

from test_support import load_orchestrator, load_reports, load_runner, load_sweep


def _catalog_definition(
    name: str, effect: str = "read", *, availability: str = "available"
) -> dict[str, object]:
    read_only = effect in {"read", "preview"}
    cancellation = {"mode": "cooperative", "points": ["before_read"]}
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
                "cancellation": cancellation,
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


class ArgumentFixtureTests(unittest.TestCase):
    def test_schema_materialization_uses_real_fixture_tokens(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id="preview-from-producer",
            snapshot_digest="digest-from-producer",
            configuration_revision="7",
            code_node_id="code-function:anchor",
        )
        definition = {
            "name": "tracedecay_example",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string"},
                    "node_id": {"type": "string"},
                    "scope": {
                        "type": "object",
                        "properties": {"generation": {"type": "string"}},
                        "required": ["generation"],
                    },
                },
                "required": ["file", "node_id", "scope"],
            },
        }

        arguments = runner.materialize_arguments(definition, fixture)

        self.assertEqual(arguments["file"], "src/lib.rs")
        self.assertEqual(arguments["node_id"], "code-function:anchor")
        self.assertEqual(arguments["scope"]["generation"], "code-generation:unpinned-latest.v1")

    def test_effect_schema_selects_apply_not_a_generic_dry_run(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id="preview-from-producer",
            snapshot_digest="digest-from-producer",
            configuration_revision="7",
        )
        definition = {
            "name": "tracedecay_source_edit",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "dry_run": {"type": "boolean"},
                    "idempotency_key": {"type": "string"},
                },
                "required": ["path"],
                "allOf": [
                    {
                        "if": {"properties": {"dry_run": {"const": False}}},
                        "then": {"required": ["idempotency_key"]},
                    }
                ],
            },
        }

        arguments = runner.materialize_arguments(definition, fixture, effect="source_edit")

        self.assertEqual(arguments["path"], "src/lib.rs")
        self.assertFalse(arguments["dry_run"])
        self.assertEqual(arguments["idempotency_key"], "tool-sweep-idempotency")

    def test_semantic_requests_choose_lexical_fallback_when_available(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id="preview-from-producer",
            snapshot_digest="digest-from-producer",
            configuration_revision="7",
        )
        definition = {
            "name": "tracedecay_search",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "semantic_mode": {"type": "string", "enum": ["strict_semantic", "fallback_allowed"]},
                },
                "required": ["query", "semantic_mode"],
            },
        }

        arguments = runner.materialize_arguments(definition, fixture)

        self.assertEqual(arguments["semantic_mode"], "fallback_allowed")

    def test_object_any_of_materializes_one_real_required_branch(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id=None,
            snapshot_digest=None,
            configuration_revision=None,
        )
        definition = {
            "name": "tracedecay_message_search",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "goals": {"type": "boolean", "default": False},
                    "format": {"type": "string", "enum": ["markdown", "json"]},
                },
                "required": [],
                "anyOf": [
                    {"required": ["query"]},
                    {
                        "properties": {"goals": {"const": True}},
                        "required": ["goals"],
                    },
                ],
            },
        }

        self.assertEqual(
            runner.materialize_arguments(definition, fixture),
            {"query": "sweep_anchor", "format": "json"},
        )

    def test_optional_selector_schema_uses_fixture_qualified_name(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="src/lib.rs::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id=None,
            snapshot_digest=None,
            configuration_revision=None,
        )
        definition = {
            "name": "tracedecay_signature",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": {"type": "string"},
                    "qualified_name": {"type": "string"},
                    "format": {"type": "string", "enum": ["markdown", "json"]},
                },
            },
        }

        self.assertEqual(
            runner.materialize_arguments(definition, fixture),
            {"qualified_name": "src/lib.rs::sweep_anchor", "format": "json"},
        )

    def test_opaque_inputs_cannot_reuse_an_unrelated_fixture_handle(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id=None,
            snapshot_digest=None,
            configuration_revision="7",
        )

        with self.assertRaisesRegex(runner.SweepError, "feedback request handle"):
            runner.materialize_arguments(
                {
                    "name": "tracedecay_feedback_get",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "request_handle": {
                                "type": "string",
                                "description": "Daemon-minted opaque request handle.",
                            }
                        },
                        "required": ["request_handle"],
                    },
                },
                fixture,
            )
        with self.assertRaisesRegex(runner.SweepError, "repository snapshot"):
            runner.materialize_arguments(
                {
                    "name": "tracedecay_git_preview",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"repository_snapshot": {"type": "object"}},
                        "required": ["repository_snapshot"],
                    },
                },
                fixture,
            )

    def test_response_handle_requires_a_response_handle_schema(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id=None,
            snapshot_digest=None,
            configuration_revision="7",
        )
        definition = {
            "name": "tracedecay_retrieve",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "handle": {
                        "type": "string",
                        "description": "Handle from a truncated MCP response to retrieve.",
                    }
                },
                "required": ["handle"],
            },
        }

        self.assertEqual(runner.materialize_arguments(definition, fixture)["handle"], "response-from-producer")

    def test_effect_digest_requires_an_authentic_producer(self) -> None:
        runner = load_runner()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id=None,
            snapshot_digest=None,
            configuration_revision="7",
        )

        with self.assertRaisesRegex(runner.SweepError, "expected_state"):
            runner.materialize_arguments(
                {
                    "name": "tracedecay_str_replace",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "dry_run": {"type": "boolean"},
                            "expected_state": {"type": "string"},
                        },
                        "required": ["path"],
                        "allOf": [
                            {
                                "if": {"properties": {"dry_run": {"const": False}}},
                                "then": {"required": ["expected_state"]},
                            }
                        ],
                    },
                },
                fixture,
                effect="source_edit",
            )


class TypedStateTests(unittest.TestCase):
    def test_unavailable_requires_structured_state_and_reason(self) -> None:
        runner = load_runner()
        response = {
            "result": {
                "isError": True,
                "content": [
                    {
                        "type": "text",
                        "text": '{"status":"unavailable","reason_code":"authority_unavailable"}',
                    }
                ],
            }
        }

        self.assertTrue(runner.is_typed_unavailable(response))
        self.assertFalse(
            runner.is_typed_unavailable(
                {"result": {"isError": True, "content": [{"type": "text", "text": "unavailable"}]}}
            )
        )
        self.assertFalse(
            runner.is_typed_unavailable(
                {
                    "result": {
                        "content": [
                            {"type": "text", "text": '{"status":"ok","lanes":[{"status":"unavailable","reason_code":"partial"}]}'},
                        ]
                    }
                }
            )
        )
        self.assertTrue(
            runner.is_typed_unavailable(
                {
                    "result": {
                        "isError": True,
                        "content": [
                            {
                                "type": "text",
                                "text": '{"contract":{"schema_id":"schema.application.problem"},'
                                '"problem":{"kind":"unavailable","code":"feedback.advisory_cycle_quarantined"}}',
                            }
                        ],
                    }
                }
            )
        )
        self.assertEqual(
            runner.response_problem_code(
                {
                    "result": {
                        "isError": True,
                        "content": [
                            {
                                "type": "text",
                                "text": '{"problem":{"kind":"unavailable","code":"feedback.advisory_cycle_quarantined"}}',
                            }
                        ],
                    }
                }
            ),
            "feedback.advisory_cycle_quarantined",
        )

    def test_effect_denial_requires_structured_state_and_reason(self) -> None:
        runner = load_runner()

        self.assertTrue(
            runner.is_typed_denial(
                {"result": {"content": [{"type": "text", "text": '{"status":"denied","reason_code":"policy_denied"}'}]}}
            )
        )
        self.assertFalse(runner.is_typed_denial({"result": {"content": [{"type": "text", "text": "denied"}]}}))


class WorkspaceSnapshotTests(unittest.TestCase):
    def test_workspace_digest_excludes_private_runtime_state(self) -> None:
        runner = load_runner()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("pub fn sweep() {}\n")
            (root / ".tracedecay").mkdir()
            (root / ".tracedecay/state.db").write_text("first")
            before = runner.workspace_digest(root)
            (root / ".tracedecay/state.db").write_text("second")

            self.assertEqual(before, runner.workspace_digest(root))


class ArtifactTests(unittest.TestCase):
    def test_json_and_junit_keep_timeout_failures_visible(self) -> None:
        reports = load_reports()
        report = {
            "tools": [
                {"name": "tracedecay_fast", "verdict": "PASS", "p95_ms": 10, "max_ms": 11},
                {
                    "name": "tracedecay_slow",
                    "verdict": "TIMEOUT",
                    "p95_ms": None,
                    "max_ms": None,
                    "note": "cancellation did not settle",
                },
            ]
        }
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            reports.write_reports(out, report)

            self.assertEqual(json.loads((out / "results.json").read_text()), report)
            self.assertEqual(json.loads((out / "cancellations.json").read_text()), [])
            suite = ET.parse(out / "junit.xml").getroot()
            self.assertEqual(suite.attrib["failures"], "1")
            failure = suite.find("testcase[@name='tracedecay_slow']/failure")
            self.assertIsNotNone(failure)
            self.assertIn("cancellation did not settle", failure.text or "")

    def test_cancellation_artifact_keeps_actual_request_identity(self) -> None:
        reports = load_reports()
        report = {
            "tools": [
                {
                    "name": "tracedecay_slow",
                    "verdict": "TIMEOUT",
                    "cancellation": [
                        {
                            "request_id": 41,
                            "timed_out": True,
                            "sent": True,
                            "settled": False,
                            "transport_error": "cancellation did not settle",
                        }
                    ],
                }
            ]
        }
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            reports.write_reports(out, report)

            self.assertEqual(
                json.loads((out / "cancellations.json").read_text()),
                [{"tool": "tracedecay_slow", **report["tools"][0]["cancellation"][0]}],
            )

    def test_junit_keeps_a_pre_catalog_fatal_visible(self) -> None:
        reports = load_reports()
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            reports.write_reports(out, {"tools": [], "fatal": "tools/list timed out"})

            suite = ET.parse(out / "junit.xml").getroot()
            self.assertEqual(suite.attrib["failures"], "1")
            self.assertIsNotNone(suite.find("testcase[@name='catalog-completion']/failure"))


class SweepSchedulingTests(unittest.TestCase):
    def test_read_shards_are_name_stable_and_catalog_complete(self) -> None:
        sweep = load_sweep()
        definitions = [{"name": name} for name in ("tracedecay_a", "tracedecay_b", "tracedecay_c")]

        partitions = sweep._partition(definitions, 2)

        assigned = [definition["name"] for partition in partitions for definition in partition]
        self.assertCountEqual(assigned, [definition["name"] for definition in definitions])
        self.assertEqual(partitions, sweep._partition(definitions, 2))

    def test_effect_receipt_is_structural_not_human_prose(self) -> None:
        sweep = load_sweep()

        self.assertTrue(sweep._response_has_effect_receipt({"result": {"content": [{"text": '{"receipt_id":"r1"}'}]}}))
        self.assertFalse(sweep._response_has_effect_receipt({"result": {"content": [{"text": "effect completed"}]}}))

    def test_effect_metadata_matches_read_only_effect_semantics(self) -> None:
        sweep = load_sweep()

        mismatch = sweep._metadata_matches_annotations(
            {"annotations": {"readOnlyHint": False}},
            SimpleNamespace(availability_state="available", effect="read"),
        )

        self.assertEqual(
            mismatch,
            "readOnlyHint=false conflicts with dispatch effect=read; expected readOnlyHint=true",
        )
        self.assertEqual(
            sweep._metadata_matches_annotations(
                {"annotations": {"readOnlyHint": True}},
                SimpleNamespace(availability_state="available", effect="administrative"),
            ),
            "readOnlyHint=true conflicts with dispatch effect=administrative; expected readOnlyHint=false",
        )
        self.assertIsNone(
            sweep._metadata_matches_annotations(
                {"annotations": {"readOnlyHint": True}},
                SimpleNamespace(availability_state="available", effect="preview"),
            )
        )

    def test_effect_phase_selects_only_the_requested_dynamic_effect(self) -> None:
        sweep = load_sweep()
        read = SimpleNamespace(availability_state="available", effect="read")
        effect = SimpleNamespace(availability_state="available", effect="administrative")
        unavailable = SimpleNamespace(availability_state="unavailable", effect="read")
        runnable = [
            ({"name": "tracedecay_read"}, read),
            ({"name": "tracedecay_effect"}, effect),
            ({"name": "tracedecay_unavailable"}, unavailable),
        ]

        selected = sweep._select_phase_effect(runnable, "tracedecay_effect")

        self.assertEqual(selected, ({"name": "tracedecay_effect"}, effect))
        with self.assertRaisesRegex(RuntimeError, "not an available mutating tool"):
            sweep._select_phase_effect(runnable, "tracedecay_read")

    def test_listed_unavailable_tool_executes_its_typed_unavailable_path(self) -> None:
        runner = load_runner()
        sweep = load_sweep()

        class Client:
            def call_tool(self, *_args):
                return SimpleNamespace(
                    request_id=44,
                    elapsed_ms=4,
                    response={
                        "error": {
                            "data": {"reason_code": "mcp_dispatch_effect_journey_unverified"}
                        }
                    },
                    timed_out=False,
                    cancellation_sent=False,
                    cancellation_settled=True,
                    transport_error=None,
                    client_queue_ms=0,
                )

        runtime = SimpleNamespace(
            timing_summary=runner.timing_summary,
            tool_error=runner.tool_is_error,
            typed_unavailable=runner.is_typed_unavailable,
            typed_deadline=runner.is_typed_deadline,
        )
        unavailable = SimpleNamespace(
            availability_state="unavailable",
            availability_reason="effect_journey_unverified",
            effect="administrative",
            deadline_ms=1_000,
        )

        row = sweep._invoke_unavailable(
            {"name": "tracedecay_lcm_doctor"}, unavailable, Client(), runtime, SimpleNamespace()
        )

        self.assertEqual(row.verdict, "PASS")
        self.assertEqual(row.rollback, "not_required")
        self.assertEqual(row.samples_ms, [4])

    def test_orchestrator_derives_effect_targets_from_the_negotiated_catalog(self) -> None:
        runner = load_runner()
        orchestrator = load_orchestrator()
        tools = [
            _catalog_definition("tracedecay_read"),
            *(
                _catalog_definition(name, "administrative")
                for name in (
                    "tracedecay_dashboard",
                    "tracedecay_fact_store",
                    "tracedecay_session_start",
                    "tracedecay_session_end",
                    "tracedecay_future_mutation",
                )
            ),
        ]

        self.assertEqual(
            orchestrator.effect_targets(runner.catalog_manifest(tools)),
            [
                "tracedecay_dashboard",
                "tracedecay_fact_store",
                "tracedecay_session_end",
                "tracedecay_session_start",
            ],
        )

    def test_orchestrator_emits_a_failure_for_an_unjourneyed_mutation(self) -> None:
        runner = load_runner()
        orchestrator = load_orchestrator()
        tools = [
            _catalog_definition("tracedecay_read"),
            _catalog_definition("tracedecay_future_mutation", "administrative"),
        ]

        report = orchestrator.merge_phase_reports(
            runner.catalog_manifest(tools),
            {"tools": [{"name": "tracedecay_read", "verdict": "PASS"}]},
            {},
            {"reads": {"returncode": 0, "launch_error": None}},
        )

        row = next(
            row for row in report["tools"] if row["name"] == "tracedecay_future_mutation"
        )
        self.assertEqual(row["verdict"], "FAIL")
        self.assertEqual(
            row["note"],
            "advertised mutation has no real producer/consumer/rollback journey",
        )

    def test_orchestrator_marks_an_omitted_effect_phase_as_failure(self) -> None:
        runner = load_runner()
        orchestrator = load_orchestrator()
        tools = [
            _catalog_definition("tracedecay_read"),
            *(
                _catalog_definition(name, "administrative")
                for name in (
                    "tracedecay_dashboard",
                    "tracedecay_fact_store",
                    "tracedecay_session_start",
                    "tracedecay_session_end",
                )
            ),
        ]
        report = orchestrator.merge_phase_reports(
            runner.catalog_manifest(tools),
            {"tools": [{"name": "tracedecay_read", "verdict": "PASS"}]},
            {},
            {},
        )

        row = next(row for row in report["tools"] if row["name"] == "tracedecay_dashboard")
        self.assertEqual(row["verdict"], "FAIL")
        self.assertIn("omitted", row["note"])

    def test_orchestrator_rejects_a_nonzero_phase_even_if_its_report_looks_green(self) -> None:
        orchestrator = load_orchestrator()
        errors: list[str] = []

        orchestrator._phase_execution_errors(
            {"reads"},
            {"reads": {"returncode": 1, "launch_error": None}},
            errors,
        )

        self.assertEqual(errors, ["reads phase exited nonzero: 1"])

    def test_available_effect_denial_preserves_cleanup_failure(self) -> None:
        runner = load_runner()
        sweep = load_sweep()

        class DeniedClient:
            def call_tool(self, *_args):
                return SimpleNamespace(
                    request_id=91,
                    elapsed_ms=4,
                    response={
                        "result": {
                            "content": [
                                {"type": "text", "text": '{"status":"denied","reason_code":"policy_denied"}'}
                            ]
                        }
                    },
                    timed_out=False,
                    cancellation_sent=False,
                    cancellation_settled=True,
                    transport_error=None,
                    client_queue_ms=0,
                )

        def failed_cleanup(_response):
            raise RuntimeError("inverse unavailable")

        runtime = SimpleNamespace(
            argument_materializer=lambda *_args, **_kwargs: self.fail("effect used generic argument materializer"),
            timing_summary=runner.timing_summary,
            tool_error=runner.tool_is_error,
            typed_unavailable=runner.is_typed_unavailable,
            typed_denial=runner.is_typed_denial,
            effect_journey=lambda *_args: SimpleNamespace(
                arguments={},
                calls=[],
                allow_no_repository_change=True,
                verify_success=lambda _response: None,
                cleanup=failed_cleanup,
            ),
        )
        policy = SimpleNamespace(
            deadline_ms=1_000,
            effect="configuration_write",
            availability_state="available",
        )
        with tempfile.TemporaryDirectory() as temporary:
            fixture = SimpleNamespace(root=Path(temporary))
            with patch.object(sweep, "fixture_state", return_value="before"):
                row = sweep._invoke_effect(
                    {"name": "tracedecay_configuration_set", "inputSchema": {"type": "object"}},
                    policy,
                    DeniedClient(),
                    runtime,
                    fixture,
                    SimpleNamespace(),
                    lambda _tool: 1_000,
                )

        self.assertEqual(row.verdict, "FAIL")
        self.assertEqual(
            row.note,
            "advertised available effect returned typed denial; "
            "real journey rollback failed: inverse unavailable",
        )
        self.assertEqual(row.rollback, "failed")

    def test_available_effect_failure_reports_typed_problem_code(self) -> None:
        """A mounted effect becoming unavailable must leave its cause in the artifact."""
        runner = load_runner()
        sweep = load_sweep()

        class UnavailableClient:
            def call_tool(self, *_args):
                return SimpleNamespace(
                    request_id=92,
                    elapsed_ms=4,
                    response={
                        "result": {
                            "isError": True,
                            "content": [
                                {
                                    "type": "text",
                                    "text": (
                                        '{"problem":{"kind":"unavailable",'
                                        '"code":"effect.backing-service-unavailable"}}'
                                    ),
                                }
                            ],
                        }
                    },
                    timed_out=False,
                    cancellation_sent=False,
                    cancellation_settled=True,
                    transport_error=None,
                    client_queue_ms=0,
                )

        runtime = SimpleNamespace(
            timing_summary=runner.timing_summary,
            tool_error=runner.tool_is_error,
            typed_unavailable=runner.is_typed_unavailable,
            typed_denial=runner.is_typed_denial,
            problem_code=runner.response_problem_code,
            effect_journey=lambda *_args: SimpleNamespace(
                arguments={},
                calls=[],
                allow_no_repository_change=True,
                verify_success=lambda _response: None,
                cleanup=lambda _response: "cleanup verified",
            ),
        )
        policy = SimpleNamespace(
            deadline_ms=1_000,
            effect="administrative",
            availability_state="available",
        )
        with tempfile.TemporaryDirectory() as temporary:
            fixture = SimpleNamespace(root=Path(temporary))
            with patch.object(sweep, "fixture_state", return_value="unchanged"):
                row = sweep._invoke_effect(
                    {"name": "tracedecay_dashboard", "inputSchema": {"type": "object"}},
                    policy,
                    UnavailableClient(),
                    runtime,
                    fixture,
                    SimpleNamespace(),
                    lambda _tool: 1_000,
                )

        self.assertEqual(row.verdict, "FAIL")
        self.assertEqual(
            row.note,
            "available effect returned error or unavailable state: effect.backing-service-unavailable",
        )
        self.assertEqual(row.rollback, "cleanup verified")

    def test_unregistered_effect_fails_without_a_generic_call(self) -> None:
        runner = load_runner()
        sweep = load_sweep()

        class NeverCalledClient:
            def call_tool(self, *_args):
                raise AssertionError("unregistered effect reached tools/call")

        runtime = SimpleNamespace(
            effect_journey=lambda *_args: (_ for _ in ()).throw(
                RuntimeError("tracedecay_unknown_effect: no real producer/consumer journey registered")
            ),
            timing_summary=runner.timing_summary,
            tool_error=runner.tool_is_error,
            typed_unavailable=runner.is_typed_unavailable,
            typed_denial=runner.is_typed_denial,
        )
        policy = SimpleNamespace(
            deadline_ms=1_000,
            effect="administrative",
            availability_state="available",
        )
        with tempfile.TemporaryDirectory() as temporary:
            row = sweep._invoke_effect(
                {"name": "tracedecay_unknown_effect", "inputSchema": {"type": "object"}},
                policy,
                NeverCalledClient(),
                runtime,
                SimpleNamespace(root=Path(temporary)),
                SimpleNamespace(),
                lambda _tool: 1_000,
            )

        self.assertEqual(row.verdict, "FAIL")
        self.assertIn("no real producer/consumer journey registered", row.note)
        self.assertEqual(row.rollback, "not_started")

    def test_timeout_checks_same_and_fresh_client_health_before_worker_leak_verdict(self) -> None:
        runner = load_runner()
        sweep = load_sweep()
        fixture = runner.FixtureLedger(
            file="src/lib.rs",
            directory="src",
            symbol="sweep_anchor",
            node_id="function:anchor",
            peer_node_id="function:peer",
            qualified_name="crate::sweep_anchor",
            branch="main",
            head="a" * 40,
            previous_head="b" * 40,
            session_id="session-from-hook",
            response_handle="response-from-producer",
            preview_id="preview-from-producer",
            snapshot_digest="digest-from-producer",
            configuration_revision="7",
        )

        class TimedOutClient:
            def call_tool(self, *_args):
                return SimpleNamespace(
                    request_id=77,
                    elapsed_ms=9,
                    response=None,
                    timed_out=True,
                    cancellation_sent=True,
                    cancellation_settled=False,
                    transport_error=None,
                )

            def ping(self, _timeout_ms):
                return False

        class FreshClient:
            def list_tools(self, _timeout_ms):
                return []

            def ping(self, _timeout_ms):
                return True

            def close(self):
                return None

        class Factory:
            def open(self, _label):
                return FreshClient()

        runtime = SimpleNamespace(
            argument_materializer=runner.materialize_arguments,
            timing_summary=runner.timing_summary,
            tool_error=runner.tool_is_error,
            typed_unavailable=runner.is_typed_unavailable,
            typed_denial=runner.is_typed_denial,
        )
        definition = {"name": "tracedecay_timeout", "inputSchema": {"type": "object"}}
        policy = SimpleNamespace(
            deadline_ms=1,
            effect="read",
            availability_state="available",
        )

        row = sweep._invoke_read(definition, policy, TimedOutClient(), runtime, fixture, Factory())

        self.assertEqual(row.verdict, "WORKER_LEAK")
        self.assertEqual(row.cancellation[0]["request_id"], 77)
        self.assertFalse(row.same_client_healthy)
        self.assertTrue(row.fresh_client_healthy)

    def test_live_runner_rejects_any_profile_outside_its_artifact_root(self) -> None:
        sweep = load_sweep()
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            inside = out / "isolated"
            inside.mkdir()
            harness = out / "daemon-harness"
            harness.mkdir()
            environment = {
                "TRACEDECAY_DAEMON_HARNESS_ACTIVE": "1",
                "TRACEDECAY_DAEMON_HARNESS_ROOT": str(harness),
                "HOME": str(inside),
                "CODEX_HOME": str(inside),
                "XDG_CONFIG_HOME": str(inside),
                "XDG_DATA_HOME": str(inside),
                "XDG_STATE_HOME": str(inside),
                "TMPDIR": str(inside),
                "TRACEDECAY_PROFILE_DIR": str(inside),
                "TRACEDECAY_DATA_DIR": "/home/zack/.tracedecay",
                "TRACEDECAY_GLOBAL_DB": str(harness / "global.db"),
                "TRACEDECAY_DAEMON_SOCKET": str(harness / "daemon.sock"),
            }

            with patch.dict("os.environ", environment, clear=True):
                with self.assertRaisesRegex(RuntimeError, "outside isolated daemon harness root"):
                    sweep.require_isolated_runtime(SimpleNamespace(out=out))
