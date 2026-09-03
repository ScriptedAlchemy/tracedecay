#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from types import ModuleType


SCORECARD_PATH = Path(__file__).with_name("efficiency-scorecard.py")


def load_scorecard() -> ModuleType:
    spec = importlib.util.spec_from_file_location("efficiency_scorecard", SCORECARD_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load efficiency scorecard from {SCORECARD_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ScorecardVerdictTests(unittest.TestCase):
    def test_invalid_sealed_generation_census_fails_run_and_scorecard(self) -> None:
        scorecard = load_scorecard()
        report = {
            "runs": [
                {
                    "status": "ok",
                    "cold_index": {
                        "graph_statistics": {
                            "state": "unavailable",
                            "reason": "sealed_generation_census_invalid",
                        }
                    },
                }
            ]
        }

        exit_code = scorecard.finalize_scorecard(report)

        self.assertEqual(report["runs"][0]["status"], "failed")
        self.assertEqual(
            report["runs"][0]["failure"],
            {
                "phase": "cold_index",
                "reason": (
                    "graph statistics were not observed: "
                    "sealed_generation_census_invalid"
                ),
            },
        )
        self.assertEqual(report["verdict"], "failed")
        self.assertEqual(exit_code, 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
