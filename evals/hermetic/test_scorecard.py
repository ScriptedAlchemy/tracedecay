#!/usr/bin/env python3
"""Deterministic offline unit tests for the fact-store adoption scorecard.

No live model, no filesystem beyond a couple of temp files, no network. Feeds a
synthetic in-memory results list through ``scorecard.aggregate`` and asserts the
computed adoption percentages per bucket, overall, and the feedback headline.

Run:  python3 evals/hermetic/test_scorecard.py
"""

from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import scorecard  # noqa: E402


def row(scenario_id: str, category: str, passed: bool, **extra) -> dict:
    """Build a synthetic scored row shaped like the ones score.py emits."""
    r = {
        "id": scenario_id,
        "category": category,
        "rep": 1,
        "tracedecay_tool_uses": 1 if passed else 0,
        "native_tool_uses": 0,
        "cli_command_uses": 0,
        "expected_tools_missing": [] if passed else ["tracedecay_fact_store_add"],
        "anti_tools_used": [],
        "verify_pass": None,
        "pass": passed,
    }
    r.update(extra)
    return r


# Synthetic fixture: 2 write pass / 1 write fail, 1 recall pass, 2 feedback fail.
SYNTHETIC_RESULTS = [
    row("w1", "factstore-write", True),
    row("w2", "factstore-write", True),
    row("w3", "factstore-write", False),
    row("r1", "factstore-recall", True),
    row("f1", "factstore-feedback", False),
    row("f2", "factstore-feedback", False),
    # A non-factstore row must be ignored entirely.
    row("c1", "context", True),
]


class AggregateTest(unittest.TestCase):
    def test_buckets_without_corpus(self):
        s = scorecard.aggregate(SYNTHETIC_RESULTS)

        # write: 3 opportunities, 2 triggered -> 66.67%
        w = s["buckets"]["write"]
        self.assertEqual(w["opportunities"], 3)
        self.assertEqual(w["triggered"], 2)
        self.assertAlmostEqual(w["adoption_pct"], 66.67, places=2)

        # recall: 1 opportunity, 1 triggered -> 100%
        rc = s["buckets"]["recall"]
        self.assertEqual(rc["opportunities"], 1)
        self.assertEqual(rc["triggered"], 1)
        self.assertAlmostEqual(rc["adoption_pct"], 100.0, places=2)

        # feedback: 2 opportunities, 0 triggered -> 0%
        fb = s["buckets"]["feedback"]
        self.assertEqual(fb["opportunities"], 2)
        self.assertEqual(fb["triggered"], 0)
        self.assertAlmostEqual(fb["adoption_pct"], 0.0, places=2)

    def test_overall_without_corpus(self):
        s = scorecard.aggregate(SYNTHETIC_RESULTS)
        # 3 + 1 + 2 = 6 opportunities, 2 + 1 + 0 = 3 triggered -> 50%
        self.assertEqual(s["overall"]["opportunities"], 6)
        self.assertEqual(s["overall"]["triggered"], 3)
        self.assertAlmostEqual(s["overall"]["adoption_pct"], 50.0, places=2)
        self.assertAlmostEqual(s["factstore_adoption_pct"], 50.0, places=2)

    def test_feedback_headline(self):
        s = scorecard.aggregate(SYNTHETIC_RESULTS)
        self.assertAlmostEqual(s["feedback_adoption_pct"], 0.0, places=2)

    def test_non_factstore_ignored(self):
        s = scorecard.aggregate(SYNTHETIC_RESULTS)
        self.assertNotIn("context", s["buckets"])
        # Overall opportunities exclude the context row.
        self.assertEqual(s["overall"]["opportunities"], 6)

    def test_passed_alias(self):
        # score.py emits "pass"; ensure "passed" is tolerated as a fallback.
        rows = [
            {"id": "w1", "category": "factstore-write", "passed": True},
            {"id": "w2", "category": "factstore-write", "passed": False},
        ]
        s = scorecard.aggregate(rows)
        self.assertEqual(s["buckets"]["write"]["opportunities"], 2)
        self.assertEqual(s["buckets"]["write"]["triggered"], 1)
        self.assertAlmostEqual(s["buckets"]["write"]["adoption_pct"], 50.0, places=2)


class CorpusDenominatorTest(unittest.TestCase):
    def test_corpus_counts_missing_scenarios(self):
        # Corpus has 4 write opportunities, but only 3 write rows exist in
        # results (one scenario errored / was skipped). Denominator = 4.
        corpus = [
            {"id": "w1", "category": "factstore-write"},
            {"id": "w2", "category": "factstore-write"},
            {"id": "w3", "category": "factstore-write"},
            {"id": "w4", "category": "factstore-write"},
            {"id": "r1", "category": "factstore-recall"},
            {"id": "f1", "category": "factstore-feedback"},
            {"id": "f2", "category": "factstore-feedback"},
            {"id": "c1", "category": "context"},
        ]
        s = scorecard.aggregate(SYNTHETIC_RESULTS, corpus)

        w = s["buckets"]["write"]
        self.assertEqual(w["opportunities"], 4)   # from corpus, not the 3 rows
        self.assertEqual(w["triggered"], 2)       # still from real results
        self.assertAlmostEqual(w["adoption_pct"], 50.0, places=2)

        # Feedback opportunities from corpus (2), 0 triggered.
        fb = s["buckets"]["feedback"]
        self.assertEqual(fb["opportunities"], 2)
        self.assertEqual(fb["triggered"], 0)

        # Overall: 4 + 1 + 2 = 7 opportunities, 3 triggered.
        self.assertEqual(s["overall"]["opportunities"], 7)
        self.assertEqual(s["overall"]["triggered"], 3)
        self.assertEqual(s["denominator_source"], "corpus")


class EmptyAndMissingTest(unittest.TestCase):
    def test_empty_results(self):
        s = scorecard.aggregate([])
        self.assertEqual(s["overall"]["opportunities"], 0)
        self.assertEqual(s["overall"]["triggered"], 0)
        self.assertAlmostEqual(s["overall"]["adoption_pct"], 0.0, places=2)
        self.assertAlmostEqual(s["feedback_adoption_pct"], 0.0, places=2)

    def test_missing_file_graceful(self):
        rows = scorecard.read_jsonl(Path("/nonexistent/does/not/exist.jsonl"))
        self.assertEqual(rows, [])

    def test_read_jsonl_skips_garbage(self):
        with tempfile.NamedTemporaryFile(
            "w", suffix=".jsonl", delete=False
        ) as fh:
            fh.write('{"id":"a","category":"factstore-write","pass":true}\n')
            fh.write("\n")
            fh.write("not json at all\n")
            fh.write('{"id":"b","category":"factstore-recall","pass":false}\n')
            path = fh.name
        try:
            rows = scorecard.read_jsonl(Path(path))
        finally:
            os.unlink(path)
        self.assertEqual(len(rows), 2)


class RenderAndCliTest(unittest.TestCase):
    def test_table_and_json_render(self):
        s = scorecard.aggregate(SYNTHETIC_RESULTS)
        table = scorecard.render_table(s)
        self.assertIn("Fact-store adoption %", table)
        self.assertIn("Feedback-loop adoption %", table)
        self.assertIn("write", table)
        self.assertIn("feedback", table)

        block = scorecard.render_json_block(s)
        self.assertIn(scorecard.JSON_BEGIN, block)
        self.assertIn(scorecard.JSON_END, block)
        # The fenced JSON must be valid and round-trip the headline.
        inner = block.split(scorecard.JSON_BEGIN, 1)[1].split(scorecard.JSON_END, 1)[0]
        parsed = json.loads(inner)
        self.assertAlmostEqual(parsed["feedback_adoption_pct"], 0.0, places=2)
        self.assertAlmostEqual(parsed["factstore_adoption_pct"], 50.0, places=2)

    def test_main_exit_zero_on_missing(self):
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = scorecard.main(["/nonexistent/results.jsonl"])
        self.assertEqual(rc, 0)
        out = buf.getvalue()
        self.assertIn("Fact-store adoption %: 0.00%", out)

    def test_main_with_real_files(self):
        with tempfile.TemporaryDirectory() as d:
            results_path = Path(d) / "results.jsonl"
            results_path.write_text(
                "\n".join(json.dumps(r) for r in SYNTHETIC_RESULTS) + "\n"
            )
            buf = io.StringIO()
            with redirect_stdout(buf):
                rc = scorecard.main([str(results_path)])
            self.assertEqual(rc, 0)
            out = buf.getvalue()
            self.assertIn("Fact-store adoption %: 50.00%", out)
            self.assertIn("Feedback-loop adoption %: 0.00%", out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
