from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from soak.trends import RESOURCE_NAMES, TrendError, TrendPolicy, evaluate_resource_trends  # noqa: E402


def limits(value: float) -> dict[str, float]:
    return {name: value for name in RESOURCE_NAMES}


class TrendTests(unittest.TestCase):
    def policy(self) -> TrendPolicy:
        return TrendPolicy(
            maximum_slope_per_second=limits(1.0),
            maximum_end_to_baseline_ratio=limits(1.2),
            maximum_post_eviction_ratio=limits(1.05),
        )

    def samples(self) -> list[dict]:
        return [
            {"elapsed_seconds": 0, **{name: 100 for name in RESOURCE_NAMES}},
            {"elapsed_seconds": 10, **{name: 102 for name in RESOURCE_NAMES}},
            {"elapsed_seconds": 20, **{name: 104 for name in RESOURCE_NAMES}},
        ]

    def test_stable_resources_and_post_eviction_pass(self):
        result = evaluate_resource_trends(
            self.samples(), {name: 99 for name in RESOURCE_NAMES}, self.policy()
        )
        self.assertTrue(result["pass"])
        self.assertTrue(all(item["pass"] for item in result["resources"].values()))

    def test_wal_growth_regression_fails_even_when_other_resources_pass(self):
        samples = self.samples()
        samples[-1]["wal_bytes"] = 150
        result = evaluate_resource_trends(
            samples, {name: 100 for name in RESOURCE_NAMES}, self.policy()
        )
        self.assertFalse(result["pass"])
        self.assertFalse(result["resources"]["wal_bytes"]["pass"])
        self.assertTrue(result["resources"]["rss_bytes"]["pass"])

    def test_post_eviction_regression_is_independent_gate(self):
        post = {name: 100 for name in RESOURCE_NAMES}
        post["fd_count"] = 110
        result = evaluate_resource_trends(self.samples(), post, self.policy())
        self.assertFalse(result["resources"]["fd_count"]["checks"]["post_eviction_ratio"])

    def test_non_monotonic_observations_are_rejected(self):
        samples = self.samples()
        samples[2]["elapsed_seconds"] = 10
        with self.assertRaises(TrendError):
            evaluate_resource_trends(samples, limits(100), self.policy())


if __name__ == "__main__":
    unittest.main()
