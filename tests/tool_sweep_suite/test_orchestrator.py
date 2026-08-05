"""Behavioral tests for whole-run catalog sweep orchestration."""

from __future__ import annotations

import importlib.util
from types import SimpleNamespace
import sys
from pathlib import Path
import tempfile
import time
import unittest


SUITE_DIR = Path(__file__).parent
ORCHESTRATOR = SUITE_DIR / "orchestrator.py"


def load_orchestrator():
    spec = importlib.util.spec_from_file_location("tool_sweep_orchestrator", ORCHESTRATOR)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WholeRunDeadlineTests(unittest.TestCase):
    def test_expiring_process_is_cancelled_before_the_final_report(self) -> None:
        """The process group is stopped at the shared deadline, not left to CI's timeout."""
        orchestrator = load_orchestrator()
        with tempfile.TemporaryDirectory() as raw:
            outcome = orchestrator.run_bounded_command(
                [sys.executable, "-c", "import time; time.sleep(5)"],
                cwd=Path(raw),
                environment=dict(),
                remaining_s=0.01,
            )

        self.assertTrue(outcome.cancelled)
        self.assertEqual(outcome.reason, "whole_run_deadline_exceeded")

    def test_run_emits_json_and_junit_when_its_first_phase_is_cancelled(self) -> None:
        """The real orchestration finally path survives cancellation before discovery."""
        orchestrator = load_orchestrator()
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw) / "artifacts"
            status = orchestrator.run(
                SimpleNamespace(
                    repo=SUITE_DIR.parents[1],
                    bin=Path(sys.executable),
                    out=out,
                    whole_run_deadline_ms=1,
                )
            )

            report = orchestrator.load_report(out / "results.json")
            junit = (out / "junit.xml").read_text()

        self.assertEqual(status, 1)
        self.assertIn("fatal", report)
        self.assertIn("mcp-catalog-sweep", junit)

    def test_deadline_cancels_phase_and_emits_an_aggregate_artifact(self) -> None:
        """A stalled isolated phase must never leave CI without a final report."""
        orchestrator = load_orchestrator()
        manifest = {
            "tools": [{"name": "tracedecay_read"}],
            "resources": [{"uri": "tracedecay://health"}],
            "prompts": [{"name": "triage"}],
        }
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw)
            report = orchestrator.cancelled_report(manifest, "whole_run_deadline_exceeded")
            orchestrator.write_final_report(out, report)

            loaded = orchestrator.load_report(out / "results.json")

        self.assertEqual(loaded["summary"]["cancelled"], 3)
        self.assertEqual(
            {row["problem_code"] for row in loaded["entries"]},
            {"tool_sweep.whole_run_deadline_exceeded"},
        )


if __name__ == "__main__":
    unittest.main()
