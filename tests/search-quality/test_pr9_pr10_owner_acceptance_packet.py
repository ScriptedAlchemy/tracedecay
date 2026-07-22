#!/usr/bin/env python3
"""Direct validator tests for PR9/PR10 unsigned owner-acceptance contracts."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
VALIDATOR = REPO / "benchmarks/pr9-pr10-owner-acceptance/validate_packet.py"
PACKET = REPO / "benchmarks/pr9-pr10-owner-acceptance/packet-v1.json"
HOLDOUT_LABELS = REPO / ".owner-evidence/pr9-pr10-holdout-labels-v1.json"
OWNER_DECISION = REPO / "benchmarks/pr9-pr10-owner-acceptance/owner-decision-v1.json"


class Pr9Pr10OwnerAcceptancePacketTest(unittest.TestCase):
    def test_packet_validates_without_inventing_acceptance(self) -> None:
        completed = subprocess.run(
            ["python3", str(VALIDATOR), "--packet", str(PACKET)],
            cwd=REPO,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        self.assertEqual(payload["status"], "ok")
        self.assertEqual(payload["evidence_control"], "canonical_sha256_only")
        self.assertEqual(payload["digests"]["owner_decision"], "absent_no_acceptance_invented")

    def test_holdout_labels_bind_sealed_queries_when_present(self) -> None:
        if not HOLDOUT_LABELS.is_file():
            self.skipTest("real holdout labels not present yet")
        completed = subprocess.run(
            [
                "python3",
                str(VALIDATOR),
                "--packet",
                str(PACKET),
                "--holdout-labels",
                str(HOLDOUT_LABELS),
            ],
            cwd=REPO,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        self.assertIn("label_digest", payload["digests"])
        self.assertTrue(payload["digests"]["label_digest"].startswith("sha256:"))

    def test_existing_gate_owner_decision_validates_without_acceptance(self) -> None:
        if not HOLDOUT_LABELS.is_file() or not OWNER_DECISION.is_file():
            self.skipTest("real holdout labels/owner decision not present yet")
        completed = subprocess.run(
            [
                "python3",
                str(VALIDATOR),
                "--packet",
                str(PACKET),
                "--holdout-labels",
                str(HOLDOUT_LABELS),
                "--owner-decision",
                str(OWNER_DECISION),
            ],
            cwd=REPO,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        decision = json.loads(OWNER_DECISION.read_text(encoding="utf-8"))
        self.assertNotEqual(decision.get("outcome"), "accepted")
        self.assertFalse(decision.get("promotion_allowed"))
        self.assertIn("owner_decision_sha256", payload["digests"])

    def test_owner_decision_rejects_signature_fields_and_blocked_outcome(self) -> None:
        if not HOLDOUT_LABELS.is_file():
            self.skipTest("real holdout labels not present yet")
        packet = json.loads(PACKET.read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as tmp:
            decision_path = Path(tmp) / "owner-decision.json"
            digest = "sha256:" + ("11" * 32)
            decision_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "decision_kind": "owner_decision_v1",
                        "authority": packet["authority"],
                        "source_repository_commit": packet["source_repository_commit"],
                        "source_repository_tree": packet["source_repository_tree"],
                        "corpus_digest": digest,
                        "partition_digest": digest,
                        "label_digest": digest,
                        "profile_digest": digest,
                        "toolchain_digest": digest,
                        "hardware_digest": digest,
                        "report_digest": digest,
                        "evidence_index_digest": digest,
                        "outcome": "blocked",
                        "decided_by": "owner-search-quality-lead",
                        "rationale": "blocked is not an owner-decision terminal",
                        "gate_receipt_digests": [],
                        "digest": digest,
                        "signature_locator": "authorized-store://forbidden",
                    },
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
            completed = subprocess.run(
                [
                    "python3",
                    str(VALIDATOR),
                    "--packet",
                    str(PACKET),
                    "--holdout-labels",
                    str(HOLDOUT_LABELS),
                    "--owner-decision",
                    str(decision_path),
                ],
                cwd=REPO,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("forbids deleted signing/reveal field", completed.stderr)


if __name__ == "__main__":
    unittest.main()
