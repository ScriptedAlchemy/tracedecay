"""Falsifiable held-out qualification tests; no synthetic model is scored as Kev."""
import copy
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import qualification
import run_http
import score
from common import CATS, noul_questions


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.questions = noul_questions()
        self.dataset = [
            {"id": "tune-1", "labels": ["impact"], "context_needed": True,
             "current_prompt": "What breaks?", "turns": []},
            {"id": "tune-2", "labels": [], "context_needed": False,
             "current_prompt": "Hello", "turns": []},
            {"id": "holdout-positive", "labels": ["impact"], "context_needed": True,
             "current_prompt": "What changes?", "turns": []},
            {"id": "holdout-negative", "labels": [], "context_needed": False,
             "current_prompt": "Show a greeting", "turns": []},
        ]
        self.dataset_bytes = json.dumps(self.dataset).encode("utf-8")
        self.dataset_sha = qualification.sha256_bytes(self.dataset_bytes)
        annotations = []
        for case in self.dataset:
            holdout = case["id"].startswith("holdout")
            annotations.append({
                "id": case["id"], "split": "holdout" if holdout else "tune",
                "project_id": "project-b" if holdout else "project-a",
                "session_id": "session-b" if holdout else "session-a",
                "author": "case-creator", "reviewer": "separate-reviewer",
                "review_ref": "https://review.example/evidence/" + case["id"],
                "verified_labels": case["labels"],
                "verified_context_needed": case["context_needed"],
            })
        self.manifest = {
            "version": 1, "application": "historical-intent-evaluation",
            "dataset_sha256": self.dataset_sha,
            "questions_sha256": qualification.sha256_json(self.questions),
            "thresholds": {key: 0.6 for key in self.questions},
            "error_tolerances": {
                "max_none_false_hint_rate": 0.0,
                "min_positive_case_recall": 1.0,
                "max_p95_ms": 20.0,
            },
            "annotations": annotations,
        }
        self.manifest_bytes = json.dumps(self.manifest).encode("utf-8")
        self.manifest_sha = qualification.sha256_bytes(self.manifest_bytes)

    def valid(self, manifest=None, cases=None):
        return qualification.validate_manifest(
            self.manifest if manifest is None else manifest,
            self.dataset_sha, self.questions,
            self.dataset if cases is None else cases,
        )

    def test_positive_and_negative_holdout_is_separate_from_tuning(self):
        verified = self.valid()
        self.assertEqual(verified["heldout_ids"], ["holdout-positive", "holdout-negative"])
        self.assertEqual(verified["thresholds"]["impact"], 0.6)
        self.assertEqual(len(verified["annotations"]), 4)

    def test_near_neighbor_session_or_project_leak_refuses_entire_split(self):
        for key, value in (("project_id", "project-a"), ("session_id", "session-a")):
            with self.subTest(key=key):
                changed = copy.deepcopy(self.manifest)
                changed["annotations"][2][key] = value
                if key == "session_id":
                    changed["annotations"][2]["project_id"] = "project-a"
                with self.assertRaisesRegex(ValueError, "leaks project/session"):
                    self.valid(changed)

    def test_reviewed_labels_are_not_inferred_from_prediction_or_actions(self):
        for name, mutate, refusal in (
            ("self_label", lambda m: m["annotations"][2].update(reviewer="case-creator"), "self-labelled"),
            ("missing_review", lambda m: m["annotations"][2].update(review_ref=""), "review_ref"),
            ("ambiguous_label", lambda m: m["annotations"][2].update(verified_labels=["impact", "impact"]), "invalid verified labels"),
            ("missing_negative", lambda m: m["annotations"][3].update(split="tune"), "positive/negative"),
            ("unreviewed_question", lambda m: m["annotations"][3].update(verified_labels=["unknown"]), "invalid verified labels"),
        ):
            with self.subTest(name=name):
                manifest = copy.deepcopy(self.manifest)
                mutate(manifest)
                with self.assertRaisesRegex(ValueError, refusal):
                    self.valid(manifest)
        changed = copy.deepcopy(self.dataset)
        changed[2]["labels"] = []
        with self.assertRaisesRegex(ValueError, "disagrees with the dataset"):
            self.valid(cases=changed)

    def test_question_or_threshold_drift_fails_before_inference(self):
        for name, mutate, refusal in (
            ("question", lambda m: m.update(questions_sha256="f" * 64), "question vocabulary"),
            ("threshold", lambda m: m["thresholds"].pop("impact"), "thresholds"),
            ("nonfinite", lambda m: m["thresholds"].update(impact=float("nan")), "finite"),
            ("unfrozen_tolerance", lambda m: m["error_tolerances"].pop("max_p95_ms"), "tolerances"),
            ("dataset", lambda m: m.update(dataset_sha256="f" * 64), "dataset digest mismatch"),
        ):
            with self.subTest(name=name):
                changed = copy.deepcopy(self.manifest)
                mutate(changed)
                with self.assertRaisesRegex(ValueError, refusal):
                    self.valid(changed)

    def signed_result(self):
        rows = []
        for case in self.dataset[2:]:
            probs = {cat: (0.9 if cat in case["labels"] else 0.1) for cat in CATS}
            rows.append({
                "id": case["id"], "labels": case["labels"],
                "context_needed": case["context_needed"],
                "probs": probs,
                "ms": 10 if case["labels"] else 20,
                "request_sha256": hashlib.sha256(case["id"].encode()).hexdigest(),
            })
        experiment = {
            "version": 2,
            "config": "B_noul_text",
            "case_ids": [row["id"] for row in rows],
            "dataset_sha256": self.dataset_sha,
            "case_annotations_sha256": score.evidence_digest([
                [r["id"], r["labels"], r["context_needed"]] for r in rows
            ]),
            "requests_sha256": score.evidence_digest([
                [r["id"], r["request_sha256"]] for r in rows
            ]),
            "noul_temperature": None,
            "qualification_manifest_sha256": self.manifest_sha,
        }
        return {
            "config": "trial_B_noul_text",
            "dataset_sha256": self.dataset_sha,
            "experiment": experiment,
            "experiment_sha256": score.evidence_digest(experiment),
            "rows": rows,
        }

    def test_only_frozen_thresholds_qualify_and_latency_reports_p95(self):
        result = self.signed_result()
        report = qualification.qualified_report(
            result, self.manifest, self.manifest_sha, self.questions
        )
        self.assertTrue(report["qualified_against_frozen_tolerances"])
        self.assertEqual(report["cases"], 2)
        self.assertEqual(report["latency"]["p50_ms"], 15)
        self.assertEqual(report["latency"]["p95_ms"], 20)
        self.assertEqual(report["fixed_threshold_metrics"]["none_false_hint_rate"], 0)
        self.assertEqual(report["fixed_threshold_metrics"]["pos_cases_with_a_correct_hint"], 1)

        stricter = copy.deepcopy(self.manifest)
        stricter["error_tolerances"]["max_p95_ms"] = 19
        # An altered post-run manifest is not a valid requalification.
        with self.assertRaisesRegex(ValueError, "not bound"):
            qualification.qualified_report(result, stricter, qualification.sha256_json(stricter), self.questions)

    def test_qualified_reporting_refuses_tampered_labels_and_unbound_results(self):
        base = self.signed_result()
        for field, mutation, pattern in (
            ("label", lambda r: r["rows"][0].update(labels=[]), "reviewed labels"),
            ("order", lambda r: r["rows"].reverse(), "held-out case order"),
            ("identity", lambda r: r["experiment"].pop("qualification_manifest_sha256"), "not bound"),
            ("forged_review", lambda r: r["rows"][1].update(context_needed=True), "reviewed labels"),
        ):
            with self.subTest(field=field):
                result = copy.deepcopy(base)
                mutation(result)
                with self.assertRaisesRegex(ValueError, pattern):
                    qualification.qualified_report(result, self.manifest, self.manifest_sha, self.questions)

    def test_http_runner_asks_only_pre_frozen_holdout_and_resumes_with_exact_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps({
                "checkpoint": "example-pinned-checkpoint",
                "server_revision": "example-pinned-server",
                "backend": "example-backend", "dtype": "float32", "temperature": 2.41,
            }))
            calls = []

            def answer(url, body):
                calls.append(body)
                return {"answers": {
                    key: {"type": "noul", "noul": 0.5}
                    for key in body["questions"]
                }}, 12.0

            args = [
                "--url", "http://127.0.0.1:8009/v1/systemone",
                "--tag", "frozen",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            with patch.object(run_http, "HERE", str(here)), patch.object(run_http, "post", side_effect=answer):
                run_http.main(args)
            self.assertEqual(len(calls), 2, "must exclude training/tune cases from inference")
            result = json.loads((here / "results" / "frozen_B_noul_text.json").read_text())
            self.assertEqual([r["id"] for r in result["rows"]], [
                "holdout-positive", "holdout-negative"
            ])
            self.assertEqual(result["experiment"]["qualification_manifest_sha256"], self.manifest_sha)
            self.assertEqual(result["experiment"]["case_ids"], [
                "holdout-positive", "holdout-negative"
            ])

    def test_runner_refuses_group_leakage_without_network_or_partial_output(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            changed = copy.deepcopy(self.manifest)
            changed["annotations"][2]["project_id"] = "project-a"
            (here / "manifest.json").write_text(json.dumps(changed))
            (here / "server.json").write_text(json.dumps({
                "checkpoint": "pinned", "server_revision": "r1", "backend": "cpu",
                "dtype": "float32", "temperature": 2.41,
            }))
            args = [
                "--url", "http://127.0.0.1:8009/v1/systemone",
                "--tag", "refused",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            with patch.object(run_http, "HERE", str(here)), patch.object(run_http, "post") as post:
                with self.assertRaisesRegex(ValueError, "leaks project/session"):
                    run_http.main(args)
                post.assert_not_called()
            self.assertFalse((here / "results").exists())


if __name__ == "__main__":
    unittest.main()
