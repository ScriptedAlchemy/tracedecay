#!/usr/bin/env python3
"""Contract tests for stable host and SDK runtime workload descriptors."""

from __future__ import annotations

import re
import unittest
from dataclasses import FrozenInstanceError, replace

from benchmarks.runtime.host_workloads import (
    ABBA_COMPARISON,
    HOST_WORKLOADS,
    NORMALIZATION_DIMENSIONS,
    PERCENTILE_POLICY,
    REQUIRED_CRATE_TAGS,
    REVIEWED_SHUTDOWN_OBSERVATIONS,
    ActivationState,
    AvailabilityState,
    BodyKind,
    ChildProcessStyle,
    DaemonState,
    EvidenceClass,
    HostKind,
    Journey,
    ProductionRoute,
    RestartState,
    SampleIdentity,
    Temperature,
    available_percentiles,
    evidence_class_for_sample_count,
    group_by_host,
    select_workloads,
    validate_catalog,
    validate_remote_success,
    validate_sample_identities,
)


class HostWorkloadCatalogTests(unittest.TestCase):
    def test_catalog_is_immutable_unique_and_stably_ordered(self) -> None:
        self.assertIsInstance(HOST_WORKLOADS, tuple)
        self.assertEqual(validate_catalog(HOST_WORKLOADS), HOST_WORKLOADS)
        self.assertEqual(
            [workload.workload_id for workload in select_workloads()],
            [workload.workload_id for workload in HOST_WORKLOADS],
        )
        self.assertEqual(
            len({workload.workload_id for workload in HOST_WORKLOADS}),
            len(HOST_WORKLOADS),
        )
        with self.assertRaises(FrozenInstanceError):
            HOST_WORKLOADS[0].workload_id = "changed"  # type: ignore[misc]

    def test_identities_never_encode_pr_stages_or_milestones(self) -> None:
        forbidden = re.compile(r"\bpr[-_ ]?\d+\b|milestone|budget", re.IGNORECASE)
        for workload in HOST_WORKLOADS:
            with self.subTest(workload=workload.workload_id):
                identity = " ".join(
                    (
                        workload.workload_id,
                        workload.host.value,
                        workload.journey.value,
                        *workload.crate_tags,
                        *workload.crate_test_ids,
                    )
                )
                if workload.inputs.production_route is not None:
                    identity += " " + workload.inputs.production_route.route_id
                self.assertIsNone(forbidden.search(identity))

    def test_each_crate_lane_has_a_stable_runtime_test_id(self) -> None:
        all_test_ids: list[str] = []
        for workload in HOST_WORKLOADS:
            with self.subTest(workload=workload.workload_id):
                self.assertEqual(len(workload.crate_test_ids), len(workload.crate_tags))
                self.assertEqual(
                    tuple(
                        test_id.split("::", maxsplit=3)[1]
                        for test_id in workload.crate_test_ids
                    ),
                    workload.crate_tags,
                )
                self.assertTrue(
                    all(test_id.startswith("runtime::") for test_id in workload.crate_test_ids)
                )
                all_test_ids.extend(workload.crate_test_ids)
        self.assertEqual(len(all_test_ids), len(set(all_test_ids)))

    def test_every_required_crate_and_host_lane_is_covered(self) -> None:
        covered_crates = {
            crate_tag for workload in HOST_WORKLOADS for crate_tag in workload.crate_tags
        }
        self.assertEqual(covered_crates, set(REQUIRED_CRATE_TAGS))
        self.assertEqual({workload.host for workload in HOST_WORKLOADS}, set(HostKind))
        self.assertTrue(
            {
                Journey.COLD,
                Journey.WARM,
                Journey.NO_OP,
                Journey.CONTENTION,
                Journey.RECOVERY,
            }.issubset({workload.journey for workload in HOST_WORKLOADS})
        )

    def test_catalog_validation_rejects_duplicates_and_missing_crate_lanes(self) -> None:
        duplicate = replace(HOST_WORKLOADS[1], workload_id=HOST_WORKLOADS[0].workload_id)
        with self.assertRaisesRegex(ValueError, "duplicate workload_id"):
            validate_catalog((HOST_WORKLOADS[0], duplicate))

        without_capture = tuple(
            workload
            for workload in HOST_WORKLOADS
            if "tracedecay-capture" not in workload.crate_tags
        )
        with self.assertRaisesRegex(ValueError, "tracedecay-capture"):
            validate_catalog(without_capture)

    def test_selection_and_grouping_preserve_catalog_order(self) -> None:
        sdk = select_workloads(host=HostKind.SDK)
        application = select_workloads(crate_tag="tracedecay-application")
        both = select_workloads(
            host=HostKind.SDK,
            crate_tag="tracedecay-application",
        )
        self.assertEqual(
            both,
            tuple(workload for workload in sdk if workload in application),
        )

        grouped = group_by_host()
        self.assertEqual(tuple(grouped), tuple(HostKind))
        for host, workloads in grouped.items():
            self.assertEqual(workloads, select_workloads(host=host))


class EvidenceContractTests(unittest.TestCase):
    def test_all_observations_are_n1_regression_only_and_never_slo_gates(self) -> None:
        for workload in HOST_WORKLOADS:
            with self.subTest(workload=workload.workload_id):
                evidence = workload.evidence
                self.assertEqual(evidence.sample_count, 1)
                self.assertIs(evidence.evidence_class, EvidenceClass.N1_REGRESSION_ONLY)
                self.assertFalse(evidence.distribution_eligible)
                self.assertTrue(evidence.wall_time.advisory)
                self.assertFalse(evidence.wall_time.slo_gate)

    def test_distribution_classification_requires_more_than_one_sample(self) -> None:
        self.assertIs(
            evidence_class_for_sample_count(1),
            EvidenceClass.N1_REGRESSION_ONLY,
        )
        self.assertIs(
            evidence_class_for_sample_count(2),
            EvidenceClass.DISTRIBUTION,
        )
        with self.assertRaisesRegex(ValueError, "positive"):
            evidence_class_for_sample_count(0)

    def test_reviewed_shutdown_totals_and_abort_offsets_are_distinct(self) -> None:
        self.assertEqual(
            tuple(
                (observation.total_seconds, observation.abort_offset_seconds)
                for observation in REVIEWED_SHUTDOWN_OBSERVATIONS
            ),
            ((89, 81), (57, 52)),
        )
        for observation in REVIEWED_SHUTDOWN_OBSERVATIONS:
            self.assertGreater(
                observation.total_seconds,
                observation.abort_offset_seconds,
            )
            self.assertEqual(observation.sample_count, 1)
            self.assertIs(
                observation.evidence_class,
                EvidenceClass.N1_REGRESSION_ONLY,
            )

    def test_repeated_capture_ids_remain_separate_raw_samples(self) -> None:
        samples = (
            SampleIdentity(sample_id="sample-a", capture_id="capture-repeat"),
            SampleIdentity(sample_id="sample-b", capture_id="capture-repeat"),
        )
        self.assertEqual(validate_sample_identities(samples), samples)
        self.assertEqual(len(validate_sample_identities(samples)), 2)

        with self.assertRaisesRegex(ValueError, "duplicate sample_id"):
            validate_sample_identities(
                (
                    samples[0],
                    SampleIdentity(
                        sample_id="sample-a",
                        capture_id="capture-other",
                    ),
                )
            )

    def test_availability_for_adversarial_inputs_is_truthful_and_typed(self) -> None:
        by_id = {workload.workload_id: workload for workload in HOST_WORKLOADS}
        expected = {
            "runtime.dashboard.cold.warming-daemon": AvailabilityState.PARTIAL,
            "runtime.cli.contention.unresponsive-daemon": AvailabilityState.UNAVAILABLE,
            "runtime.dashboard.contention.malformed-response": AvailabilityState.FAILED,
            "runtime.dashboard.contention.no-content-204": AvailabilityState.UNAVAILABLE,
            "runtime.dashboard.contention.not-found-404": AvailabilityState.UNSUPPORTED,
        }
        for workload_id, availability in expected.items():
            with self.subTest(workload=workload_id):
                self.assertIs(
                    by_id[workload_id].evidence.expected_availability,
                    availability,
                )

    def test_host_activation_and_restart_inputs_cover_each_agent_host(self) -> None:
        for host in (HostKind.CURSOR, HostKind.CLAUDE, HostKind.CODEX):
            workloads = select_workloads(host=host)
            states = {
                (
                    workload.inputs.activation_state,
                    workload.inputs.restart_state,
                )
                for workload in workloads
            }
            self.assertIn(
                (ActivationState.PENDING, RestartState.NOT_REQUIRED),
                states,
            )
            self.assertIn(
                (ActivationState.ACTIVE, RestartState.NOT_REQUIRED),
                states,
            )
            self.assertIn(
                (ActivationState.ACTIVE, RestartState.REQUIRED),
                states,
            )

    def test_sdk_child_contract_requires_concurrent_stream_drain_and_timeout_evidence(
        self,
    ) -> None:
        by_id = {workload.workload_id: workload for workload in HOST_WORKLOADS}
        verbose = by_id["runtime.sdk.warm.verbose-child"]
        hanging = by_id["runtime.sdk.contention.hanging-child"]

        self.assertIs(
            verbose.inputs.child_process.style,
            ChildProcessStyle.VERBOSE,
        )
        self.assertTrue(verbose.inputs.child_process.concurrent_stream_drain)
        self.assertIs(
            hanging.inputs.child_process.style,
            ChildProcessStyle.HANGING,
        )
        self.assertTrue(hanging.inputs.child_process.concurrent_stream_drain)
        self.assertTrue(hanging.inputs.child_process.expected_to_hang)
        self.assertTrue(
            {
                "stdout_bytes",
                "stderr_bytes",
                "process_count",
                "daemon_survived",
                "timeout_phase",
            }.issubset(hanging.evidence.required_fields)
        )

    def test_dashboard_and_runtime_measurement_contracts_are_explicit(self) -> None:
        by_id = {workload.workload_id: workload for workload in HOST_WORKLOADS}
        self.assertIs(
            by_id["runtime.dashboard.contention.malformed-response"].inputs.dashboard.body,
            BodyKind.MALFORMED,
        )
        self.assertEqual(
            by_id["runtime.dashboard.contention.no-content-204"].inputs.dashboard.status_code,
            204,
        )
        self.assertEqual(
            by_id["runtime.dashboard.contention.not-found-404"].inputs.dashboard.status_code,
            404,
        )

        fields = {
            field
            for workload in HOST_WORKLOADS
            for field in workload.evidence.required_fields
        }
        self.assertTrue(
            {
                "cli_wall_ns",
                "mcp_wall_ns",
                "hook_wall_ns",
                "host_wall_ns",
                "handler_us",
                "request_bytes",
                "response_bytes",
                "content_bytes",
                "process_count",
                "daemon_survived",
                "timeout_phase",
            }.issubset(fields)
        )
        mcp = select_workloads(host=HostKind.MCP)
        self.assertTrue(
            all(
                workload.evidence.wall_time.includes_handler_middle_slice
                for workload in mcp
            )
        )

    def test_daemon_states_cover_warming_unresponsive_and_survival(self) -> None:
        daemon_states = {workload.inputs.daemon_state for workload in HOST_WORKLOADS}
        self.assertTrue(
            {
                DaemonState.WARMING,
                DaemonState.UNRESPONSIVE,
                DaemonState.READY,
                DaemonState.SURVIVED_TIMEOUT,
            }.issubset(daemon_states)
        )

    def test_normalization_is_runtime_only_and_covers_required_dimensions(self) -> None:
        self.assertEqual(
            NORMALIZATION_DIMENSIONS,
            ("platform", "shard", "storage_mode", "concurrency", "cold_warm"),
        )
        self.assertTrue(
            all(workload.normalization.runtime_only for workload in HOST_WORKLOADS)
        )
        self.assertTrue(
            all(
                workload.normalization.dimensions == NORMALIZATION_DIMENSIONS
                for workload in HOST_WORKLOADS
            )
        )
        self.assertEqual(
            {workload.normalization.cold_warm for workload in HOST_WORKLOADS},
            {Temperature.COLD, Temperature.WARM},
        )

    def test_remote_final_v2_success_requires_a_mounted_committed_route(self) -> None:
        remote_hosts = {HostKind.CURSOR, HostKind.CLAUDE, HostKind.CODEX, HostKind.SDK}
        for workload in HOST_WORKLOADS:
            if workload.host not in remote_hosts:
                continue
            with self.subTest(workload=workload.workload_id):
                route = workload.inputs.production_route
                self.assertIsNotNone(route)
                self.assertTrue(route.committed)
                self.assertTrue(route.mounted)
                self.assertTrue(route.wired)
                self.assertTrue(route.route_id.startswith("runtime.route."))
                validate_remote_success(
                    route,
                    workload.evidence.expected_availability,
                )

        invalid_routes = (
            ProductionRoute(
                route_id="runtime.route.uncommitted",
                committed=False,
                mounted=True,
                wired=True,
            ),
            ProductionRoute(
                route_id="runtime.route.unmounted",
                committed=True,
                mounted=False,
                wired=True,
            ),
            ProductionRoute(
                route_id="runtime.route.contract-only",
                committed=True,
                mounted=True,
                wired=False,
            ),
        )
        for route in invalid_routes:
            with self.subTest(route=route.route_id):
                with self.assertRaisesRegex(ValueError, "production route"):
                    validate_remote_success(route, AvailabilityState.AVAILABLE)

    def test_percentiles_require_matching_runtime_samples_not_junit_retention(
        self,
    ) -> None:
        self.assertFalse(PERCENTILE_POLICY.junit_retention_is_percentile_history)
        self.assertEqual(PERCENTILE_POLICY.p95_min_matching_samples, 40)
        self.assertEqual(PERCENTILE_POLICY.p99_min_matching_samples, 100)
        self.assertNotIn("p95", available_percentiles(39))
        self.assertIn("p95", available_percentiles(40))
        self.assertNotIn("p99", available_percentiles(99))
        self.assertIn("p99", available_percentiles(100))

    def test_abba_metadata_preserves_raw_samples_without_a_gate(self) -> None:
        self.assertEqual(ABBA_COMPARISON.order, ("A", "B", "B", "A"))
        self.assertTrue(ABBA_COMPARISON.preserve_raw_samples)
        self.assertEqual(
            ABBA_COMPARISON.identity_fields,
            ("workload_id", "host", "journey", "crate_tags"),
        )
        self.assertFalse(ABBA_COMPARISON.slo_gate)


if __name__ == "__main__":
    unittest.main()
