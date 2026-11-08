"""Exercise qualification exit status through the real scoring entry point.

The shared cases are evaluator fixtures, not model-qualification evidence.
"""
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import test_qualification as fixtures


class QualificationCliTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.QualificationTests()
        self.fixture.setUp()
        staging = tempfile.TemporaryDirectory()
        self.addCleanup(staging.cleanup)
        self.root = Path(staging.name)
        source = Path(__file__).resolve().parent
        for name in ("common.py", "qualification.py", "run_http.py", "score.py"):
            shutil.copyfile(source / name, self.root / name)
        (self.root / "manifest.json").write_bytes(self.fixture.manifest_bytes)
        (self.root / "dataset.json").write_bytes(self.fixture.dataset_bytes)

    def run_score(self, result):
        (self.root / "result.json").write_text(
            json.dumps(result, allow_nan=False), encoding="utf-8"
        )
        return subprocess.run(
            [sys.executable, str(self.root / "score.py"), "--qualified",
             str(self.root / "manifest.json"), str(self.root / "dataset.json"),
             str(self.root / "result.json")],
            cwd=self.root, capture_output=True, text=True, timeout=30,
        )

    def published_report(self, process):
        report = json.loads(process.stdout)
        published = json.loads(
            (self.root / "qualification_summary.json").read_text(encoding="utf-8")
        )
        self.assertEqual(published, report)
        self.assertEqual(report["cases"], 2)
        self.assertFalse((self.root / "summary.json").exists())
        return report

    def test_passing_qualification_exits_zero_with_matching_report(self):
        process = self.run_score(self.fixture.signed_result())
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stderr, "")
        report = self.published_report(process)
        self.assertTrue(report["qualified_against_frozen_tolerances"])
        self.assertTrue(all(report["checks"].values()))

    def test_failed_checks_exit_nonzero_and_replace_a_previous_passing_report(self):
        failures = (
            ("negative_false_hint", "none_false_hint_rate",
             lambda r: r["rows"][1]["probs"].update(impact=0.9)),
            ("extra_positive_hint", "unwanted_hint_rate",
             lambda r: r["rows"][0]["probs"].update(call_graph=0.9)),
            ("missed_positive", "positive_case_recall",
             lambda r: r["rows"][0]["probs"].update(impact=0.1)),
            ("slow_response", "p95_ms", lambda r: r["rows"][1].update(ms=21)),
            ("unknown_truncation", "untruncated_input_evidence",
             lambda r: r["rows"][0].update(dropped=None)),
            ("truncated_input", "untruncated_input_evidence",
             lambda r: r["rows"][0].update(dropped=1)),
        )
        for name, failed_check, mutate in failures:
            with self.subTest(name=name):
                passing = self.run_score(self.fixture.signed_result())
                self.assertEqual(passing.returncode, 0, passing.stderr)
                self.assertTrue(
                    self.published_report(passing)["qualified_against_frozen_tolerances"]
                )
                result = self.fixture.signed_result()
                mutate(result)
                process = self.run_score(result)
                self.assertEqual(process.returncode, 1, process.stderr)
                report = self.published_report(process)
                self.assertFalse(report["qualified_against_frozen_tolerances"])
                self.assertFalse(report["checks"][failed_check])
                self.assertIn("qualification failed frozen checks:", process.stderr)
                self.assertIn(failed_check, process.stderr)

    def test_invalid_dataset_fails_without_publishing_a_qualification(self):
        (self.root / "dataset.json").write_bytes(self.fixture.dataset_bytes + b"\n")
        process = self.run_score(self.fixture.signed_result())
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("dataset digest differs from supplied evidence", process.stderr)
        self.assertEqual(process.stdout, "")
        self.assertFalse((self.root / "qualification_summary.json").exists())

    def test_unqualified_mode_reports_the_complete_required_command(self):
        results = self.root / "results"
        results.mkdir()
        (results / "heldout.json").write_text(
            json.dumps(self.fixture.signed_result(), allow_nan=False), encoding="utf-8"
        )
        process = subprocess.run(
            [sys.executable, str(self.root / "score.py")],
            cwd=self.root, capture_output=True, text=True, timeout=30,
        )
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("--qualified MANIFEST DATASET RESULT", process.stderr)
        self.assertFalse((self.root / "summary.json").exists())
        self.assertFalse((self.root / "qualification_summary.json").exists())


if __name__ == "__main__":
    unittest.main()
