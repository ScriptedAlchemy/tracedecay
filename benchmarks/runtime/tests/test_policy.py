from __future__ import annotations

import hashlib
import json
import unittest
from pathlib import Path

from benchmarks.runtime.policy import (
    PolicyViolation,
    evaluate_artifact,
    load_acceptance_policy,
    make_policy_receipt,
)


ROOT = Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "policies" / "acceptance-v1.json"


class AcceptancePolicyTests(unittest.TestCase):
    def test_policy_is_independently_versioned_and_hashed_into_receipt(self) -> None:
        policy = load_acceptance_policy(POLICY_PATH)
        receipt = make_policy_receipt(policy, artifact_sha256="a" * 64)

        self.assertEqual(policy.policy_id, "runtime-acceptance-v1")
        self.assertEqual(policy.policy_version, 1)
        self.assertEqual(
            receipt["policy_sha256"],
            hashlib.sha256(POLICY_PATH.read_bytes()).hexdigest(),
        )
        self.assertEqual(receipt["artifact_sha256"], "a" * 64)
        self.assertEqual(receipt["policy_id"], policy.policy_id)

    def test_producer_cannot_relax_acceptance_policy(self) -> None:
        policy = load_acceptance_policy(POLICY_PATH)
        producer_artifact = {
            "schema_version": 1,
            "sample_count": 1,
            "measurements": [{"elapsed_ns": 1}],
            "acceptance_thresholds": {
                "p95_minimum_matching_samples": 1,
                "p99_minimum_matching_samples": 1,
                "latency_ns": 10**18,
            },
        }

        with self.assertRaisesRegex(PolicyViolation, "producer-authored"):
            evaluate_artifact(producer_artifact, policy)

    def test_percentile_eligibility_comes_only_from_policy(self) -> None:
        policy = load_acceptance_policy(POLICY_PATH)

        below_p95 = evaluate_artifact(
            {"schema_version": 1, "sample_count": 39, "measurements": []},
            policy,
        )
        p95_only = evaluate_artifact(
            {"schema_version": 1, "sample_count": 40, "measurements": []},
            policy,
        )
        p99 = evaluate_artifact(
            {"schema_version": 1, "sample_count": 100, "measurements": []},
            policy,
        )

        self.assertFalse(below_p95["p95_eligible"])
        self.assertFalse(below_p95["p99_eligible"])
        self.assertTrue(p95_only["p95_eligible"])
        self.assertFalse(p95_only["p99_eligible"])
        self.assertTrue(p99["p95_eligible"])
        self.assertTrue(p99["p99_eligible"])
        self.assertEqual(policy.latency_mode, "advisory")

    def test_policy_file_is_canonical_json(self) -> None:
        document = json.loads(POLICY_PATH.read_text(encoding="utf-8"))
        expected = json.dumps(
            document,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        ) + "\n"
        self.assertEqual(POLICY_PATH.read_text(encoding="utf-8"), expected)


if __name__ == "__main__":
    unittest.main()
