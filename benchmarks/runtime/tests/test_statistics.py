from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from benchmarks.runtime.statistics import (  # noqa: E402
    bootstrap_confidence_interval,
    nearest_rank,
    summarize_distribution,
    summarize_normalized_history,
    summarize_rates,
    summarize_throughput,
)


def retained_sample(
    latency_ns: int,
    *,
    retention_kind: str = "runtime_sample",
    **identity_overrides: object,
) -> dict:
    identity = {
        "crate_id": "tracedecay-store",
        "journey_id": "durable-read",
        "workload_id": "lookup",
        "platform": "linux-x86_64",
        "shard": "runtime-0",
        "storage_mode": "sqlite",
        "concurrency": 1,
        "temperature": "warm",
    }
    identity.update(identity_overrides)
    return {
        "identity": identity,
        "retention_kind": retention_kind,
        "latency_ns": latency_ns,
    }


class PercentileTests(unittest.TestCase):
    def test_small_sample_p99_is_maximum(self) -> None:
        self.assertEqual(nearest_rank([30, 10, 20], 0.99), 30)
        self.assertEqual(nearest_rank([30, 10, 20], 0.50), 20)

        summary = summarize_distribution([30, 10, 20])
        self.assertEqual(summary["p50"], 20)
        self.assertEqual(summary["p95"], 30)
        self.assertEqual(summary["p99"], 30)
        self.assertEqual(summary["percentile_method"], "nearest_rank")

    def test_empty_distribution_has_null_percentiles(self) -> None:
        self.assertIsNone(nearest_rank([], 0.99))
        self.assertEqual(
            summarize_distribution([]),
            {
                "sample_count": 0,
                "min": None,
                "p50": None,
                "p95": None,
                "p99": None,
                "max": None,
                "mean": None,
                "percentile_method": "nearest_rank",
            },
        )

    def test_invalid_percentiles_and_non_finite_samples_are_rejected(self) -> None:
        for percentile in (0, -0.1, 1.1):
            with self.subTest(percentile=percentile):
                with self.assertRaises(ValueError):
                    nearest_rank([1], percentile)

        with self.assertRaises(ValueError):
            summarize_distribution([1, float("nan")])


class RetainedHistoryTests(unittest.TestCase):
    def test_normalization_includes_runtime_identity_and_junit_dimensions(self) -> None:
        summary = summarize_normalized_history([retained_sample(10)])

        self.assertEqual(
            summary["normalization"],
            retained_sample(10)["identity"],
        )
        self.assertEqual(summary["sample_count"], 1)
        self.assertEqual(summary["evidence_class"], "regression_sample")
        self.assertFalse(summary["p95"]["available"])
        self.assertFalse(summary["p99"]["available"])

    def test_p95_requires_40_and_p99_requires_100_matching_samples(self) -> None:
        below_p95 = summarize_normalized_history(
            [retained_sample(value) for value in range(1, 40)]
        )
        at_p95 = summarize_normalized_history(
            [retained_sample(value) for value in range(1, 41)]
        )
        below_p99 = summarize_normalized_history(
            [retained_sample(value) for value in range(1, 100)]
        )
        at_p99 = summarize_normalized_history(
            [retained_sample(value) for value in range(1, 101)]
        )

        self.assertEqual(
            below_p95["p95"],
            {"available": False, "value": None, "minimum_samples": 40},
        )
        self.assertEqual(
            at_p95["p95"],
            {"available": True, "value": 38, "minimum_samples": 40},
        )
        self.assertEqual(
            below_p99["p99"],
            {"available": False, "value": None, "minimum_samples": 100},
        )
        self.assertEqual(
            at_p99["p99"],
            {"available": True, "value": 99, "minimum_samples": 100},
        )

    def test_junit_retention_is_not_percentile_history(self) -> None:
        retained = summarize_normalized_history(
            [
                retained_sample(value, retention_kind="junit_retention")
                for value in range(1, 101)
            ]
        )

        self.assertEqual(retained["sample_count"], 0)
        self.assertEqual(retained["junit_retention_count"], 100)
        self.assertEqual(retained["evidence_class"], "unavailable")
        self.assertFalse(retained["p95"]["available"])
        self.assertFalse(retained["p99"]["available"])

    def test_mixed_normalization_dimensions_are_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "normalized identity"):
            summarize_normalized_history(
                [
                    retained_sample(10, platform="linux-x86_64"),
                    retained_sample(11, platform="macos-arm64"),
                ]
            )


class BootstrapTests(unittest.TestCase):
    def test_seeded_bootstrap_confidence_interval_is_deterministic(self) -> None:
        values = [-0.2, -0.1, 0.0, 0.1, 0.2]

        first = bootstrap_confidence_interval(values, seed=123, resamples=1_000)
        second = bootstrap_confidence_interval(values, seed=123, resamples=1_000)

        self.assertEqual(first, second)
        self.assertLessEqual(first[0], 0.0)
        self.assertGreaterEqual(first[1], 0.0)

    def test_single_sample_bootstrap_preserves_regression_evidence(self) -> None:
        self.assertEqual(
            bootstrap_confidence_interval([0.25], seed=7, resamples=100),
            (0.25, 0.25),
        )

    def test_bootstrap_rejects_missing_or_invalid_inputs(self) -> None:
        with self.assertRaises(ValueError):
            bootstrap_confidence_interval([], seed=1)
        with self.assertRaises(ValueError):
            bootstrap_confidence_interval([1.0, float("inf")], seed=1)
        with self.assertRaises(ValueError):
            bootstrap_confidence_interval([1.0], seed=1, resamples=0)
        with self.assertRaises(ValueError):
            bootstrap_confidence_interval([1.0], seed=1, confidence=1.0)


class RateTests(unittest.TestCase):
    def test_rates_count_timeouts_as_errors_and_as_timeouts(self) -> None:
        summary = summarize_rates(["success", "error", "timeout", "success"])

        self.assertEqual(summary["attempt_count"], 4)
        self.assertEqual(summary["success_count"], 2)
        self.assertEqual(summary["error_count"], 2)
        self.assertEqual(summary["timeout_count"], 1)
        self.assertEqual(summary["success_rate"], 0.5)
        self.assertEqual(summary["error_rate"], 0.5)
        self.assertEqual(summary["timeout_rate"], 0.25)

    def test_empty_rates_are_zero_not_nan(self) -> None:
        summary = summarize_rates([])

        self.assertEqual(summary["attempt_count"], 0)
        self.assertEqual(summary["success_rate"], 0.0)
        self.assertEqual(summary["error_rate"], 0.0)
        self.assertEqual(summary["timeout_rate"], 0.0)

    def test_unknown_status_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            summarize_rates(["success", "unavailable"])


class ThroughputTests(unittest.TestCase):
    def test_throughput_uses_elapsed_nanoseconds(self) -> None:
        self.assertEqual(
            summarize_throughput(completed_count=4, elapsed_ns=2_000_000_000, total_bytes=100),
            {
                "elapsed_ns": 2_000_000_000,
                "completed_count": 4,
                "total_bytes": 100,
                "operations_per_second": 2.0,
                "bytes_per_second": 50.0,
            },
        )

    def test_non_positive_elapsed_time_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            summarize_throughput(completed_count=1, elapsed_ns=0)

        with self.assertRaises(ValueError):
            summarize_throughput(completed_count=-1, elapsed_ns=1)


if __name__ == "__main__":
    unittest.main()
