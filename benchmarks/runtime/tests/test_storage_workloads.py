#!/usr/bin/env python3
"""Contract tests for storage, index, and query runtime workloads."""

from __future__ import annotations

import dataclasses
import re
import unittest

from benchmarks.runtime.storage_workloads import (
    ABBA_PAIRING,
    BENCHMARK_AUTHORITY,
    DECLARED_CRATE_LANES,
    WORKLOADS,
    AvailabilityExpectation,
    AvailabilityStatus,
    CapturePolicy,
    CatalogValidationError,
    ColdWarm,
    NormalizationMetadata,
    RuntimeState,
    Surface,
    assess_availability,
    build_capture_plan,
    group_workloads_by_crate,
    percentile_eligibility,
    runtime_test_identities,
    validate_workloads,
    workloads_for_crate,
)


REQUIRED_CRATE_LANES = {
    "tracedecay-domain",
    "tracedecay-store",
    "tracedecay-query",
    "tracedecay-code-index",
    "tracedecay-application",
    "tracedecay-rusqlite-parity",
    "tracedecay-rusqlite-runtime",
    "tracedecay",
}


class StorageWorkloadCatalogTest(unittest.TestCase):
    def test_catalog_covers_final_storage_index_and_query_journeys(self) -> None:
        operations = {workload.operation for workload in WORKLOADS}

        self.assertTrue(
            {
                "tracedecay_find_exact_symbol",
                "tracedecay_code_declaration",
                "tracedecay_code_exact_occurrence",
                "tracedecay_grep",
                "tracedecay_code_phrase_search",
                "tracedecay_callers",
                "tracedecay_callees",
                "tracedecay_impact",
                "tracedecay_active_project",
                "tracedecay_project_context",
                "tracedecay_code_symbol_search",
                "tracedecay_message_search",
                "tracedecay_lcm_grep",
                "tracedecay_lcm_expand",
                "tracedecay_lcm_expand_query",
                "tracedecay_context",
            }.issubset(operations)
        )
        self.assertTrue(any(item.journey_id == "payload-stress" for item in WORKLOADS))
        self.assertTrue(any(item.journey_id == "storage-contention" for item in WORKLOADS))

    def test_every_declared_crate_lane_has_representative_workloads(self) -> None:
        self.assertEqual(set(DECLARED_CRATE_LANES), REQUIRED_CRATE_LANES)
        validate_workloads()

        for crate_tag in DECLARED_CRATE_LANES:
            with self.subTest(crate_tag=crate_tag):
                selected = workloads_for_crate(crate_tag)
                self.assertTrue(selected)
                self.assertTrue(all(crate_tag in item.crate_tags for item in selected))

    def test_selection_and_grouping_preserve_catalog_order(self) -> None:
        rebuilt = group_workloads_by_crate(reversed(DECLARED_CRATE_LANES))

        self.assertEqual(
            tuple(crate_tag for crate_tag, _ in rebuilt),
            tuple(reversed(DECLARED_CRATE_LANES)),
        )
        for crate_tag, selected in rebuilt:
            self.assertEqual(selected, workloads_for_crate(crate_tag))
            self.assertEqual(
                [item.id for item in selected],
                [item.id for item in WORKLOADS if crate_tag in item.crate_tags],
            )

    def test_catalog_order_is_deterministic_and_ids_are_unique(self) -> None:
        first = tuple(item.id for item in WORKLOADS)
        second = tuple(item.id for item in WORKLOADS)

        self.assertEqual(first, second)
        self.assertEqual(len(first), len(set(first)))
        self.assertEqual(WORKLOADS, validate_workloads(WORKLOADS))

    def test_descriptors_are_immutable_and_arguments_are_fresh(self) -> None:
        workload = next(item for item in WORKLOADS if item.id == "context-composite-warm")

        with self.assertRaises(dataclasses.FrozenInstanceError):
            workload.id = "changed"  # type: ignore[misc]

        first = workload.arguments()
        first["keywords"].append("mutation")
        second = workload.arguments()
        self.assertNotIn("mutation", second["keywords"])
        self.assertEqual(second, workload.arguments())

    def test_all_runtime_states_and_typed_metadata_are_explicit(self) -> None:
        self.assertTrue(
            {
                RuntimeState.COLD,
                RuntimeState.ADMISSION,
                RuntimeState.FIRST,
                RuntimeState.WARM,
                RuntimeState.REPEAT,
                RuntimeState.PERSISTENT,
                RuntimeState.NO_OP,
                RuntimeState.CONTENTION,
                RuntimeState.RECOVERY,
            }.issubset({item.runtime_state for item in WORKLOADS})
        )
        for workload in WORKLOADS:
            with self.subTest(workload=workload.id):
                self.assertIsInstance(workload.surface, Surface)
                self.assertIsInstance(
                    workload.availability_expectation,
                    AvailabilityExpectation,
                )
                self.assertTrue(workload.timeout_phase)
                self.assertTrue(workload.result_digest_policy)
                self.assertTrue(workload.crate_tags)
                self.assertFalse(hasattr(workload, "slo_gate"))
                self.assertFalse(hasattr(workload, "budget_ms"))
                self.assertFalse(hasattr(workload, "milestone_budget"))

    def test_throughput_workloads_declare_concurrency_1_4_8(self) -> None:
        throughput = [item for item in WORKLOADS if item.throughput_meaningful]

        self.assertTrue(throughput)
        self.assertTrue(all(item.concurrency == (1, 4, 8) for item in throughput))
        self.assertTrue(
            all(not item.concurrency for item in WORKLOADS if not item.throughput_meaningful)
        )

    def test_capture_policy_distinguishes_n1_from_distribution(self) -> None:
        n1 = build_capture_plan(CapturePolicy.N1_REGRESSION_ONLY)

        self.assertEqual(n1.measured_sample_count, 1)
        self.assertEqual(n1.label, "n=1_regression_only")
        self.assertFalse(n1.distribution_evidence)
        with self.assertRaisesRegex(ValueError, "explicit measured_sample_count"):
            build_capture_plan(CapturePolicy.DISTRIBUTION)
        with self.assertRaisesRegex(ValueError, "greater than one"):
            build_capture_plan(CapturePolicy.DISTRIBUTION, measured_sample_count=1)

        distribution = build_capture_plan(
            CapturePolicy.DISTRIBUTION,
            measured_sample_count=7,
        )
        self.assertEqual(distribution.measured_sample_count, 7)
        self.assertTrue(distribution.distribution_evidence)
        self.assertTrue(
            all(
                workload.capture_plan.label == "n=1_regression_only"
                for workload in WORKLOADS
            )
        )

    def test_percentile_eligibility_uses_matching_samples_only(self) -> None:
        n1 = percentile_eligibility(1, junit_retained_sample_count=500)
        p95 = percentile_eligibility(40)
        below_p99 = percentile_eligibility(99)
        p99 = percentile_eligibility(100)

        self.assertFalse(n1.p95_eligible)
        self.assertFalse(n1.p99_eligible)
        self.assertTrue(n1.junit_retention_excluded)
        self.assertTrue(p95.p95_eligible)
        self.assertFalse(p95.p99_eligible)
        self.assertTrue(below_p99.p95_eligible)
        self.assertFalse(below_p99.p99_eligible)
        self.assertTrue(p99.p95_eligible)
        self.assertTrue(p99.p99_eligible)

    def test_runtime_test_identities_are_stable_per_crate_and_normalized(self) -> None:
        first = runtime_test_identities(
            platform="linux-x86_64",
            shard="storage-index-query",
            storage_mode="isolated-fixture",
        )
        second = runtime_test_identities(
            platform="linux-x86_64",
            shard="storage-index-query",
            storage_mode="isolated-fixture",
        )

        self.assertEqual(first, second)
        self.assertEqual(len({item.id for item in first}), len(first))
        self.assertEqual({item.crate_tag for item in first}, REQUIRED_CRATE_LANES)
        for identity in first:
            with self.subTest(identity=identity.id):
                self.assertIn(identity.workload_id, identity.id)
                self.assertIn(identity.journey_id, identity.id)
                self.assertIsInstance(identity.normalization, NormalizationMetadata)
                self.assertEqual(identity.normalization.platform, "linux-x86_64")
                self.assertEqual(identity.normalization.shard, "storage-index-query")
                self.assertEqual(identity.normalization.storage_mode, "isolated-fixture")
                self.assertIn(identity.normalization.concurrency, (1, 4, 8))
                self.assertIsInstance(identity.normalization.cold_warm, ColdWarm)

    def test_capture_plans_expose_percentile_eligibility_without_slo_claims(self) -> None:
        n1 = build_capture_plan(CapturePolicy.N1_REGRESSION_ONLY)
        p95 = build_capture_plan(CapturePolicy.DISTRIBUTION, measured_sample_count=40)
        p99 = build_capture_plan(CapturePolicy.DISTRIBUTION, measured_sample_count=100)

        self.assertFalse(n1.percentiles.p95_eligible)
        self.assertFalse(n1.percentiles.p99_eligible)
        self.assertTrue(p95.percentiles.p95_eligible)
        self.assertFalse(p95.percentiles.p99_eligible)
        self.assertTrue(p99.percentiles.p99_eligible)
        self.assertFalse(hasattr(n1, "slo_gate"))

    def test_abba_metadata_preserves_raw_samples_without_statistics(self) -> None:
        self.assertEqual(ABBA_PAIRING.execution_order, ("A", "B", "B", "A"))
        self.assertEqual(ABBA_PAIRING.pair_indices, ((0, 1), (3, 2)))
        self.assertTrue(ABBA_PAIRING.retain_raw_samples)
        self.assertFalse(hasattr(ABBA_PAIRING, "p_value"))
        self.assertFalse(hasattr(ABBA_PAIRING, "confidence_interval"))

    def test_availability_is_truthful_for_missing_and_unsupported_operations(self) -> None:
        workload = next(item for item in WORKLOADS if item.id == "lcm-expand-warm")

        unavailable = assess_availability(workload, available_operations=())
        unsupported = assess_availability(
            workload,
            available_operations=(),
            unsupported_operations=(workload.operation,),
        )
        available = assess_availability(
            workload,
            available_operations=(workload.operation,),
        )

        self.assertEqual(unavailable.status, AvailabilityStatus.UNAVAILABLE)
        self.assertFalse(unavailable.runnable)
        self.assertEqual(unsupported.status, AvailabilityStatus.UNSUPPORTED)
        self.assertFalse(unsupported.runnable)
        self.assertEqual(available.status, AvailabilityStatus.AVAILABLE)
        self.assertTrue(available.runnable)
        self.assertTrue(unavailable.detail)
        self.assertTrue(unsupported.detail)
        self.assertIsNone(available.detail)

    def test_final_identities_never_encode_pr_stages_or_milestones(self) -> None:
        forbidden = re.compile(r"(?:^|[-_])(pr[-_]?\d+|stage[-_]?\d+|milestone)(?:$|[-_])")

        self.assertEqual(BENCHMARK_AUTHORITY, "measurement_fixture_not_product_contract")
        for workload in WORKLOADS:
            with self.subTest(workload=workload.id):
                identities = (workload.id, workload.journey_id, *workload.crate_tags)
                self.assertTrue(all(forbidden.search(value) is None for value in identities))

    def test_validation_rejects_duplicate_ids_missing_lanes_and_bad_throughput(self) -> None:
        duplicate = WORKLOADS + (WORKLOADS[0],)
        with self.assertRaisesRegex(CatalogValidationError, "duplicate workload id"):
            validate_workloads(duplicate)

        without_root = tuple(
            dataclasses.replace(
                item,
                crate_tags=tuple(tag for tag in item.crate_tags if tag != "tracedecay"),
            )
            for item in WORKLOADS
        )
        with self.assertRaisesRegex(CatalogValidationError, "tracedecay"):
            validate_workloads(without_root)

        throughput = next(item for item in WORKLOADS if item.throughput_meaningful)
        malformed = tuple(
            dataclasses.replace(item, concurrency=())
            if item.id == throughput.id
            else item
            for item in WORKLOADS
        )
        with self.assertRaisesRegex(CatalogValidationError, "concurrency"):
            validate_workloads(malformed)


if __name__ == "__main__":
    unittest.main()
