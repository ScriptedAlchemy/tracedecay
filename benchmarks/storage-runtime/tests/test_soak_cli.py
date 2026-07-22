"""End-to-end CLI coverage for offline soak planning and evaluation."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import run_storage_baseline as rsb  # noqa: E402
from soak.trends import RESOURCE_NAMES  # noqa: E402

HASH = "a" * 64


def baseline(platform: str, *, evidence: bool = True) -> dict:
    components = {
        name: {
            "verified": True,
            "kind": "tree" if name == "corpus" else "file",
            "sha256": HASH,
        }
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
        "status": "completed" if evidence else "not_evidence",
        "evidence_status": {
            "state": "evidence" if evidence else "not_evidence",
            "reasons": [] if evidence else ["fixture is synthetic"],
        },
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


def soak_result(plan: dict) -> dict:
    resources = {name: 100 for name in RESOURCE_NAMES}
    sustained = []
    for phase in plan["sustained"]:
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
    limits = {name: 1.1 for name in RESOURCE_NAMES}
    environment = {
        "platform": "test",
        "python": "test",
        "psutil": "test",
    }
    environment["sha256"] = rsb.sha256_bytes(
        rsb.canonical_compact_json(environment).encode()
    )
    payload = {
        "schema": "storage-runtime-soak-result-v2",
        "plan_identity": {"sha256": plan["plan_sha256"]},
        "workload_identity": {
            "id": plan["workload_id"],
            "implementation_sha256": "c" * 64,
        },
        "commit_identity": {"sha": "d" * 40},
        "binary_identity": {
            "product_sha256": "e" * 64,
            "evidence_sha256": "f" * 64,
        },
        "environment_identity": environment,
        "resource_samples": [
            {"elapsed_seconds": second, **resources}
            for second in range(plan["duration_seconds"] + 1)
        ],
        "post_eviction": resources,
        "trend_policy": {
            "maximum_slope_per_second": {name: 0 for name in RESOURCE_NAMES},
            "maximum_end_to_baseline_ratio": limits,
            "maximum_post_eviction_ratio": limits,
            "maximum_cadence_gap_seconds": 2,
        },
    }
    receipt = {
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
            "plan_sha256": plan["plan_sha256"],
            "workload_id": plan["workload_id"],
            "workload_implementation_sha256": "c" * 64,
            "commit_sha": "d" * 40,
            "product_binary_sha256": "e" * 64,
            "evidence_binary_sha256": "f" * 64,
            "environment_sha256": environment["sha256"],
            "payload_sha256": rsb.sha256_bytes(
                rsb.canonical_compact_json(payload).encode()
            ),
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
            "product_gate_evidence": [],
            "product_commit_sha": None,
            "sustained": sustained,
            "crash_count_completed": plan["crash_count"],
            "crash_recovery_count": plan["crash_count"],
            "restore_rehearsal_count": plan["restore_rehearsal_count"],
            "restore_verified_count": plan["restore_rehearsal_count"],
    }
    receipt["receipt_sha256"] = rsb.sha256_bytes(
        rsb.canonical_compact_json(receipt).encode()
    )
    return {**payload, "execution_receipt": receipt}


class SoakCliTests(unittest.TestCase):
    def test_plan_and_evaluate_explicit_temp_artifacts(self):
        with tempfile.TemporaryDirectory(prefix="soak-cli-") as temporary:
            root = Path(temporary)
            plan_path = root / "plan.json"
            self.assertEqual(
                rsb.main(
                    [
                        "soak-plan", "--seed", "7", "--duration-seconds", "3",
                        "--current-rate", "1", "--ten-x-rate", "2",
                        "--overload-rate", "3", "--crash-count", "0",
                        "--restore-rehearsals", "1",
                        "--workload-id", "storage-runtime-product-fts-v1",
                        "--output", str(plan_path),
                    ]
                ),
                0,
            )
            plan = json.loads(plan_path.read_text(encoding="utf-8"))
            self.assertEqual(plan["safety"]["profile_discovery"], "forbidden")

            baselines = []
            for platform in ("linux", "windows", "macos"):
                path = root / f"{platform}.json"
                path.write_text(json.dumps(baseline(platform)), encoding="utf-8")
                baselines.extend(["--baseline", str(path.resolve())])
            result_path = root / "result.json"
            result_path.write_text(json.dumps(soak_result(plan)), encoding="utf-8")
            assessment_path = root / "assessment.json"
            self.assertEqual(
                rsb.main(
                    ["soak-evaluate", *baselines, "--plan", str(plan_path),
                     "--result", str(result_path), "--output", str(assessment_path)]
                ),
                0,
            )
            assessment = json.loads(assessment_path.read_text(encoding="utf-8"))
            self.assertEqual(assessment["evidence_status"]["state"], "evidence")

    def test_not_evidence_baseline_cannot_be_promoted(self):
        with tempfile.TemporaryDirectory(prefix="soak-cli-") as temporary:
            root = Path(temporary)
            plan_path = root / "plan.json"
            self.assertEqual(
                rsb.main([
                    "soak-plan", "--seed", "1", "--duration-seconds", "2",
                    "--current-rate", "1", "--ten-x-rate", "2",
                    "--overload-rate", "3", "--crash-count", "0",
                    "--restore-rehearsals", "0", "--output", str(plan_path),
                ]),
                0,
            )
            plan = json.loads(plan_path.read_text(encoding="utf-8"))
            result_path = root / "result.json"
            result_path.write_text(json.dumps(soak_result(plan)), encoding="utf-8")
            paths = []
            for platform in ("linux", "windows", "macos"):
                path = root / f"{platform}.json"
                path.write_text(
                    json.dumps(baseline(platform, evidence=platform != "linux")),
                    encoding="utf-8",
                )
                paths.extend(["--baseline", str(path.resolve())])
            output = root / "assessment.json"
            self.assertEqual(
                rsb.main(["soak-evaluate", *paths, "--plan", str(plan_path),
                          "--result", str(result_path), "--output", str(output)]),
                2,
            )
            assessment = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(assessment["status"], "not_evidence")
            self.assertIn("marked not-evidence", " ".join(
                assessment["evidence_status"]["reasons"]
            ))
            lint_output = root / "lint-assessment.json"
            self.assertEqual(
                rsb.main(
                    [
                        "soak-evaluate",
                        *paths,
                        "--plan",
                        str(plan_path),
                        "--result",
                        str(result_path),
                        "--output",
                        str(lint_output),
                        "--mode",
                        "lint",
                    ]
                ),
                0,
            )


if __name__ == "__main__":
    unittest.main()
