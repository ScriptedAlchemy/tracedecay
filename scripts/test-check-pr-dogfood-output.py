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
            "graph_generation": None,
            "next_cursor": None,
            "symbol_page": {
                "limit": 200,
                "returned": 0,
                "has_more": False,
                "complete": False,
                "selection": "unavailable",
                "continuation_available": False,
            },
            "analysis_coverage": {
                "seed_symbols_analyzed": 0,
                "symbols_returned": 0,
                "symbols_complete": False,
                "impact_nodes_admitted": 0,
                "impact_nodes_returned": 0,
                "direct_call_edges_admitted": 0,
                "impact_bytes_admitted": 0,
                "impact_partial": True,
                "complete": False,
            },
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

    def validate_strict(self, payload: dict[str, object]) -> None:
        self.checker.validate_pr_context(
            payload,
            expected_base_oid="base-oid",
            expected_head_oid="head-oid",
            expected_merge_base="merge-base-oid",
            strict=True,
        )

    def test_accepts_exact_partial_warmup_evidence(self) -> None:
        self.validate(self.payload)

    def test_accepts_complete_output_without_graph_unavailability(self) -> None:
        del self.payload["status"]
        del self.payload["verified_graph_evidence"]
        self.payload["analysis_coverage"] = {"complete": True}
        self.validate(self.payload)

    def test_strict_accepts_graph_ready_bounded_prefix_with_more_symbols(self) -> None:
        del self.payload["status"]
        del self.payload["verified_graph_evidence"]
        self.payload["graph_generation"] = "code-graph:sha256:ready-generation"
        self.payload["next_cursor"] = "pr-context.cursor.next"
        self.payload["symbol_page"] = {
            "limit": 500,
            "returned": 500,
            "has_more": True,
            "complete": False,
            "selection": "stable_prefix",
            "continuation_available": True,
        }
        self.payload["analysis_coverage"] = {
            "seed_symbols_analyzed": 500,
            "symbols_returned": 500,
            "symbols_complete": False,
            "impact_nodes_admitted": 700,
            "impact_nodes_returned": 700,
            "direct_call_edges_admitted": 900,
            "impact_bytes_admitted": 65536,
            "impact_partial": True,
            "complete": False,
        }
        self.validate_strict(self.payload)

    def test_strict_rejects_partial_graph_unavailability(self) -> None:
        with self.assertRaisesRegex(ValueError, "strict.*unavailable graph"):
            self.validate_strict(self.payload)

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


class StrictReadinessOutputTests(unittest.TestCase):
    def setUp(self) -> None:
        self.checker = load_checker()

    def test_strict_status_accepts_current_text_and_observed_graph(self) -> None:
        self.checker.validate_status(
            {
                "code_index_freshness": {
                    "status": "current",
                    "worktree": {
                        "coverage": "complete",
                        "staleness_state": "fresh",
                        "latest_generation_id": "generation.ready",
                        "code_graph_serving": {"state": "ready"},
                    },
                },
                "graph_statistics": {
                    "state": "observed",
                    "generation_id": "generation.ready",
                    "symbol_count": 12,
                    "edge_count": 9,
                },
            },
            strict=True,
        )

    def test_strict_status_rejects_graph_that_is_not_ready_to_serve(self) -> None:
        for graph_serving in (
            {"state": "pending"},
            {"state": "refused", "reason": "projection_failed"},
            {"state": "unavailable", "reason": "generation_unavailable"},
            None,
        ):
            with self.subTest(graph_serving=graph_serving):
                worktree = {
                    "coverage": "complete",
                    "staleness_state": "fresh",
                    "latest_generation_id": "generation.ready",
                }
                if graph_serving is not None:
                    worktree["code_graph_serving"] = graph_serving
                with self.assertRaisesRegex(ValueError, "ready code-graph"):
                    self.checker.validate_status(
                        {
                            "code_index_freshness": {
                                "status": "current",
                                "worktree": worktree,
                            },
                            "graph_statistics": {
                                "state": "observed",
                                "generation_id": "generation.ready",
                                "symbol_count": 12,
                                "edge_count": 9,
                            },
                        },
                        strict=True,
                    )

    def test_strict_status_rejects_live_exact_scope_graph_degradation(self) -> None:
        with self.assertRaisesRegex(ValueError, "exact_scope_generation_not_ready"):
            self.checker.validate_status(
                {
                    "code_index_freshness": {
                        "status": "current",
                        "worktree": {
                            "coverage": "complete",
                            "staleness_state": "fresh",
                            "latest_generation_id": "generation.text-only",
                        },
                    },
                    "graph_statistics": {
                        "state": "unavailable",
                        "reason": "exact_scope_generation_not_ready",
                    },
                },
                strict=True,
            )

    def test_structural_status_still_accepts_degraded_typed_output(self) -> None:
        self.checker.validate_status(
            {
                "graph_statistics": {
                    "state": "unavailable",
                    "reason": "exact_scope_generation_not_ready",
                }
            }
        )

    def test_strict_context_accepts_lexical_and_graph_symbol_evidence(self) -> None:
        self.checker.validate_context(
            {
                "coverage": {
                    "exact": "complete",
                    "lexical": "complete",
                    "graph": "complete",
                    "semantic": {"status": "unavailable", "reason": "disabled"},
                    "recall": "partial",
                },
                "search_matches": [{"file": "src/main.rs"}],
                "symbols": [{"node_id": "symbol:main"}],
            },
            strict=True,
        )

    def test_strict_context_rejects_lexical_only_evidence(self) -> None:
        with self.assertRaisesRegex(ValueError, "graph symbol evidence"):
            self.checker.validate_context(
                {
                    "coverage": {
                        "exact": "complete",
                        "lexical": "complete",
                        "graph": {
                            "status": "unavailable",
                            "reason": "verified_code_graph_not_ready",
                        },
                        "semantic": {"status": "unavailable", "reason": "warming"},
                        "recall": "partial",
                    },
                    "search_matches": [{"file": "src/main.rs"}],
                    "symbols": [],
                    "verified_graph_evidence": {
                        "status": "unavailable",
                        "reason_code": "verified-code-graph-read-unavailable",
                        "retryable": True,
                    },
                },
                strict=True,
            )


if __name__ == "__main__":
    unittest.main()
