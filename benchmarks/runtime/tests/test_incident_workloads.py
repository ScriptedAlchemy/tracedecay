#!/usr/bin/env python3
"""Contracts for final runtime incident workloads."""

from __future__ import annotations

import unittest

from benchmarks.runtime.incident_workloads import (
    INCIDENT_WORKLOADS,
    IncidentWorkloadError,
    incident_catalog_document,
    validate_incident_observation,
    validate_incident_workloads,
)


EXPECTED_WORKLOADS = {
    "missing-daemon-after-shell",
    "sustained-edit-commit-indexing",
    "foreground-under-maintenance",
    "diagnostic-dedup-batch-rate",
    "daemon-steady-state-resources",
    "renderer-consumer-event-count",
}


class IncidentWorkloadCatalogTest(unittest.TestCase):
    def test_catalog_covers_every_final_incident(self) -> None:
        catalog = validate_incident_workloads()

        self.assertEqual({workload.id for workload in catalog}, EXPECTED_WORKLOADS)
        self.assertTrue(all(workload.sample_count == 1 for workload in catalog))
        self.assertTrue(all(not workload.slo_gate for workload in catalog))
        self.assertTrue(
            all(workload.percentile_minimums == {"p95": 40, "p99": 100}
                for workload in catalog)
        )

    def test_missing_daemon_requires_fail_fast_and_reaping_evidence(self) -> None:
        workload = next(
            item for item in INCIDENT_WORKLOADS
            if item.id == "missing-daemon-after-shell"
        )

        self.assertEqual(workload.process_policy, "crash")
        self.assertEqual(
            set(workload.required_observations),
            {"missing_daemon_fail_fast_ns", "process_tree_reaped"},
        )

    def test_metric_workloads_require_reviewed_raw_observations(self) -> None:
        required = {
            item.id: set(item.required_observations)
            for item in INCIDENT_WORKLOADS
        }

        self.assertTrue(
            {
                "edit_count",
                "commit_count",
                "indexing_run_count",
                "indexing_noop_count",
                "indexing_coalesced_count",
                "generation",
            }.issubset(required["sustained-edit-commit-indexing"])
        )
        self.assertTrue(
            {
                "diagnostic_generated_count",
                "diagnostic_deduplicated_count",
                "diagnostic_batch_count",
            }.issubset(required["diagnostic-dedup-batch-rate"])
        )
        self.assertTrue(
            {
                "daemon_cpu_time_ns",
                "daemon_peak_rss_bytes",
                "daemon_pss_bytes",
                "wal_bytes",
                "disk_read_bytes",
                "disk_write_bytes",
                "write_amplification_ppm",
                "queue_depth",
                "generation",
            }.issubset(required["daemon-steady-state-resources"])
        )
        self.assertTrue(
            {
                "foreground_under_maintenance_ns",
                "queue_enqueued_count",
                "queue_shed_count",
                "queue_cancelled_count",
                "queue_retry_count",
            }.issubset(required["foreground-under-maintenance"])
        )
        self.assertEqual(
            required["renderer-consumer-event-count"],
            {"renderer_event_count", "consumer_event_count"},
        )

    def test_catalog_is_machine_readable_and_fail_closed_until_fixes_land(
        self,
    ) -> None:
        document = incident_catalog_document()

        self.assertEqual(
            document["percentile_eligibility"],
            {"p95_minimum_samples": 40, "p99_minimum_samples": 100},
        )
        self.assertEqual(document["evidence_class"], "n=1_regression_only")
        self.assertTrue(
            all(item["availability"]["state"] == "unavailable"
                for item in document["workloads"])
        )
        self.assertTrue(
            all(item["slo_gate"] is False for item in document["workloads"])
        )


class IncidentObservationValidationTest(unittest.TestCase):
    def test_observation_invariants_preserve_truthful_counts(self) -> None:
        observation = {
            "edit_count": 20,
            "commit_count": 5,
            "indexing_run_count": 8,
            "indexing_noop_count": 3,
            "indexing_coalesced_count": 14,
            "diagnostic_generated_count": 100,
            "diagnostic_deduplicated_count": 30,
            "diagnostic_batch_count": 10,
            "daemon_cpu_time_ns": 50_000,
            "daemon_peak_rss_bytes": 8_192,
            "wal_bytes": 4_096,
            "queue_depth": 2,
            "generation": 7,
            "renderer_event_count": 12,
            "consumer_event_count": 12,
            "missing_daemon_fail_fast_ns": 1_000_000,
            "process_tree_reaped": True,
        }

        self.assertIs(validate_incident_observation(observation), observation)

    def test_observation_rejects_impossible_dedup_and_consumer_counts(self) -> None:
        with self.assertRaisesRegex(IncidentWorkloadError, "deduplicated"):
            validate_incident_observation(
                {
                    "diagnostic_generated_count": 1,
                    "diagnostic_deduplicated_count": 2,
                }
            )
        with self.assertRaisesRegex(IncidentWorkloadError, "consumer"):
            validate_incident_observation(
                {"renderer_event_count": 1, "consumer_event_count": 2}
            )

    def test_observation_rejects_unknown_or_negative_metrics(self) -> None:
        with self.assertRaisesRegex(IncidentWorkloadError, "unknown"):
            validate_incident_observation({"producer_threshold_ns": 10**18})
        with self.assertRaisesRegex(IncidentWorkloadError, "non-negative"):
            validate_incident_observation({"wal_bytes": -1})


if __name__ == "__main__":
    unittest.main()
