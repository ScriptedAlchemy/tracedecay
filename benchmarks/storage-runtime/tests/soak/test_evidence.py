from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from soak.evidence import EvidenceError, assess_evidence, load_baselines  # noqa: E402
from soak.scheduler import CampaignConfig, build_campaign  # noqa: E402
from soak.trends import RESOURCE_NAMES  # noqa: E402

HASH = "a" * 64
RESOURCES = RESOURCE_NAMES


def baseline(platform: str) -> dict:
    components = {
        name: {"verified": True, "kind": "tree" if name == "corpus" else "file", "sha256": HASH}
        for name in (
            "product_binary",
            "evidence_binary",
            "schema_manifest",
            "workload",
            "corpus",
            "config",
        )
    }
    components["evidence_binary"]["sha256"] = "b" * 64
    return {
        "artifact_id": "storage-runtime-baseline-result-v2",
        "schema_version": 2,
        "status": "completed",
        "evidence_status": {"state": "evidence", "reasons": []},
        "execution_scope": {"mode": "full"},
        "workload": {"evidence_eligible": True},
        "platform": {"current": platform},
        "frozen_identity": {"status": "supplied", "sha256": HASH},
        "identity_binding": {
            "status": "bound",
            "product_commit_sha": "d" * 40,
            "components": components,
        },
        "runs": [
            {
                "status": "completed",
                "evidence": {
                    "state": {
                        "schema": "storage-runtime-logical-sqlite-evidence-v1",
                        "integrity": {"status": "ok"},
                    }
                },
            }
        ],
        "product_adapter_output": {
            "schema": "tracedecay-storage-runtime-product-probe-v1",
            "status": "not_evidence",
            "evidence_status": {
                "state": "not_evidence",
                "reasons": ["standalone adapter output"],
            },
            "operation": "fts",
            "family": "graph",
            "product_output": {
                "redacted": True,
                "sha256": HASH,
                "byte_count": 1,
            },
        },
    }


def plan() -> dict:
    return build_campaign(
        CampaignConfig(
            seed=7,
            duration_seconds=3,
            rates_per_second={"current": 1, "ten_x": 2, "overload": 3},
            crash_count=0,
            restore_rehearsals=1,
        )
    )


def receipt(campaign: dict | None = None) -> dict:
    campaign = campaign or plan()
    sustained = []
    for phase in campaign["sustained"]:
        offered = phase["offered_count"]
        sustained.append(
            {
                "scale": phase["scale"],
                "offered": offered,
                "admitted": offered,
                "completed": offered,
                "failed": 0,
                "shed_runner_in_flight": 0,
                "shed_command_saturation": 0,
                "terminal": offered,
                "latency_origin": "scheduled_issue_time",
            }
        )
    return {
        "schema": "storage-runtime-soak-execution-receipt-v2",
        "executor_id": "tracedecay-storage-runtime-soak-executor",
        "executor_version": 1,
        "artifact_schema": "storage-runtime-soak-result-v2",
        "status": "completed",
        "coordinated_omission": False,
        "artifacts_bounded": True,
        "fixture_source": "explicit",
        "fixture_schema": "storage-runtime-fixture-v1",
        "fixture_verified": True,
        "product_adapter_validated": True,
        "fixture_sha256": "b" * 64,
        "frozen_identity_sha256": HASH,
        "plan_sha256": campaign["plan_sha256"],
        "workload_id": campaign["workload_id"],
        "commit_sha": "d" * 40,
        "product_binary_sha256": "c" * 64,
        "evidence_binary_sha256": "d" * 64,
        "logical_evidence": [
            {
                "schema": "storage-runtime-logical-sqlite-evidence-v1",
                "integrity": {
                    "status": "ok",
                    "result_sha256": HASH,
                    "result_row_count": 1,
                },
                "schema_sha256": HASH,
                "tables": [],
                "fts": [],
            }
        ],
        "sustained": sustained,
        "crash_count_completed": campaign["crash_count"],
        "crash_recovery_count": campaign["crash_count"],
        "restore_rehearsal_count": campaign["restore_rehearsal_count"],
        "restore_verified_count": campaign["restore_rehearsal_count"],
    }


def passing_trends() -> dict:
    return {
        "pass": True,
        "resources": {
            name: {
                "pass": True,
                "checks": {
                    "slope": True,
                    "end_ratio": True,
                    "post_eviction_ratio": True,
                },
                "slope_per_second": 0.0,
                "end_to_baseline_ratio": 1.0,
                "post_eviction_to_baseline_ratio": 1.0,
                "sample_count": 3,
            }
            for name in RESOURCES
        },
    }


class EvidenceTests(unittest.TestCase):
    def write(self, root: Path, platform: str, document: dict | None = None) -> Path:
        path = (root / f"{platform}.json").resolve()
        path.write_text(json.dumps(document or baseline(platform)), encoding="utf-8")
        return path

    def complete_set(self, root: Path):
        return load_baselines(
            [self.write(root, platform) for platform in ("linux", "windows", "macos")]
        )

    def test_complete_frozen_cross_platform_fixture_set_can_be_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            baselines = self.complete_set(Path(temporary))
            campaign = plan()
            result = assess_evidence(baselines, campaign, passing_trends(), receipt(campaign))
        self.assertEqual(result["evidence_status"]["state"], "evidence")
        self.assertEqual(result["platforms"], ["linux", "macos", "windows"])
        self.assertEqual(len(result["baseline_artifact_sha256"]), 3)

    def test_incomplete_or_not_evidence_baseline_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document = baseline("linux")
            document["evidence_status"]["state"] = "not_evidence"
            document["identity_binding"]["components"].pop("corpus")
            baselines = load_baselines([self.write(root, "linux", document)])
            result = assess_evidence(baselines, plan(), passing_trends(), receipt())
        reasons = " ".join(result["evidence_status"]["reasons"])
        self.assertEqual(result["status"], "not_evidence")
        self.assertIn("marked not-evidence", reasons)
        self.assertIn("frozen identity components", reasons)
        self.assertIn("required platform baselines missing", reasons)

    def test_runtime_gate_failure_cannot_claim_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            baselines = self.complete_set(Path(temporary))
            campaign = plan()
            incomplete = receipt(campaign)
            incomplete["sustained"][0]["terminal"] -= 1
            result = assess_evidence(
                baselines, campaign, {"pass": False, "resources": {}}, incomplete
            )
        reasons = result["evidence_status"]["reasons"]
        self.assertIn("one or more resource trend gates failed", reasons)
        self.assertIn("current open-loop counts violate terminal invariants", reasons)

    def test_baseline_without_logical_or_adapter_evidence_cannot_promote(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document = baseline("linux")
            document["runs"] = []
            document.pop("product_adapter_output")
            baselines = load_baselines([self.write(root, "linux", document)])
            result = assess_evidence(baselines, plan(), passing_trends(), receipt())
        reasons = " ".join(result["evidence_status"]["reasons"])
        self.assertIn("no executed product runs", reasons)
        self.assertIn("lacks validated product adapter output", reasons)
        self.assertEqual(result["status"], "not_evidence")

    def test_relative_or_directory_input_is_never_discovered(self):
        with self.assertRaises(EvidenceError):
            load_baselines([Path("baseline.json")])
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(EvidenceError):
                load_baselines([Path(temporary).resolve()])

    def test_oversized_baseline_is_rejected_before_loading(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = (Path(temporary) / "oversized.json").resolve()
            with path.open("wb") as handle:
                handle.truncate(16 * 1024 * 1024 + 1)
            with self.assertRaises(EvidenceError):
                load_baselines([path])


if __name__ == "__main__":
    unittest.main()
