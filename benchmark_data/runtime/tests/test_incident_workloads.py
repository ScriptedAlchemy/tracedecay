#!/usr/bin/env python3
"""Contracts for final runtime incident workloads."""

from __future__ import annotations

import unittest

from benchmark_data.runtime.incident_workloads import (
    IncidentWorkloadError,
    validate_incident_observation,
)


class IncidentObservationValidationTest(unittest.TestCase):
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
