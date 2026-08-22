#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from types import ModuleType


CHECKER_PATH = Path(__file__).with_name("check-pr-dogfood-output.py")


def load_checker() -> ModuleType:
    if not CHECKER_PATH.exists():
        raise AssertionError(f"dogfood validator is missing: {CHECKER_PATH}")
    spec = importlib.util.spec_from_file_location("pr_dogfood_output", CHECKER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load dogfood validator from {CHECKER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PrDogfoodOutputTests(unittest.TestCase):
    def setUp(self) -> None:
        self.checker = load_checker()
        self.payload = {
            "status": "partial",
            "base_oid": "base-oid",
            "head_oid": "head-oid",
            "merge_base": "merge-base-oid",
            "files_changed": 1,
            "changes": [{"path": "src/lib.rs", "status": "modified"}],
            "analysis_coverage": {"complete": False},
            "verified_graph_evidence": {
                "status": "unavailable",
                "reason_code": "code-graph-unavailable",
                "retryable": True,
            },
        }

    def validate(self, payload: dict[str, object]) -> None:
        self.checker.validate_pr_context(
            payload,
            expected_base_oid="base-oid",
            expected_head_oid="head-oid",
            expected_merge_base="merge-base-oid",
        )

    def test_accepts_exact_partial_warmup_evidence(self) -> None:
        self.validate(self.payload)

    def test_accepts_complete_output_without_graph_unavailability(self) -> None:
        del self.payload["status"]
        del self.payload["verified_graph_evidence"]
        self.payload["analysis_coverage"] = {"complete": True}
        self.validate(self.payload)

    def test_rejects_evidence_for_a_different_head(self) -> None:
        self.payload["head_oid"] = "wrong-head"
        with self.assertRaisesRegex(ValueError, "head_oid"):
            self.validate(self.payload)

    def test_rejects_terminal_graph_failure_as_warmup(self) -> None:
        self.payload["verified_graph_evidence"] = {
            "status": "unavailable",
            "reason_code": "code-graph-corrupt",
            "retryable": False,
        }
        with self.assertRaisesRegex(ValueError, "transient"):
            self.validate(self.payload)

    def test_rejects_complete_coverage_claim_on_partial_output(self) -> None:
        self.payload["analysis_coverage"] = {"complete": True}
        with self.assertRaisesRegex(ValueError, "incomplete"):
            self.validate(self.payload)

    def test_rejects_partial_output_without_typed_graph_evidence(self) -> None:
        del self.payload["verified_graph_evidence"]
        with self.assertRaisesRegex(ValueError, "graph evidence"):
            self.validate(self.payload)


if __name__ == "__main__":
    unittest.main()
