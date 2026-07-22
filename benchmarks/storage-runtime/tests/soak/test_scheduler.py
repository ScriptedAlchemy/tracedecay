from __future__ import annotations

import hashlib
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from soak.scheduler import CampaignConfig, ScheduleError, build_campaign  # noqa: E402


class SchedulerTests(unittest.TestCase):
    def config(self, seed: int = 41) -> CampaignConfig:
        return CampaignConfig(
            seed=seed,
            duration_seconds=20,
            rates_per_second={"current": 2, "ten_x": 7.5, "overload": 20},
            crash_count=4,
            restore_rehearsals=3,
            minimum_crash_spacing_seconds=1,
        )

    def test_seeded_campaign_is_reproducible(self):
        first = build_campaign(self.config())
        second = build_campaign(self.config())
        self.assertEqual(first, second)
        self.assertNotEqual(first["crashes"], build_campaign(self.config(42))["crashes"])
        unhashed = dict(first)
        unhashed.pop("plan_sha256")
        expected = hashlib.sha256(
            json.dumps(unhashed, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        self.assertEqual(first["plan_sha256"], expected)

    def test_open_loop_count_and_offsets_do_not_depend_on_completion(self):
        plan = build_campaign(self.config())
        by_scale = {item["scale"]: item for item in plan["sustained"]}
        self.assertEqual(by_scale["current"]["offered_count"], 40)
        self.assertEqual(by_scale["ten_x"]["offered_count"], 150)
        self.assertEqual(by_scale["overload"]["offered_count"], 400)
        self.assertEqual(
            by_scale["current"]["schedule_preview"][1],
            {"request_id": 1, "scheduled_offset_ns": 500_000_000},
        )
        self.assertEqual(by_scale["overload"]["latency_origin"], "scheduled_issue_time")

    def test_crashes_obey_spacing_and_restore_steps_are_explicit(self):
        plan = build_campaign(self.config())
        offsets = [item["scheduled_offset_ns"] for item in plan["crashes"]]
        self.assertTrue(all(b - a >= 1_000_000_000 for a, b in zip(offsets, offsets[1:])))
        self.assertEqual(
            plan["restores"][0]["steps"],
            ["backup", "verify_manifest", "restore", "logical_compare"],
        )

    def test_invalid_or_excessive_schedule_is_rejected(self):
        with self.assertRaises(ScheduleError):
            build_campaign(
                CampaignConfig(
                    seed=0,
                    duration_seconds=1,
                    rates_per_second={"current": 1, "ten_x": 10, "overload": 10_000_001},
                    crash_count=0,
                    restore_rehearsals=0,
                )
            )


if __name__ == "__main__":
    unittest.main()
