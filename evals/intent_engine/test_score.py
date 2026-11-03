"""Behavioral coverage for sparse evaluation slices and their CLI reports."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import score


def row(case_id, labels=(), context=False, choice=None):
    labels = list(labels)
    result = {
        "id": case_id,
        "labels": labels,
        "context_needed": context,
        "probs": {cat: 0.9 if cat in labels else 0.1 for cat in score.CATS},
        "ms": 100,
        "state_tokens": 20,
        "dropped": 0,
    }
    if choice is not None:
        result["choice"] = choice
    return result


class ScoringTests(unittest.TestCase):
    def test_empty_slice_metrics_are_unknown(self):
        metrics = score.case_metrics([], {cat: 0.5 for cat in score.CATS})
        self.assertEqual(metrics["n"], 0)
        self.assertEqual(metrics["none_n"], 0)
        for key in (
            "exact_set_acc", "top1_acc", "pos_cases_with_a_correct_hint",
            "none_false_hint_rate",
        ):
            self.assertIsNone(metrics[key], key)
        self.assertEqual(score.reliability([]), {"buckets": [], "ece": None})

    def test_sparse_auc_reports_coverage_and_absent_context(self):
        result = score.score_config({
            "config": "sparse",
            "rows": [row("positive", [score.CATS[0]]), row("negative")],
        })
        self.assertEqual(result["macro_auc"], 1.0)
        self.assertEqual(result["macro_auc_n_categories"], 1)
        self.assertIsNone(result["macro_auc_ctx"])
        self.assertEqual(result["macro_auc_ctx_n_categories"], 0)
        self.assertIsNone(result["t0.5"]["macro_f1_ctx"])
        self.assertIsNone(result["t0.5"]["ctx"]["top1_acc"])
        self.assertEqual(result["t0.5"]["no_ctx"]["n"], 2)
        self.assertEqual(result["t0.5"]["all"]["exact_set_acc"], 1.0)
        self.assertEqual(json.loads(json.dumps(result, allow_nan=False)), result)

    def test_all_negative_cases_have_no_auc_but_measured_false_hint_rate(self):
        result = score.score_config({
            "config": "negative",
            "rows": [row("a", choice="none"), row("b", context=True, choice="none")],
        })
        self.assertIsNone(result["macro_auc"])
        self.assertIsNone(result["macro_auc_ctx"])
        self.assertEqual(result["macro_auc_n_categories"], 0)
        self.assertEqual(result["macro_auc_ctx_n_categories"], 0)
        self.assertEqual(result["choice_none_false_hint_rate"], 0.0)
        self.assertEqual(result["choice_top1_acc"], {"all": 1.0, "ctx": 1.0, "no_ctx": 1.0})

    def test_choice_without_negative_or_noncontext_cases(self):
        result = score.score_config({
            "config": "positive",
            "rows": [row(case, score.CATS, True, score.CATS[0]) for case in ("a", "b")],
        })
        self.assertIsNone(result["macro_auc"])
        self.assertEqual(result["macro_auc_n_categories"], 0)
        self.assertIsNone(result["choice_none_false_hint_rate"])
        self.assertIsNone(result["choice_top1_acc"]["no_ctx"])
        self.assertEqual(result["choice_top1_acc"]["ctx"], 1.0)
        self.assertEqual(result["t0.5"]["no_ctx"]["n"], 0)

    def test_singleton_has_no_leave_one_out_training_evidence(self):
        result = score.score_config({"config": "one", "rows": [row("a", [score.CATS[0]])]})
        self.assertEqual(result["loo_evaluated_cases"], 0)
        self.assertIsNone(result["macro_loo_f1"])
        for category in score.CATS:
            for metric in ("loo_f1", "loo_p", "loo_r"):
                self.assertIsNone(result["per_cat"][category][metric])
        self.assertEqual(result["t0.5"]["all"]["top1_acc"], 1.0)

    def test_empty_results_and_partial_choice_coverage_are_refused(self):
        with self.assertRaisesRegex(ValueError, "empty.*no rows to score"):
            score.score_config({"config": "empty", "rows": []})
        for rows in (
            [row("a", choice="none"), row("b")],
            [row("a"), row("b", choice="none")],
        ):
            with self.assertRaisesRegex(ValueError, "incomplete choice coverage"):
                score.score_config({"config": "mixed", "rows": rows})

    def test_full_class_coverage_preserves_existing_scores(self):
        result = score.score_config({
            "config": "full",
            "rows": [row("positive", score.CATS, True), row("negative")],
        }, {cat: 0.6 for cat in score.CATS})
        self.assertEqual(result["macro_auc"], 1.0)
        self.assertEqual(result["macro_auc_n_categories"], len(score.CATS))
        self.assertEqual(result["macro_loo_f1"], 1.0)
        self.assertEqual(result["loo_evaluated_cases"], 2)
        for name in ("t0.5", "t0.6", "t0.7", "best_t_in_sample", "fixed"):
            self.assertEqual(result[name]["macro_f1"], 1.0)
            self.assertEqual(result[name]["all"]["exact_set_acc"], 1.0)

    def test_cli_reports_the_scored_subset_without_loading_the_full_dataset(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            (root / "results" / "sample.json").write_text(json.dumps({
                "config": "sample",
                "rows": [
                    row("a", [score.CATS[0]], True, score.CATS[0]),
                    row("b", context=True, choice="none"),
                ],
            }), encoding="utf-8")
            scored = self.run_script(root, "score.py")
            self.assertEqual(scored.returncode, 0, scored.stderr)
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            self.assertIsNone(summary["sample"]["choice_top1_acc"]["no_ctx"])
            self.assertIn(
                f"AUC categories 1/{len(score.CATS)} ctx 1/{len(score.CATS)}", scored.stdout,
            )
            compared = self.run_script(root, "compare.py", "sample")
            self.assertEqual(compared.returncode, 0, compared.stderr)
            self.assertIn("sample: n=2 none=1 ctx=2", compared.stdout)
            self.assertIn(f"| {score.CATS[0]} | n=1;", compared.stdout)
            self.assertIn("AUC eligible categories", compared.stdout)
            self.assertFalse((root / "dataset.json").exists())

    def test_empty_cli_result_does_not_overwrite_an_existing_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            (root / "results" / "empty.json").write_text(
                json.dumps({"config": "empty", "rows": []}), encoding="utf-8",
            )
            summary = root / "summary.json"
            summary.write_bytes(b"existing summary\n")
            result = self.run_script(root, "score.py")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no rows to score", result.stderr)
            self.assertEqual(summary.read_bytes(), b"existing summary\n")

    @staticmethod
    def stage_scripts(root):
        for name in ("score.py", "compare.py", "common.py"):
            (root / name).write_bytes(Path(score.__file__).with_name(name).read_bytes())

    @staticmethod
    def run_script(root, name, *args):
        return subprocess.run(
            [sys.executable, str(root / name), *args],
            cwd=root, capture_output=True, text=True, timeout=30, check=False,
        )


if __name__ == "__main__":
    unittest.main()
