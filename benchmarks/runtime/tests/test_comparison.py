from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from benchmarks.runtime.comparison import (  # noqa: E402
    aggregate_hard_failures,
    bootstrap_confidence_interval,
    classify_change,
    compare_abba,
    latency_budget_findings,
    pair_abba_rounds,
    paired_log_ratios,
)


def comparison_identity(**overrides: object) -> dict:
    identity = {
        "baseline_candidate_id": "candidate-a",
        "treatment_candidate_id": "candidate-b",
        "run_id": "run-1",
        "capture_id": "capture-1",
        "crate_id": "tracedecay-application",
        "journey_id": "indexed-query",
        "workload_id": "exact-symbol",
        "platform": "linux-x86_64",
        "shard": "runtime-0",
        "storage_mode": "sqlite",
        "state": "warm",
        "temperature": "warm",
        "surface": "mcp",
        "concurrency": 1,
    }
    identity.update(overrides)
    return identity


def sample(
    status: str = "success",
    *,
    expected_digest: str = "a" * 64,
    actual_digest: str | None = "a" * 64,
    error: str | None = None,
    daemon_survived: bool = True,
) -> dict:
    return {
        "availability": {"state": "available", "detail": None},
        "lifecycle": {"daemon_survived": daemon_survived},
        "outcome": {
            "status": status,
            "expected_digest": expected_digest,
            "actual_digest": actual_digest,
            "result_digest": actual_digest,
            "error": error,
        }
    }


class AbbaPairingTests(unittest.TestCase):
    def test_abba_rounds_pair_adjacent_measurements_in_time(self) -> None:
        rounds = [
            [("A", 100), ("B", 110), ("B", 90), ("A", 100)],
            [("A", 200), ("B", 220), ("B", 180), ("A", 200)],
        ]

        self.assertEqual(
            pair_abba_rounds(rounds),
            [(100.0, 110.0), (100.0, 90.0), (200.0, 220.0), (200.0, 180.0)],
        )

    def test_incomplete_or_out_of_order_rounds_are_rejected(self) -> None:
        bad_rounds = (
            [("A", 1), ("B", 2), ("B", 3)],
            [("A", 1), ("B", 2), ("A", 3), ("B", 4)],
        )
        for round_ in bad_rounds:
            with self.subTest(round_=round_):
                with self.assertRaisesRegex(ValueError, "ABBA"):
                    pair_abba_rounds([round_])

    def test_paired_log_ratios_reject_zero_values(self) -> None:
        self.assertAlmostEqual(
            paired_log_ratios([(100, 110), (100, 90)])[0],
            0.09531017980432493,
        )
        with self.assertRaises(ValueError):
            paired_log_ratios([(0, 1)])


class BootstrapTests(unittest.TestCase):
    def test_seeded_bootstrap_is_deterministic(self) -> None:
        values = [-0.2, -0.1, 0.0, 0.1, 0.2]

        first = bootstrap_confidence_interval(values, seed=123, resamples=1_000)
        second = bootstrap_confidence_interval(values, seed=123, resamples=1_000)

        self.assertEqual(first, second)
        self.assertLessEqual(first[0], 0.0)
        self.assertGreaterEqual(first[1], 0.0)


class DecisionTests(unittest.TestCase):
    def test_practical_floor_and_relative_threshold_must_both_be_crossed(self) -> None:
        self.assertEqual(
            classify_change(100, 111, relative_threshold=0.05, practical_floor=20),
            "no_material_change",
        )
        self.assertEqual(
            classify_change(100, 121, relative_threshold=0.05, practical_floor=20),
            "regression",
        )
        self.assertEqual(
            classify_change(100, 79, relative_threshold=0.05, practical_floor=20),
            "improvement",
        )

    def test_machine_mismatch_makes_comparison_descriptive(self) -> None:
        result = compare_abba(
            [[("A", 100), ("B", 130), ("B", 130), ("A", 100)]],
            identity=comparison_identity(),
            baseline_machine_fingerprint="machine-a",
            treatment_machine_fingerprint="machine-b",
            relative_threshold=0.10,
            practical_floor=5,
            resamples=100,
        )

        self.assertEqual(result["decision"], "descriptive_only")
        self.assertTrue(result["informational"])
        self.assertFalse(result["machine_comparable"])
        self.assertEqual(result["paired"]["change"], "regression")

    def test_latency_regression_is_advisory_but_correctness_failure_is_hard(self) -> None:
        rounds = [[("A", 100), ("B", 130), ("B", 130), ("A", 100)]]
        advisory = compare_abba(
            rounds,
            identity=comparison_identity(),
            baseline_machine_fingerprint="machine-a",
            treatment_machine_fingerprint="machine-a",
            relative_threshold=0.10,
            practical_floor=5,
            resamples=100,
        )
        self.assertEqual(advisory["decision"], "descriptive_only")
        self.assertEqual(
            advisory["evidence"],
            {
                "sample_count": 1,
                "evidence_class": "regression_sample",
            },
        )
        self.assertEqual(advisory["hard_failures"], [])
        self.assertEqual(advisory["identity"], comparison_identity())

        hard = compare_abba(
            rounds,
            identity=comparison_identity(),
            baseline_machine_fingerprint="machine-a",
            treatment_machine_fingerprint="machine-a",
            baseline_samples=[sample()],
            treatment_samples=[sample(actual_digest="b" * 64)],
            relative_threshold=0.10,
            practical_floor=5,
            resamples=100,
        )
        self.assertEqual(hard["decision"], "fail")
        self.assertTrue(hard["hard_failures"])

    def test_n1_is_regression_evidence_without_slo_gate_metadata(self) -> None:
        result = compare_abba(
            [[("A", 100), ("B", 100), ("B", 100), ("A", 100)]],
            identity=comparison_identity(),
            baseline_machine_fingerprint="machine-a",
            treatment_machine_fingerprint="machine-a",
            resamples=100,
        )

        self.assertEqual(result["decision"], "descriptive_only")
        self.assertEqual(result["evidence"]["evidence_class"], "regression_sample")
        self.assertNotIn("slo_eligible", result["evidence"])
        self.assertNotIn("baseline_policy_met", result["evidence"])
        self.assertNotIn("accepted", result)

    def test_distribution_latency_findings_remain_descriptive_only(self) -> None:
        result = compare_abba(
            [
                [("A", 100), ("B", 130), ("B", 130), ("A", 100)],
                [("A", 100), ("B", 130), ("B", 130), ("A", 100)],
            ],
            identity=comparison_identity(),
            baseline_machine_fingerprint="machine-a",
            treatment_machine_fingerprint="machine-a",
            evidence_class="distribution",
            relative_threshold=0.10,
            practical_floor=5,
            resamples=100,
        )

        self.assertEqual(result["evidence"]["sample_count"], 2)
        self.assertEqual(result["evidence"]["evidence_class"], "distribution")
        self.assertEqual(result["decision"], "descriptive_only")
        self.assertNotIn("slo_eligible", result["evidence"])

    def test_pr_stage_and_milestone_comparison_identities_are_rejected(self) -> None:
        rounds = [[("A", 100), ("B", 100), ("B", 100), ("A", 100)]]
        for field, value in (
            ("journey_id", "pr14-stage"),
            ("workload_id", "pr15-milestone"),
            ("scenario", "pr16"),
            ("milestone_budget_ns", 1),
        ):
            with self.subTest(field=field):
                with self.assertRaisesRegex(ValueError, "PR-stage|unexpected"):
                    compare_abba(
                        rounds,
                        identity=comparison_identity(**{field: value}),
                        baseline_machine_fingerprint="machine-a",
                        treatment_machine_fingerprint="machine-a",
                        resamples=100,
                    )


class CorrectnessTests(unittest.TestCase):
    def test_digest_errors_and_timeout_rate_regressions_are_hard_failures(self) -> None:
        failures = aggregate_hard_failures(
            [sample(), sample(), sample()],
            [
                sample(actual_digest="b" * 64),
                sample("error", actual_digest=None, error="unexpected"),
                sample("timeout", actual_digest=None, error="deadline exceeded"),
            ],
        )

        self.assertEqual(
            {failure["code"] for failure in failures},
            {
                "digest_mismatch",
                "unexpected_error",
                "error_rate_regression",
                "timeout_rate_regression",
            },
        )

    def test_daemon_death_is_a_hard_failure_even_without_latency_regression(self) -> None:
        failures = aggregate_hard_failures(
            [sample()],
            [
                sample(
                    "error",
                    actual_digest=None,
                    error="daemon exited",
                    daemon_survived=False,
                )
            ],
        )

        self.assertIn("daemon_death", {failure["code"] for failure in failures})

    def test_latency_budgets_are_advisory_findings(self) -> None:
        findings = latency_budget_findings(
            {"p50_ns": 90, "p99_ns": 150},
            {"p50_ns": 100, "p99_ns": 125},
        )

        self.assertEqual(
            findings,
            [
                {
                    "metric": "p99_ns",
                    "observed": 150.0,
                    "budget": 125.0,
                    "severity": "advisory",
                    "code": "latency_budget_exceeded",
                }
            ],
        )
        self.assertNotIn("milestone", str(findings).lower())


if __name__ == "__main__":
    unittest.main()
