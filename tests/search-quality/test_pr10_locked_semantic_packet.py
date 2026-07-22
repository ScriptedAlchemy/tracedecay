#!/usr/bin/env python3
"""Contract tests for the pending PR10 locked semantic evaluation packet."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any


REPOSITORY = Path(__file__).resolve().parents[2]
PACKET_DIR = REPOSITORY / "benchmarks/pr10-semantic-search"
WORKLOAD_PATH = PACKET_DIR / "workload-v1.json"
RESULT_PATH = PACKET_DIR / "result-pending.json"
VALIDATOR_PATH = PACKET_DIR / "validate_packet.py"

VALIDATOR_SPEC = importlib.util.spec_from_file_location(
    "pr10_semantic_packet_validator",
    VALIDATOR_PATH,
)
if VALIDATOR_SPEC is None or VALIDATOR_SPEC.loader is None:
    raise RuntimeError("cannot load PR10 semantic packet validator")
VALIDATOR = importlib.util.module_from_spec(VALIDATOR_SPEC)
VALIDATOR_SPEC.loader.exec_module(VALIDATOR)


def load_object(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise AssertionError(f"{path} must contain an object")
    return value


def sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


class Pr10LockedSemanticPacketTest(unittest.TestCase):
    def test_pending_packet_is_unmeasured_and_non_promoting(self) -> None:
        workload = load_object(WORKLOAD_PATH)
        result = load_object(RESULT_PATH)

        self.assertEqual(workload["acceptance"]["state"], "pending_parent_gates")
        self.assertEqual(result["outcome"], "pending")
        self.assertFalse(result["acceptance_authority"])
        self.assertIsNone(result["measured_results"])
        self.assertIsNone(result["promotion_evidence"])
        self.assertTrue(result["semantic_activation_disabled"])

    def test_real_corpus_bytes_match_every_pin(self) -> None:
        workload = load_object(WORKLOAD_PATH)
        corpus = workload["corpus"]

        self.assertEqual(corpus["provider"], "checked_in_real_repo_fixture")
        self.assertGreaterEqual(len(corpus["files"]), 10)
        for artifact in corpus["files"]:
            path = REPOSITORY / artifact["path"]
            self.assertTrue(path.is_file(), artifact["path"])
            self.assertEqual(path.stat().st_size, artifact["byte_len"])
            self.assertEqual(sha256(path), artifact["digest"])

    def test_exact_flat_is_oracle_and_research_profiles_stay_candidates(self) -> None:
        workload = load_object(WORKLOAD_PATH)
        profiles = {profile["id"]: profile for profile in workload["profiles"]}

        self.assertEqual(
            profiles["semantic-exact-flat"]["role"],
            "production_baseline_and_oracle",
        )
        self.assertEqual(
            profiles["semantic-exact-flat"]["production_api"],
            "SemanticVectorReadPort::scan_exact_flat",
        )
        self.assertEqual(profiles["semantic-exact-flat"]["search_kind"], "exact_flat")
        for profile_id in (
            "semantic-ann",
            "semantic-late-interaction",
            "semantic-quantized",
        ):
            self.assertEqual(profiles[profile_id]["state"], "candidate_evidence_required")
            self.assertFalse(profiles[profile_id]["activation_eligible"])

    def test_acceptance_covers_calibration_fallback_resources_and_rollback(self) -> None:
        workload = load_object(WORKLOAD_PATH)

        self.assertEqual(
            workload["calibration"]["invalid_or_shifted_behavior"],
            "abstain_and_report_invalid_calibration",
        )
        self.assertEqual(
            workload["fallback"]["comparison"],
            "byte_identical_pr9_fallback_subpayload",
        )
        self.assertEqual(
            {stratum["scale"] for stratum in workload["resource_strata"]},
            {"current", "10x"},
        )
        self.assertEqual(
            {stratum["platform"] for stratum in workload["platform_strata"]},
            {"linux", "windows"},
        )
        self.assertTrue(workload["rollback"]["cold_start"])
        self.assertTrue(workload["rollback"]["offline"])
        self.assertEqual(workload["rollback"]["on_failure"], "semantic_disabled")

    def test_callable_contracts_cover_non_blocking_indexing_and_atomic_activation(self) -> None:
        workload = load_object(WORKLOAD_PATH)

        VALIDATOR.validate_feature_contract(REPOSITORY)
        VALIDATOR.validate_production_apis(workload, REPOSITORY)
        VALIDATOR.validate_model_integrity(workload, REPOSITORY)
        VALIDATOR.validate_async_activation(workload)
        VALIDATOR.validate_callable_acceptance(workload, REPOSITORY)
        VALIDATOR.validate_plan_contract(REPOSITORY)

        asynchronous = workload["asynchronous_activation"]
        self.assertFalse(asynchronous["baseline_waits_for_semantic"])
        self.assertFalse(asynchronous["excluded_states_affect_rank"])
        self.assertEqual(
            asynchronous["search_during_indexing_comparison"],
            "byte_identical_pr9_fallback_and_rank",
        )
        self.assertEqual(
            asynchronous["activation_visibility"],
            "generation_and_active_pointer_single_atomic_step",
        )

    def test_callable_validator_rejects_scaffolding_and_blocking_semantics(self) -> None:
        workload = load_object(WORKLOAD_PATH)

        blocking = copy.deepcopy(workload)
        blocking["asynchronous_activation"]["baseline_waits_for_semantic"] = True
        with self.assertRaisesRegex(VALIDATOR.PacketError, "cannot wait"):
            VALIDATOR.validate_async_activation(blocking)

        scaffold = copy.deepcopy(workload)
        scaffold["production_apis"]["fastembed_runtime"]["required_symbols"] = [
            "FastEmbedEmbeddingRuntime"
        ]
        with self.assertRaisesRegex(VALIDATOR.PacketError, "symbol scaffolding"):
            VALIDATOR.validate_production_apis(scaffold, REPOSITORY)

    def test_every_binding_requirement_is_audited_but_not_accepted(self) -> None:
        workload = load_object(WORKLOAD_PATH)
        audit = {entry["id"]: entry for entry in workload["implementation_audit"]}

        self.assertEqual(set(audit), VALIDATOR.EXPECTED_AUDIT_REQUIREMENTS)
        self.assertEqual(
            audit["locked_evidence_before_activation"]["delivery"],
            "activation_guard_delivered_evidence_consumption_pending",
        )
        self.assertTrue(
            all(
                entry["locked_acceptance"] == "pending_parent_execution"
                for entry in audit.values()
            )
        )

    def test_validator_lints_but_strict_acceptance_remains_pending(self) -> None:
        lint = subprocess.run(
            [sys.executable, str(VALIDATOR_PATH)],
            cwd=REPOSITORY,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(lint.returncode, 0, lint.stderr)
        self.assertIn("pending_parent_gates", lint.stdout)

        strict = subprocess.run(
            [sys.executable, str(VALIDATOR_PATH), "--strict"],
            cwd=REPOSITORY,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(strict.returncode, 3, strict.stderr)
        self.assertIn("acceptance pending", strict.stderr)


if __name__ == "__main__":
    unittest.main()
