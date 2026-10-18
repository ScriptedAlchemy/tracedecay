#!/usr/bin/env python3
"""Contract tests for storage, index, and query runtime workloads."""

from __future__ import annotations

import dataclasses
import unittest

from benchmark_data.runtime.storage_workloads import (
    WORKLOADS,
    AvailabilityStatus,
    CapturePolicy,
    CatalogValidationError,
    assess_availability,
    build_capture_plan,
    percentile_eligibility,
    validate_workloads,
)


class StorageWorkloadCatalogTest(unittest.TestCase):
    def test_descriptors_are_immutable_and_arguments_are_fresh(self) -> None:
        workload = next(item for item in WORKLOADS if item.id == "context-composite-warm")

        with self.assertRaises(dataclasses.FrozenInstanceError):
            workload.id = "changed"  # type: ignore[misc]

        first = workload.arguments()
        first["keywords"].append("mutation")
        second = workload.arguments()
        self.assertNotIn("mutation", second["keywords"])
        self.assertEqual(second, workload.arguments())

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
