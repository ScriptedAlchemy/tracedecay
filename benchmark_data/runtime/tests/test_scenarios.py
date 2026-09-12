#!/usr/bin/env python3
"""Contract tests for deterministic runtime scenario definitions."""

from __future__ import annotations

import json
import re
import unittest
from dataclasses import fields

from benchmark_data.runtime.schema import SCHEMA_VERSION, validate_sample
from benchmark_data.runtime.scenarios import (
    SCENARIOS,
    WORKLOADS,
    CapabilityStatus,
    DigestSemantics,
    WorkloadInputs,
    stable_digest,
    validate_stable_id,
)


class ScenarioCatalogTest(unittest.TestCase):
    def test_argument_factories_match_required_stable_shapes(self) -> None:
        inputs = WorkloadInputs(
            symbol="stable_symbol",
            literal="stable::literal",
            node_id="node-123",
            code_generation="generation-456",
            phrase="stable phrase",
            query="stable query",
            session_query="session sentinel",
            provider="codex",
            session_id="session-789",
            payload_size=32,
        )
        by_id = {workload.id: workload for workload in WORKLOADS}

        self.assertEqual(
            by_id["exact-symbol"].arguments(inputs),
            {"name": "stable_symbol", "limit": 20, "format": "json"},
        )
        self.assertEqual(
            set(by_id["exact-occurrence"].arguments(inputs)),
            {"literal", "scope", "meta", "format"},
        )
        self.assertEqual(
            by_id["exact-occurrence"].arguments(inputs)["scope"]["generation"],
            "generation-456",
        )
        self.assertEqual(
            by_id["lexical-phrase"].arguments(inputs)["phrases"],
            ["stable phrase"],
        )
        self.assertEqual(
            by_id["graph-callers"].arguments(inputs)["node_id"],
            "node-123",
        )
        self.assertEqual(
            by_id["session-expand-query"].arguments(inputs)["session_id"],
            "session-789",
        )
        self.assertEqual(
            len(by_id["payload-stress"].arguments(inputs)["keywords"][0]),
            32,
        )

    def test_argument_factories_return_fresh_values(self) -> None:
        workload = next(item for item in WORKLOADS if item.id == "query-context")

        first = workload.arguments()
        first["keywords"].append("mutation")
        second = workload.arguments()

        self.assertNotIn("mutation", second["keywords"])

    def test_cli_argv_is_canonical_and_does_not_require_a_shell(self) -> None:
        workload = next(item for item in WORKLOADS if item.id == "exact-symbol")

        argv = workload.cli_argv()

        self.assertEqual(argv[:3], ("tool", workload.tool, "--args"))
        self.assertEqual(
            json.loads(argv[3]),
            workload.arguments(),
        )
        self.assertNotIn("sh", argv)

    def test_capability_assessment_distinguishes_unavailable_and_unsupported(self) -> None:
        scenario = next(
            item for item in SCENARIOS if item.workload_id == "session-expand-query"
        )
        required = set(scenario.required_capabilities)
        tool = "tracedecay_lcm_expand_query"

        unavailable = scenario.assess_capabilities(available=required - {tool})
        unsupported = scenario.assess_capabilities(
            available=required - {tool},
            unsupported={tool},
        )
        available = scenario.assess_capabilities(available=required)

        self.assertEqual(unavailable.status, CapabilityStatus.UNAVAILABLE)
        self.assertEqual(unavailable.missing, (tool,))
        self.assertEqual(unsupported.status, CapabilityStatus.UNSUPPORTED)
        self.assertEqual(unsupported.unsupported, (tool,))
        self.assertEqual(available.status, CapabilityStatus.AVAILABLE)
        self.assertTrue(available.runnable)

    def test_capability_assessment_preserves_partial_and_failed_states(self) -> None:
        scenario = next(
            item for item in SCENARIOS if item.workload_id == "session-expand-query"
        )
        required = set(scenario.required_capabilities)
        tool = "tracedecay_lcm_expand_query"

        partial = scenario.assess_capabilities(
            available=required,
            partial={tool},
        )
        failed = scenario.assess_capabilities(
            available=required,
            failed={tool},
        )

        self.assertEqual(partial.status, CapabilityStatus.PARTIAL)
        self.assertEqual(partial.partial, (tool,))
        self.assertFalse(partial.runnable)
        self.assertEqual(failed.status, CapabilityStatus.FAILED)
        self.assertEqual(failed.failed, (tool,))
        self.assertFalse(failed.runnable)

    def test_stable_digests_ignore_timing_but_respect_order_semantics(self) -> None:
        first = {
            "results": [{"id": "a"}, {"id": "b"}],
            "_meta": {"duration_us": 1},
        }
        reordered = {
            "_meta": {"duration_us": 99},
            "results": [{"id": "b"}, {"id": "a"}],
        }

        self.assertNotEqual(
            stable_digest(first, DigestSemantics.ORDERED_JSON),
            stable_digest(reordered, DigestSemantics.ORDERED_JSON),
        )
        self.assertEqual(
            stable_digest(first, DigestSemantics.UNORDERED_JSON),
            stable_digest(reordered, DigestSemantics.UNORDERED_JSON),
        )

    def test_ids_reject_delivery_stage_and_milestone_vocabulary(self) -> None:
        serialized_catalog = json.dumps(
            {
                "workloads": [
                    {
                        "id": workload.id,
                        "journey_id": workload.journey_id,
                        "lanes": [
                            lane.value for lane in workload.supported_crate_lanes
                        ],
                    }
                    for workload in WORKLOADS
                ],
                "scenarios": [
                    {
                        "id": scenario.id,
                        "journey_id": scenario.journey_id,
                        "workload_id": scenario.workload_id,
                        "crate_lane": scenario.crate_lane.value,
                    }
                    for scenario in SCENARIOS
                ],
            },
            sort_keys=True,
        )

        self.assertIsNone(
            re.search(r"(?:^|[^a-z])pr[-_]?\d+|milestone|stage[-_]?\d+", serialized_catalog)
        )
        for rejected in ("pr14-runtime", "PR-19", "milestone-3", "stage-12"):
            with self.subTest(rejected=rejected):
                with self.assertRaises(ValueError):
                    validate_stable_id(rejected)
        self.assertNotIn("budget", {field.name for field in fields(type(SCENARIOS[0]))})

    def test_abba_sample_identity_is_raw_sample_schema_compatible(self) -> None:
        scenario = SCENARIOS[0]
        identity = scenario.sample_identity(
            run_id="run-final-v2",
            variant="baseline",
            machine_fingerprint="machine-stable",
            round_index=4,
            abba_position=3,
        )
        digest = "0" * 64
        sample = {
            "schema_version": SCHEMA_VERSION,
            "identity": identity,
            "evidence": {
                "sample_count": 1,
                "evidence_class": "regression_sample",
            },
            "availability": {"state": "available", "detail": None},
            "timing": {
                "started_ns": 10,
                "elapsed_ns": 20,
                "cli_wall_ns": 20,
                "mcp_wall_ns": None,
                "hook_wall_ns": None,
                "host_wall_ns": None,
                "handler_us": None,
                "daemon_us": None,
                "admission_us": None,
                "stages_us": {},
                "shutdown_total_ns": None,
                "abort_offset_ns": None,
            },
            "size": {
                "process_count": 1,
                "request_bytes": 30,
                "response_bytes": 40,
                "content_bytes": 10,
            },
            "lifecycle": {
                "timeout_phase": None,
                "activation_state": "not_applicable",
                "restart_state": "not_applicable",
                "daemon_survived": True,
            },
            "observations": {},
            "outcome": {
                "status": "success",
                "expected_digest": digest,
                "actual_digest": digest,
                "result_digest": digest,
                "error": None,
            },
        }

        self.assertIs(validate_sample(sample), sample)
        self.assertEqual(identity["crate_id"], scenario.crate_lane.value)
        self.assertEqual(identity["journey_id"], scenario.journey_id)
        self.assertEqual(identity["workload_id"], scenario.workload_id)
        self.assertEqual(identity["round_index"], 4)
        self.assertEqual(identity["abba_position"], 3)

    def test_remote_journeys_require_committed_mounted_production_routes(self) -> None:
        remote = next(scenario for scenario in SCENARIOS if scenario.is_remote)

        contract_only = remote.assess_production_route(
            committed=False,
            mounted=False,
            contract_only=True,
        )
        unwired = remote.assess_production_route(
            committed=True,
            mounted=False,
        )
        failed = remote.assess_production_route(
            committed=True,
            mounted=True,
            failed=True,
        )
        mounted = remote.assess_production_route(
            committed=True,
            mounted=True,
        )

        self.assertEqual(contract_only.status, CapabilityStatus.UNAVAILABLE)
        self.assertFalse(contract_only.runnable)
        self.assertEqual(unwired.status, CapabilityStatus.UNAVAILABLE)
        self.assertFalse(unwired.runnable)
        self.assertEqual(failed.status, CapabilityStatus.FAILED)
        self.assertFalse(failed.runnable)
        self.assertEqual(mounted.status, CapabilityStatus.AVAILABLE)
        self.assertTrue(mounted.runnable)


if __name__ == "__main__":
    unittest.main()
