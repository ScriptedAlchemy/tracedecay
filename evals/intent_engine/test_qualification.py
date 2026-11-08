"""Falsifiable held-out qualification tests; no synthetic model is scored as Kev."""
import copy
import hashlib
import json
import shutil
import subprocess
import sys
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
        for case in self.dataset:
            holdout = case["id"].startswith("holdout")
            case["project_id"] = "project-b" if holdout else "project-a"
            case["session_id"] = "session-b" if holdout else "session-a"
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
                "max_unwanted_hint_rate": 0.0,
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

    def test_group_identity_cannot_be_invented_by_the_annotation_manifest(self):
        for field in ("project_id", "session_id"):
            with self.subTest(field=field):
                forged = copy.deepcopy(self.manifest)
                forged["annotations"][2][field] = "invented-" + field
                with self.assertRaisesRegex(ValueError, "group identity disagrees with frozen dataset"):
                    self.valid(forged)
                missing_source = copy.deepcopy(self.dataset)
                del missing_source[2][field]
                with self.assertRaisesRegex(ValueError, "group identity disagrees with frozen dataset"):
                    self.valid(cases=missing_source)

    def test_self_consistent_rehashed_metadata_cannot_hide_group_forgery(self):
        forged = copy.deepcopy(self.manifest)
        forged["annotations"][2]["project_id"] = "invented-independent-project"
        forged_sha = qualification.sha256_json(forged)
        result = self.signed_result()
        result["experiment"]["qualification_manifest_sha256"] = forged_sha
        result["experiment_sha256"] = score.evidence_digest(result["experiment"])
        with self.assertRaisesRegex(ValueError, "group identity disagrees with frozen dataset"):
            qualification.qualified_report(
                result, forged, forged_sha, self.questions, self.dataset_bytes
            )

    def test_near_neighbor_session_or_project_leak_refuses_entire_split(self):
        for key, value in (("project_id", "project-a"), ("session_id", "session-a")):
            with self.subTest(key=key):
                changed = copy.deepcopy(self.manifest)
                changed["annotations"][2][key] = value
                if key == "session_id":
                    changed["annotations"][2]["project_id"] = "project-a"
                with self.assertRaisesRegex(ValueError, "leaks project/session"):
                    self.valid(changed)

    def test_reused_session_id_under_different_project_is_not_independent(self):
        changed = copy.deepcopy(self.manifest)
        changed["annotations"][2]["session_id"] = "session-a"
        # Changing the project label must not conceal a shared session.
        self.assertNotEqual(changed["annotations"][2]["project_id"], "project-a")
        with self.assertRaisesRegex(ValueError, "leaks project/session"):
            self.valid(changed)

    def test_unwanted_hints_on_positive_cases_are_not_hidden_by_recall(self):
        result = self.signed_result()
        result["rows"][0]["probs"]["call_graph"] = 0.99
        report = qualification.qualified_report(
            result, self.manifest, self.manifest_sha, self.questions, self.dataset_bytes
        )
        self.assertEqual(report["fixed_threshold_metrics"]["none_false_hint_rate"], 0)
        self.assertEqual(report["fixed_threshold_metrics"]["pos_cases_with_a_correct_hint"], 1)
        self.assertEqual(report["holdout_decisions"]["unwanted_hint_cases"], 1)
        self.assertEqual(report["holdout_decisions"]["unwanted_hint_rate"], 0.5)
        self.assertEqual(report["holdout_decisions"]["abstention_rate"], 0.5)
        self.assertFalse(report["checks"]["unwanted_hint_rate"])
        self.assertFalse(report["qualified_against_frozen_tolerances"])

    def test_missing_or_dropped_server_tokens_cannot_qualify(self):
        for value in (None, 1):
            with self.subTest(state_tokens_dropped=value):
                result = self.signed_result()
                result["rows"][0]["dropped"] = value
                report = qualification.qualified_report(
                    result, self.manifest, self.manifest_sha, self.questions, self.dataset_bytes
                )
                self.assertFalse(report["checks"]["untruncated_input_evidence"])
                self.assertFalse(report["qualified_against_frozen_tolerances"])
                self.assertEqual(
                    report["latency"]["truncation_measured_cases"],
                    1 if value is None else 2,
                )
        complete = qualification.qualified_report(
            self.signed_result(), self.manifest, self.manifest_sha,
            self.questions, self.dataset_bytes,
        )
        self.assertTrue(complete["checks"]["untruncated_input_evidence"])
        self.assertTrue(complete["qualified_against_frozen_tolerances"])

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
            ("unfrozen_unwanted", lambda m: m["error_tolerances"].pop("max_unwanted_hint_rate"), "tolerances"),
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
                "dropped": 0,
                "request_sha256": hashlib.sha256(run_http.request_bytes({
                    "state": run_http.STATES["B_noul_text"](case),
                    "questions": self.questions,
                })).hexdigest(),
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
            "server_config": {
                "checkpoint": "checkpoint-pinned-by-operator",
                "server_revision": "immutable-server-revision",
                "backend": "cpu", "dtype": "float32", "temperature": 2.41,
            },
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
            result, self.manifest, self.manifest_sha, self.questions, self.dataset_bytes
        )
        self.assertTrue(report["qualified_against_frozen_tolerances"])
        self.assertEqual(report["cases"], 2)
        self.assertEqual(report["latency"]["p50_ms"], 15)
        self.assertEqual(report["latency"]["p95_ms"], 20)
        self.assertEqual(report["holdout_decisions"]["coverage_rate"], 0.5)
        self.assertEqual(report["holdout_decisions"]["abstention_rate"], 0.5)
        self.assertEqual(report["holdout_decisions"]["unwanted_hint_rate"], 0)
        self.assertTrue(report["checks"]["untruncated_input_evidence"])
        self.assertEqual(report["fixed_threshold_metrics"]["none_false_hint_rate"], 0)
        self.assertEqual(report["fixed_threshold_metrics"]["pos_cases_with_a_correct_hint"], 1)

        stricter = copy.deepcopy(self.manifest)
        stricter["error_tolerances"]["max_p95_ms"] = 19
        # An altered post-run manifest is not a valid requalification.
        with self.assertRaisesRegex(ValueError, "not bound"):
            qualification.qualified_report(result, stricter, qualification.sha256_json(stricter), self.questions, self.dataset_bytes)


    def test_qualification_requires_complete_serving_identity(self):
        for field, change in (
            ("missing_server", lambda r: r["experiment"].pop("server_config")),
            ("missing_checkpoint", lambda r: r["experiment"]["server_config"].pop("checkpoint")),
            ("unknown_revision", lambda r: r["experiment"]["server_config"].update(server_revision="")),
            ("nan_temperature", lambda r: r["experiment"]["server_config"].update(temperature=float("nan"))),
        ):
            with self.subTest(field=field):
                result = self.signed_result()
                change(result)
                with self.assertRaisesRegex(ValueError, "serving"):
                    qualification.qualified_report(
                        result, self.manifest, self.manifest_sha, self.questions, self.dataset_bytes
                    )

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
                    qualification.qualified_report(result, self.manifest, self.manifest_sha, self.questions, self.dataset_bytes)

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


    def test_frozen_manifest_cannot_reorder_a_dataset(self):
        changed = copy.deepcopy(self.manifest)
        changed["annotations"][0], changed["annotations"][1] = (
            changed["annotations"][1], changed["annotations"][0]
        )
        with self.assertRaisesRegex(ValueError, "case ordering"):
            self.valid(changed)

    def test_changed_manifest_cannot_resume_or_spend_inference_tokens(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            manifest_path = here / "manifest.json"
            manifest_path.write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps({
                "checkpoint": "pinned", "server_revision": "r1", "backend": "cpu",
                "dtype": "float32", "temperature": 2.41,
            }))
            args = [
                "--url", "http://127.0.0.1:8009/v1/systemone",
                "--tag", "interrupted",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(manifest_path),
                "B_noul_text",
            ]
            answer = {"answers": {
                key: {"type": "noul", "noul": 0.5} for key in self.questions
            }}
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", side_effect=[
                    (answer, 11.0), RuntimeError("interrupted"),
                ]) as initial:
                    with self.assertRaisesRegex(RuntimeError, "interrupted"):
                        run_http.main(args)
                    self.assertEqual(initial.call_count, 2)
                partial = here / "results" / "interrupted_B_noul_text.json.partial"
                frozen_bytes = partial.read_bytes()
                self.assertEqual(len(frozen_bytes.splitlines()), 2,
                                 "first accepted prediction must be durably resumable")
                changed = copy.deepcopy(self.manifest)
                changed["error_tolerances"]["max_p95_ms"] = 21.0
                manifest_path.write_text(json.dumps(changed))
                with patch.object(run_http, "post") as no_inference:
                    with self.assertRaisesRegex(ValueError, "another or unrecorded experiment"):
                        run_http.main(args)
                    no_inference.assert_not_called()
                self.assertEqual(partial.read_bytes(), frozen_bytes)
                manifest_path.write_bytes(self.manifest_bytes)
                with patch.object(run_http, "post", return_value=(answer, 12.0)) as remaining:
                    run_http.main(args)
                    self.assertEqual(remaining.call_count, 1)
            completed = here / "results" / "interrupted_B_noul_text.json"
            self.assertEqual(len(json.loads(completed.read_text())["rows"]), 2)


    def test_qualified_cli_is_separate_and_historical_cli_refuses_heldout(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            for filename in ("score.py", "qualification.py", "common.py", "run_http.py"):
                shutil.copyfile(Path(score.__file__).with_name(filename), here / filename)
            (here / "results").mkdir()
            result_path = here / "results" / "qualified.json"
            result_path.write_text(json.dumps(self.signed_result()))
            manifest_path = here / "qualification.json"
            manifest_path.write_bytes(self.manifest_bytes)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            summary = here / "summary.json"
            summary.write_bytes(b"existing historical evidence\\n")

            default = subprocess.run(
                [sys.executable, str(here / "score.py")],
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(default.returncode, 0)
            self.assertIn("refuse in-sample scoring", default.stderr)
            self.assertEqual(summary.read_bytes(), b"existing historical evidence\\n")

            qualified = subprocess.run(
                [sys.executable, str(here / "score.py"), "--qualified",
                 str(manifest_path), str(here / "dataset.json"), str(result_path)],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(qualified.returncode, 0, qualified.stderr)
            report_path = here / "qualification_summary.json"
            report = json.loads(report_path.read_text())
            self.assertTrue(report["qualified_against_frozen_tolerances"])
            self.assertEqual(report["latency"]["p95_ms"], 20)
            self.assertEqual(summary.read_bytes(), b"existing historical evidence\\n")

            corrupted = self.signed_result()
            corrupted["rows"][0]["labels"] = []
            result_path.write_text(json.dumps(corrupted))
            previous_report = report_path.read_bytes()
            rejected = subprocess.run(
                [sys.executable, str(here / "score.py"), "--qualified",
                 str(manifest_path), str(here / "dataset.json"), str(result_path)],
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("reviewed labels", rejected.stderr)
            self.assertEqual(report_path.read_bytes(), previous_report)

    def test_qualified_requires_original_cases_and_reproducible_requests(self):
        result = self.signed_result()
        altered_cases = copy.deepcopy(self.dataset)
        altered_cases[2]["current_prompt"] = "A different prompt"
        with self.assertRaisesRegex(ValueError, "dataset digest differs"):
            qualification.qualified_report(
                result, self.manifest, self.manifest_sha, self.questions,
                json.dumps(altered_cases).encode("utf-8"),
            )
        # The stored hashes can be re-computed consistently around a forged
        # request fingerprint; reconstruction from real cases must catch it.
        altered = copy.deepcopy(result)
        altered["rows"][0]["request_sha256"] = "f" * 64
        altered["experiment"]["requests_sha256"] = score.evidence_digest([
            [row["id"], row["request_sha256"]] for row in altered["rows"]
        ])
        altered["experiment_sha256"] = score.evidence_digest(altered["experiment"])
        with self.assertRaisesRegex(ValueError, "request fingerprint disagrees"):
            qualification.qualified_report(
                altered, self.manifest, self.manifest_sha,
                self.questions, self.dataset_bytes,
            )

    def test_runner_can_consume_explicit_frozen_input_file(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            dataset_path = here / "external-cases.json"
            dataset_path.write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps({
                "checkpoint": "pinned", "server_revision": "r1",
                "backend": "cpu", "dtype": "float32", "temperature": 2.41,
            }))
            answer = {"answers": {
                key: {"type": "noul", "noul": 0.5} for key in self.questions
            }}
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", return_value=(answer, 12.0)) as post:
                    run_http.main([
                        "--url", "http://127.0.0.1:8009/v1/systemone",
                        "--tag", "external",
                        "--server-config", str(here / "server.json"),
                        "--dataset", str(dataset_path),
                        "--qualification-manifest", str(here / "manifest.json"),
                        "B_noul_text",
                    ])
                    self.assertEqual(post.call_count, 2)
            result = json.loads(
                (here / "results" / "external_B_noul_text.json").read_text()
            )
            qualification.qualified_report(
                result, self.manifest, self.manifest_sha,
                self.questions, self.dataset_bytes,
            )


if __name__ == "__main__":
    unittest.main()
