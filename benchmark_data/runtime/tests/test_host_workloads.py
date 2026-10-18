#!/usr/bin/env python3
"""Contract tests for stable host and SDK runtime workload descriptors."""

from __future__ import annotations

import unittest
from dataclasses import replace

from benchmark_data.runtime.host_workloads import (
    HOST_WORKLOADS,
    PERCENTILE_POLICY,
    AvailabilityState,
    EvidenceClass,
    HostKind,
    ProductionRoute,
    SampleIdentity,
    available_percentiles,
    evidence_class_for_sample_count,
    validate_catalog,
    validate_remote_success,
    validate_sample_identities,
)


class HostWorkloadCatalogTests(unittest.TestCase):
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


class EvidenceContractTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
