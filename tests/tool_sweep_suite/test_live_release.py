"""One small, isolated release-binary journey alongside the fake-client contracts."""

from __future__ import annotations

import os
from pathlib import Path
import tempfile
import unittest

import test_orchestrator as orchestration_tests


class ReleaseBinaryJourneyTests(unittest.TestCase):
    def test_release_binary_discovers_and_rolls_back_a_fact(self) -> None:
        """CI proves discovery and one real mutation outside the fake-client test doubles."""
        if os.environ.get("TRACEDECAY_SWEEP_LIVE") != "1":
            self.skipTest("set TRACEDECAY_SWEEP_LIVE=1 after building the release binary")
        binary_value = os.environ.get("TRACEDECAY_SWEEP_RELEASE_BIN")
        self.assertIsNotNone(binary_value, "live sweep requires TRACEDECAY_SWEEP_RELEASE_BIN")
        binary = Path(binary_value or "")
        self.assertTrue(binary.is_file() and os.access(binary, os.X_OK), "live sweep binary is not executable")
        orchestrator = orchestration_tests.load_orchestrator()
        repo = Path(__file__).parents[2]
        with tempfile.TemporaryDirectory(prefix="tracedecay-release-sweep-") as raw:
            out = Path(raw) / "out"
            deadline = orchestrator.WholeRunDeadline(300_000)
            discovery = orchestrator.run_phase(
                repo=repo, binary=binary, out=out, deadline=deadline, label="discovery", phase="discovery"
            )
            self.assertEqual(discovery.outcome.returncode, 0, (discovery.root / "stderr.log").read_text())
            manifest = orchestrator.load_manifest(discovery.root / "catalog.json")
            self.assertIn("tracedecay_fact_store", orchestrator.effect_targets(manifest))
            effect = orchestrator.run_phase(
                repo=repo,
                binary=binary,
                out=out,
                deadline=deadline,
                label="effects/fact-store",
                phase="effect",
                effect="tracedecay_fact_store",
                catalog=discovery.root / "catalog.json",
            )
            self.assertEqual(effect.outcome.returncode, 0, (effect.root / "stderr.log").read_text())
            report = orchestrator.load_report(effect.root / "results.json")
        self.assertEqual(len(report["entries"]), 1)
        row = report["entries"][0]
        self.assertEqual(row["name"], "tracedecay_fact_store")
        self.assertEqual(row["verdict"], "PASS")
        self.assertEqual(row.get("rollback"), "verified")


if __name__ == "__main__":
    unittest.main()
