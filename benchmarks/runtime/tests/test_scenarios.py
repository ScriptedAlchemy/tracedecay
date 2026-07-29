#!/usr/bin/env python3
"""Contract tests for deterministic runtime scenario definitions."""

from __future__ import annotations

import json
import re
import unittest
from dataclasses import fields

from benchmarks.runtime.schema import SCHEMA_VERSION, validate_sample
from benchmarks.runtime.scenarios import (
    SCENARIOS,
    WORKLOADS,
    CapabilityStatus,
    CrateLane,
    DaemonSurvival,
    DigestSemantics,
    RuntimeState,
    ShutdownEvidence,
    Surface,
    TimeoutPhase,
    WorkloadInputs,
    WorkloadKind,
    build_scenarios,
    stable_digest,
    validate_stable_id,
)


class ScenarioCatalogTest(unittest.TestCase):
    def test_workloads_cover_every_required_query_family_and_tool(self) -> None:
        tools = {workload.tool for workload in WORKLOADS}
        kinds = {workload.kind for workload in WORKLOADS}

        self.assertEqual(
            kinds,
            {
                WorkloadKind.EXACT,
                WorkloadKind.LEXICAL,
                WorkloadKind.GRAPH,
                WorkloadKind.SESSION,
                WorkloadKind.CONTEXT,
                WorkloadKind.QUERY,
                WorkloadKind.PAYLOAD,
                WorkloadKind.CONCURRENCY,
            },
        )
        self.assertTrue(
            {
                "tracedecay_find_exact_symbol",
                "tracedecay_code_exact_occurrence",
                "tracedecay_grep",
                "tracedecay_code_phrase_search",
                "tracedecay_callers",
                "tracedecay_callees",
                "tracedecay_impact",
                "tracedecay_search",
                "tracedecay_message_search",
                "tracedecay_lcm_grep",
                "tracedecay_lcm_expand_query",
                "tracedecay_context",
            }.issubset(tools)
        )

    def test_scenarios_cover_surfaces_states_and_throughput_levels(self) -> None:
        self.assertEqual({scenario.surface for scenario in SCENARIOS}, set(Surface))
        self.assertEqual({scenario.state for scenario in SCENARIOS}, set(RuntimeState))
        self.assertEqual(
            {
                scenario.concurrency
                for scenario in SCENARIOS
                if scenario.is_throughput
            },
            {1, 4, 8},
        )
        self.assertEqual(
            {
                scenario.surface
                for scenario in SCENARIOS
                if scenario.is_throughput
            },
            {Surface.CLI, Surface.MCP},
        )
        self.assertTrue(
            all(
                scenario.surface is Surface.MCP
                for scenario in SCENARIOS
                if scenario.state is RuntimeState.PERSISTENT_MCP
            )
        )

    def test_final_runtime_states_include_noop_contention_and_recovery(self) -> None:
        self.assertTrue(
            {
                RuntimeState.COLD_ADMISSION,
                RuntimeState.WARM,
                RuntimeState.NO_OP,
                RuntimeState.CONTENTION,
                RuntimeState.RECOVERY,
            }.issubset({scenario.state for scenario in SCENARIOS})
        )

    def test_catalog_is_reproducible_and_has_unique_ids(self) -> None:
        rebuilt = build_scenarios()

        self.assertEqual(rebuilt, SCENARIOS)
        self.assertEqual(
            len({scenario.id for scenario in SCENARIOS}),
            len(SCENARIOS),
        )

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

    def test_every_scenario_declares_digest_and_capability_semantics(self) -> None:
        workload_by_id = {workload.id: workload for workload in WORKLOADS}

        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.id):
                workload = workload_by_id[scenario.workload_id]
                self.assertIsInstance(scenario.digest_semantics, DigestSemantics)
                self.assertIn(workload.tool, scenario.required_capabilities)
                self.assertGreaterEqual(len(scenario.required_capabilities), 1)

    def test_workloads_name_final_crate_lanes_and_scenarios_stay_in_lane(self) -> None:
        expected_lanes = {
            "tracedecay-query",
            "tracedecay-code-index",
            "tracedecay-capture",
            "tracedecay-application",
            "tracedecay-hooks",
            "tracedecay-api",
            "tracedecay-rusqlite-runtime",
            "tracedecay",
        }
        self.assertEqual({lane.value for lane in CrateLane}, expected_lanes)
        self.assertEqual(
            {
                lane
                for workload in WORKLOADS
                for lane in workload.supported_crate_lanes
            },
            set(CrateLane),
        )
        workload_by_id = {workload.id: workload for workload in WORKLOADS}
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.id):
                workload = workload_by_id[scenario.workload_id]
                self.assertIn(
                    scenario.crate_lane,
                    workload.supported_crate_lanes,
                )
                self.assertTrue(scenario.journey_id)

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

    def test_runtime_normalization_dimensions_are_explicit_and_only_runtime(self) -> None:
        cold = next(
            scenario
            for scenario in SCENARIOS
            if scenario.state is RuntimeState.COLD_ADMISSION
        )
        warm = next(
            scenario for scenario in SCENARIOS if scenario.state is RuntimeState.WARM
        )

        self.assertEqual(
            cold.normalization_dimensions(
                platform="linux-x86_64",
                shard="shard-a",
                storage_mode="durable",
            ),
            {
                "platform": "linux-x86_64",
                "shard": "shard-a",
                "storage_mode": "durable",
                "concurrency": cold.concurrency,
                "cache_state": "cold",
            },
        )
        self.assertEqual(
            warm.normalization_dimensions(
                platform="linux-x86_64",
                shard="shard-a",
                storage_mode="durable",
            )["cache_state"],
            "warm",
        )
        self.assertEqual(
            cold.test_identity,
            (cold.crate_lane.value, cold.journey_id, cold.workload_id, cold.id),
        )

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

    def test_every_scenario_is_n1_wall_time_regression_evidence_not_an_slo(self) -> None:
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.id):
                self.assertEqual(scenario.sample_count, 1)
                self.assertTrue(scenario.measures_wall_time)
                self.assertFalse(scenario.slo_gate)
                self.assertFalse(scenario.accepts_empty_success)
                self.assertIsInstance(scenario.timeout_phase, TimeoutPhase)
                self.assertIsInstance(scenario.daemon_survival, DaemonSurvival)

    def test_host_evidence_scenarios_preserve_typed_outcomes_and_states(self) -> None:
        evidence = {
            scenario.evidence_id: scenario
            for scenario in SCENARIOS
            if scenario.evidence_id is not None
        }

        self.assertTrue(
            {
                "warming-daemon",
                "unresponsive-daemon",
                "dashboard-malformed",
                "dashboard-204",
                "dashboard-404",
                "verbose-hanging-child",
                "repeated-capture-ids",
            }.issubset(evidence)
        )
        self.assertEqual(
            evidence["warming-daemon"].expected_status,
            CapabilityStatus.PARTIAL,
        )
        self.assertEqual(
            evidence["unresponsive-daemon"].expected_status,
            CapabilityStatus.UNAVAILABLE,
        )
        self.assertEqual(
            evidence["dashboard-malformed"].expected_status,
            CapabilityStatus.FAILED,
        )
        self.assertEqual(
            evidence["dashboard-204"].expected_status,
            CapabilityStatus.UNAVAILABLE,
        )
        self.assertEqual(
            evidence["dashboard-404"].expected_status,
            CapabilityStatus.UNSUPPORTED,
        )
        self.assertEqual(
            evidence["verbose-hanging-child"].timeout_phase,
            TimeoutPhase.CHILD_IO,
        )
        self.assertEqual(
            evidence["repeated-capture-ids"].expected_status,
            CapabilityStatus.AVAILABLE,
        )
        self.assertEqual(
            {scenario.state for scenario in evidence.values()},
            {RuntimeState.HOST_ACTIVATION, RuntimeState.HOST_RESTART},
        )
        self.assertTrue(
            all(
                scenario.daemon_survival is DaemonSurvival.REQUIRED
                for scenario in evidence.values()
            )
        )

    def test_shutdown_evidence_keeps_total_and_abort_offsets_distinct(self) -> None:
        shutdown = {
            scenario.shutdown_evidence
            for scenario in SCENARIOS
            if scenario.shutdown_evidence is not None
        }

        self.assertEqual(
            shutdown,
            {
                ShutdownEvidence(total_seconds=89, abort_offset_seconds=81),
                ShutdownEvidence(total_seconds=57, abort_offset_seconds=52),
            },
        )

    def test_catalog_exposes_every_typed_expected_state(self) -> None:
        self.assertEqual(
            {scenario.expected_status for scenario in SCENARIOS},
            set(CapabilityStatus),
        )


if __name__ == "__main__":
    unittest.main()
