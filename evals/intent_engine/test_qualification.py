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
        self.endpoint = "http://127.0.0.1:8009/v1/systemone"
        self.server_config = {
            "checkpoint": "pinned", "server_revision": "r1",
            "backend": "cpu", "dtype": "float32", "temperature": 2.41,
        }
        self.manifest = {
            "version": 1, "application": "historical-intent-evaluation",
            "dataset_sha256": self.dataset_sha,
            "questions_sha256": qualification.sha256_ordered_questions(self.questions),
            "holdout_requests_sha256": self.holdout_request_digest(),
            "arm": {
                "config": "B_noul_text", "requested_model": None,
                "endpoint_sha256": qualification.sha256_bytes(self.endpoint.encode("utf-8")),
                "server_config_sha256": qualification.sha256_json(self.server_config),
                "timeout_ms": 1_800_000,
            },
            "noul_temperature": None,
            "max_request_bytes": 65536,
            "max_input_tokens": 64,
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

    def holdout_request_digest(self, questions=None, model=None, config="B_noul_text"):
        questions = self.questions if questions is None else questions
        fingerprints = []
        for case in self.dataset[2:]:
            body = {"state": run_http.STATES[config](case), "questions": questions}
            if model is not None:
                body["model"] = model
            fingerprints.append([
                case["id"],
                hashlib.sha256(run_http.request_bytes(body)).hexdigest(),
            ])
        return score.evidence_digest(fingerprints)

    def valid(self, manifest=None, cases=None):
        return qualification.validate_manifest(
            self.manifest if manifest is None else manifest,
            self.dataset_sha, self.questions,
            self.dataset if cases is None else cases,
        )

    def test_ambiguous_frozen_input_json_refuses_without_model_calls(self):
        # Same-value duplicates are deliberately non-vacuous: the old
        # last-key-wins parser accepted all three frozen artifacts.
        raw_manifest = self.manifest_bytes.decode("utf-8")
        raw_dataset = self.dataset_bytes.decode("utf-8")
        raw_server = json.dumps(self.server_config)
        variants = (
            ("manifest", raw_manifest.replace(
                '"max_input_tokens": 64',
                '"max_input_tokens": 64, "max_input_tokens": 64', 1)),
            ("dataset", raw_dataset.replace(
                '"project_id": "project-b"',
                '"project_id": "project-b", "project_id": "project-b"', 1)),
            ("server", raw_server.replace(
                '"checkpoint": "pinned"',
                '"checkpoint": "pinned", "checkpoint": "pinned"', 1)),
        )
        for changed_file, ambiguous in variants:
            with self.subTest(changed_file=changed_file):
                with tempfile.TemporaryDirectory() as temp:
                    here = Path(temp)
                    dataset_bytes = (ambiguous.encode("utf-8")
                                     if changed_file == "dataset" else self.dataset_bytes)
                    manifest = copy.deepcopy(self.manifest)
                    manifest["dataset_sha256"] = qualification.sha256_bytes(dataset_bytes)
                    (here / "dataset.json").write_bytes(dataset_bytes)
                    (here / "manifest.json").write_text(
                        ambiguous if changed_file == "manifest" else json.dumps(manifest)
                    )
                    (here / "server.json").write_text(
                        ambiguous if changed_file == "server" else raw_server
                    )
                    with patch.object(run_http, "HERE", str(here)), patch.object(
                        run_http, "post"
                    ) as post:
                        with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
                            run_http.main([
                                "--url", self.endpoint, "--tag", "frozen",
                                "--server-config", str(here / "server.json"),
                                "--qualification-manifest", str(here / "manifest.json"),
                                "B_noul_text",
                            ])
                        post.assert_not_called()
                    self.assertFalse((here / "results").exists())

    def test_nonfinite_unused_manifest_field_is_not_reproducible_json(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "manifest.json"
            path.write_bytes(self.manifest_bytes[:-1] + b', "unreviewed": NaN}')
            with self.assertRaisesRegex(ValueError, "nonfinite JSON constant"):
                qualification.load_manifest(path)

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


    def test_native_kev_truncation_is_measured_not_assumed_from_a_flag(self):
        # Native Kev emits state_tokens/state_tokens_used and a top-level
        # truncated flag, not necessarily state_tokens_dropped.
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", self.endpoint, "--tag", "native-kev",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            calls = []

            def native_answer(url, body, *, timeout):
                self.assertEqual(timeout, self.manifest["arm"]["timeout_ms"] / 1000)
                index = len(calls)
                calls.append(body)
                case = self.dataset[index + 2]
                return {
                    "model": "self-reported-test-model",
                    "answers": {
                        key: {"type": "noul", "noul": 0.9 if key in case["labels"] else 0.1}
                        for key in self.questions
                    },
                    "usage": {
                        "input_tokens": 64, "state_tokens": 32,
                        "state_tokens_used": 32 if index == 0 else 24,
                    },
                    "truncated": index == 1,
                    "latency_ms": 5.0,
                }, 10.0

            with patch.object(run_http, "HERE", str(here)), patch.object(
                run_http, "post", side_effect=native_answer
            ):
                run_http.main(args)
            self.assertEqual(len(calls), 2)
            result = json.loads(
                (here / "results" / "native-kev_B_noul_text.json").read_text()
            )
            self.assertEqual([r["dropped"] for r in result["rows"]], [0, 8])
            self.assertEqual([r["state_tokens"] for r in result["rows"]], [32, 32])
            self.assertEqual([r["state_tokens_used"] for r in result["rows"]], [32, 24])
            self.assertEqual([r["truncated"] for r in result["rows"]], [False, True])
            report = qualification.qualified_report(
                result, self.manifest, self.manifest_sha, self.questions,
                self.dataset_bytes,
            )
            self.assertEqual(report["latency"]["cases_truncated"], 1)
            self.assertEqual(report["numeric_truncation_source_evidence"]["measured_cases"], 2)
            self.assertFalse(report["checks"]["untruncated_input_evidence"])
            self.assertFalse(report["qualified_against_frozen_tolerances"])
            self.assertTrue(report["checks"]["reported_input_token_budget"])
            # Rehashing only the declared experiment cannot hide a rewritten
            # normalized drop count when native server counts are present.
            forged = copy.deepcopy(result)
            forged["rows"][1]["dropped"] = 0
            with self.assertRaisesRegex(ValueError, "truncation row disagrees"):
                qualification.qualified_report(
                    forged, self.manifest, self.manifest_sha,
                    self.questions, self.dataset_bytes,
                )

    def test_conflicting_native_truncation_is_refused_before_row_append(self):
        cases = (
            ("used_exceeds_original", {"truncated": False,
                "usage": {"state_tokens": 24, "state_tokens_used": 32}}),
            ("flag_contradicts_counts", {"truncated": False,
                "usage": {"state_tokens": 32, "state_tokens_used": 24}}),
            ("direct_contradicts_native", {"truncated": True,
                "usage": {"state_tokens": 32, "state_tokens_used": 24,
                          "state_tokens_dropped": 0}}),
            ("invalid_flag", {"truncated": 0,
                "usage": {"state_tokens": 32, "state_tokens_used": 32}}),
            ("flags_disagree", {"truncated": False,
                "usage": {"truncated": True}}),
            ("invalid_count", {"truncated": False,
                "usage": {"state_tokens": True, "state_tokens_used": 32}}),
        )
        for label, metadata in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as temp:
                here = Path(temp)
                (here / "dataset.json").write_bytes(self.dataset_bytes)
                (here / "manifest.json").write_bytes(self.manifest_bytes)
                (here / "server.json").write_text(json.dumps(self.server_config))
                args = [
                    "--url", self.endpoint, "--tag", "bad-native",
                    "--server-config", str(here / "server.json"),
                    "--qualification-manifest", str(here / "manifest.json"),
                    "B_noul_text",
                ]
                response = {
                    "model": "self-reported-test-model",
                    "answers": {
                        key: {"type": "noul", "noul": 0.5}
                        for key in self.questions
                    },
                    "usage": {"input_tokens": 64, **metadata["usage"]},
                    "truncated": metadata["truncated"],
                }
                with patch.object(run_http, "HERE", str(here)), patch.object(
                    run_http, "post", return_value=(response, 10.0)
                ) as post:
                    with self.assertRaisesRegex(ValueError, "truncation"):
                        run_http.main(args)
                post.assert_called_once()
                self.assertFalse(
                    (here / "results" / "bad-native_B_noul_text.json").exists()
                )
                partial = here / "results" / "bad-native_B_noul_text.json.partial"
                self.assertEqual(len(partial.read_text().splitlines()), 1)


    def test_native_truncation_partial_tampering_refuses_without_network(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", self.endpoint, "--tag", "native-partial",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            response = {
                "model": "self-reported-test-model",
                "answers": {
                    key: {"type": "noul", "noul": 0.9}
                    for key in self.questions
                },
                "usage": {
                    "input_tokens": 64, "state_tokens": 30,
                    "state_tokens_used": 25,
                },
                "truncated": True,
            }
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", side_effect=[
                    (response, 10.0), RuntimeError("network interruption")
                ]) as first:
                    with self.assertRaisesRegex(RuntimeError, "network interruption"):
                        run_http.main(args)
                    self.assertEqual(first.call_count, 2)
                partial = here / "results" / "native-partial_B_noul_text.json.partial"
                header, row = partial.read_text().splitlines()
                original = json.loads(row)
                self.assertEqual(original["dropped"], 5)
                original["dropped"] = 0
                partial.write_text(header + "\n" + json.dumps(original) + "\n")
                before = partial.read_bytes()
                with patch.object(run_http, "post") as blocked:
                    with self.assertRaisesRegex(ValueError, "truncation row disagrees"):
                        run_http.main(args)
                    blocked.assert_not_called()
                self.assertEqual(partial.read_bytes(), before)
                self.assertFalse(
                    (here / "results" / "native-partial_B_noul_text.json").exists()
                )

    def test_impossible_token_counters_cannot_qualify_even_with_rehashed_evidence(self):
        control = qualification.qualified_report(
            self.signed_result(), self.manifest, self.manifest_sha,
            self.questions, self.dataset_bytes,
        )
        self.assertTrue(control["qualified_against_frozen_tolerances"])
        # The old scorer accepted input_tokens=1 as proof of context fit
        # even when Kev reported using 32 state tokens on that request.
        forged = self.signed_result()
        forged["rows"][0].update(
            input_tokens=1, state_tokens=32, state_tokens_used=32, dropped=0,
        )
        with self.assertRaisesRegex(ValueError, "used state tokens exceed total input tokens"):
            qualification.qualified_report(
                forged, self.manifest, self.manifest_sha,
                self.questions, self.dataset_bytes,
            )

        # A second contradictory server receipt also cannot be interpreted
        # as a legitimate, zero-truncation or measured-drop result.
        forged = self.signed_result()
        forged["rows"][0].update(
            state_tokens=3, reported_state_tokens_dropped=4, dropped=4,
        )
        with self.assertRaisesRegex(ValueError, "dropped state tokens exceed original"):
            qualification.qualified_report(
                forged, self.manifest, self.manifest_sha,
                self.questions, self.dataset_bytes,
            )

    def test_boolean_truncation_without_counts_is_unknown(self):
        self.assertIsNone(run_http.dropped_state_tokens({"truncated": False}, {}))
        self.assertIsNone(run_http.dropped_state_tokens({"truncated": True}, {}))
        self.assertEqual(
            run_http.dropped_state_tokens(
                {"truncated": False}, {"state_tokens_dropped": 0}
            ), 0
        )

    def test_missing_or_dropped_server_tokens_cannot_qualify(self):
        for value in (None, 1):
            with self.subTest(state_tokens_dropped=value):
                result = self.signed_result()
                result["rows"][0].update(
                    dropped=value, reported_state_tokens_dropped=value,
                )
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

    def test_forged_zero_drops_without_server_counter_does_not_qualify(self):
        result = self.signed_result()
        del result["rows"][0]["reported_state_tokens_dropped"]
        report = qualification.qualified_report(
            result, self.manifest, self.manifest_sha,
            self.questions, self.dataset_bytes,
        )
        # The old aggregate accepted two claimed zeros even though one row
        # had no original numeric server evidence. That is not measured zero.
        self.assertEqual(report["latency"]["cases_truncated"], 0)
        self.assertEqual(report["latency"]["truncation_measured_cases"], 2)
        self.assertEqual(
            report["numeric_truncation_source_evidence"],
            {"measured_cases": 1, "missing_cases": 1},
        )
        self.assertFalse(report["checks"]["untruncated_input_evidence"])
        self.assertFalse(report["qualified_against_frozen_tolerances"])

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
            ("missing_calibration", lambda m: m.pop("noul_temperature"), "frozen client calibration"),
            ("nonfinite_calibration", lambda m: m.update(noul_temperature=float("nan")), "client calibration"),
            ("zero_calibration", lambda m: m.update(noul_temperature=0), "client calibration"),
            ("boolean_calibration", lambda m: m.update(noul_temperature=True), "client calibration"),
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
            rendered = run_http.request_bytes({
                "state": run_http.STATES["B_noul_text"](case),
                "questions": self.questions,
            })
            rows.append({
                "id": case["id"], "labels": case["labels"],
                "context_needed": case["context_needed"],
                "probs": probs,
                "ms": 10 if case["labels"] else 20,
                "input_tokens": 64,
                "dropped": 0,
                "reported_state_tokens_dropped": 0,
                "request_sha256": hashlib.sha256(rendered).hexdigest(),
                "request_size_bytes": len(rendered),
                "observed_model": "self-reported-test-model",
            })
        experiment = {
            "version": 2,
            "config": "B_noul_text",
            "requested_model": None,
            "endpoint_sha256": self.manifest["arm"]["endpoint_sha256"],
            "timeout_ms": self.manifest["arm"]["timeout_ms"],
            "case_ids": [row["id"] for row in rows],
            "dataset_sha256": self.dataset_sha,
            "case_annotations_sha256": score.evidence_digest([
                [r["id"], r["labels"], r["context_needed"]] for r in rows
            ]),
            "requests_sha256": score.evidence_digest([
                [r["id"], r["request_sha256"]] for r in rows
            ]),
            "noul_temperature": None,
            "server_config": copy.deepcopy(self.server_config),
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
        self.assertEqual(report["request_size_bytes"]["frozen_limit"], 65536)
        self.assertEqual(report["input_token_evidence"]["frozen_limit"], 64)
        self.assertEqual(report["input_token_evidence"]["measured_cases"], 2)
        self.assertEqual(report["input_token_evidence"]["max_reported_tokens"], 64)
        self.assertTrue(report["checks"]["reported_input_token_budget"])
        self.assertEqual(report["request_size_bytes"]["total"], sum(
            row["request_size_bytes"] for row in result["rows"]
        ))
        self.assertEqual(report["holdout_decisions"]["coverage_rate"], 0.5)
        self.assertEqual(report["holdout_decisions"]["abstention_rate"], 0.5)
        self.assertEqual(report["holdout_decisions"]["unwanted_hint_rate"], 0)
        self.assertTrue(report["checks"]["untruncated_input_evidence"])
        self.assertEqual(report["reported_model"], "self-reported-test-model")
        self.assertTrue(report["checks"]["reported_model_consistency"])
        self.assertEqual(report["fixed_threshold_metrics"]["none_false_hint_rate"], 0)
        self.assertEqual(report["fixed_threshold_metrics"]["pos_cases_with_a_correct_hint"], 1)

        stricter = copy.deepcopy(self.manifest)
        stricter["error_tolerances"]["max_p95_ms"] = 19
        # An altered post-run manifest is not a valid requalification.
        with self.assertRaisesRegex(ValueError, "not bound"):
            qualification.qualified_report(result, stricter, qualification.sha256_json(stricter), self.questions, self.dataset_bytes)


    def test_qualified_scorer_rejects_edited_choice_and_extra_none_evidence(self):
        # These edits previously passed the generic historical scorer. An
        # independently scored Noul-only result must reject them even when
        # the request hashes, experiment SHA and verified labels still agree.
        control = self.signed_result()
        self.assertTrue(qualification.qualified_report(
            control, self.manifest, self.manifest_sha,
            self.questions, self.dataset_bytes,
        )["qualified_against_frozen_tolerances"])

        for variant in ("extra_none_probability", "choice_on_every_case"):
            with self.subTest(variant=variant):
                forged = copy.deepcopy(control)
                if variant == "extra_none_probability":
                    forged["rows"][0]["probs"]["none"] = 0.95
                else:
                    for row in forged["rows"]:
                        row["choice"] = "none"

                # An unqualified historical result may legitimately have
                # these fields. This passing control proves the old scored
                # path would not refuse our adversarial evidence.
                self.assertIn(
                    "latency",
                    score.score_config(forged, cats=tuple(self.questions)),
                )
                with self.assertRaisesRegex(ValueError, "only frozen Noul probabilities"):
                    qualification.qualified_report(
                        forged, self.manifest, self.manifest_sha,
                        self.questions, self.dataset_bytes,
                    )

    def test_qualification_reports_missing_or_drifting_model_identity(self):
        for value in (None, "", True, "another-model"):
            with self.subTest(observed_model=value):
                result = self.signed_result()
                result["rows"][1]["observed_model"] = value
                report = qualification.qualified_report(
                    result, self.manifest, self.manifest_sha,
                    self.questions, self.dataset_bytes,
                )
                self.assertFalse(report["qualified_against_frozen_tolerances"])
                self.assertFalse(report["checks"]["reported_model_consistency"])
                self.assertIsNone(report["reported_model"])

    def test_qualification_rejects_model_different_from_frozen_selector(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["arm"]["requested_model"] = "pinned-model-selector"
        manifest["holdout_requests_sha256"] = self.holdout_request_digest(model="pinned-model-selector")
        manifest_sha = qualification.sha256_bytes(
            json.dumps(manifest).encode("utf-8")
        )
        result = self.signed_result()
        experiment = result["experiment"]
        experiment["qualification_manifest_sha256"] = manifest_sha
        experiment["requested_model"] = "pinned-model-selector"
        for row in result["rows"]:
            case = next(c for c in self.dataset if c["id"] == row["id"])
            body = {
                "state": run_http.STATES["B_noul_text"](case),
                "questions": self.questions,
                "model": "pinned-model-selector",
            }
            rendered = run_http.request_bytes(body)
            row["request_sha256"] = hashlib.sha256(rendered).hexdigest()
            row["request_size_bytes"] = len(rendered)
        experiment["requests_sha256"] = score.evidence_digest([
            [row["id"], row["request_sha256"]] for row in result["rows"]
        ])
        result["experiment_sha256"] = score.evidence_digest(experiment)
        report = qualification.qualified_report(
            result, manifest, manifest_sha, self.questions, self.dataset_bytes
        )
        self.assertFalse(report["checks"]["reported_model_consistency"])
        self.assertFalse(report["qualified_against_frozen_tolerances"])

    def test_runner_refuses_explicit_model_mismatch_before_result_append(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            manifest = copy.deepcopy(self.manifest)
            manifest["arm"]["requested_model"] = "pinned-model-selector"
            manifest["holdout_requests_sha256"] = self.holdout_request_digest(model="pinned-model-selector")
            (here / "manifest.json").write_text(json.dumps(manifest))
            (here / "server.json").write_text(json.dumps(self.server_config))
            response = {
                "model": "wrong-model",
                "answers": {
                    key: {"type": "noul", "noul": 0.5}
                    for key in self.questions
                },
            }
            args = [
                "--url", self.endpoint,
                "--tag", "model-mismatch",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "--send-model", "pinned-model-selector", "B_noul_text",
            ]
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", return_value=(response, 12.0)) as post:
                    with self.assertRaisesRegex(ValueError, "reported model differs"):
                        run_http.main(args)
                    self.assertEqual(post.call_count, 1)
            self.assertFalse((here / "results" / "model-mismatch_B_noul_text.json").exists())
            partial = here / "results" / "model-mismatch_B_noul_text.json.partial"
            self.assertEqual(len(partial.read_text().splitlines()), 1)

    def test_qualified_inference_refuses_server_model_change_before_second_append(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", self.endpoint, "--tag", "model-drift",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]

            def reply(model):
                return {
                    "model": model,
                    "answers": {key: {"type": "noul", "noul": 0.5}
                                for key in self.questions},
                    "usage": {"input_tokens": 64, "state_tokens": 32,
                              "state_tokens_used": 32},
                    "truncated": False,
                }, 10.0

            with patch.object(run_http, "HERE", str(here)), patch.object(
                run_http, "post", side_effect=[
                    reply("reported-model-a"), reply("reported-model-b"),
                ]
            ) as post:
                with self.assertRaisesRegex(ValueError, "mix reported models"):
                    run_http.main(args)
                self.assertEqual(post.call_count, 2)
            path = here / "results" / "model-drift_B_noul_text.json"
            self.assertFalse(path.exists())
            header, first = (here / "results" / "model-drift_B_noul_text.json.partial").read_text().splitlines()
            self.assertEqual(json.loads(first)["observed_model"], "reported-model-a")
            self.assertEqual(json.loads(header)["experiment"]["case_ids"],
                             ["holdout-positive", "holdout-negative"])

    def test_qualified_resume_refuses_forged_model_before_network(self):
        for altered_model in (None, "different-model"):
            with self.subTest(altered_model=altered_model), tempfile.TemporaryDirectory() as temp:
                here = Path(temp)
                (here / "dataset.json").write_bytes(self.dataset_bytes)
                manifest = copy.deepcopy(self.manifest)
                manifest["arm"]["requested_model"] = "selected-model"
                manifest["holdout_requests_sha256"] = self.holdout_request_digest(model="selected-model")
                (here / "manifest.json").write_text(json.dumps(manifest))
                (here / "server.json").write_text(json.dumps(self.server_config))
                args = [
                    "--url", self.endpoint, "--tag", "model-resume",
                    "--server-config", str(here / "server.json"),
                    "--qualification-manifest", str(here / "manifest.json"),
                    "--send-model", "selected-model", "B_noul_text",
                ]
                answer = {
                    "model": "selected-model",
                    "answers": {key: {"type": "noul", "noul": 0.5}
                                for key in self.questions},
                    "usage": {"input_tokens": 64, "state_tokens": 32,
                              "state_tokens_used": 32},
                    "truncated": False,
                }
                with patch.object(run_http, "HERE", str(here)):
                    with patch.object(run_http, "post", side_effect=[
                        (answer, 10.0), OSError("interrupted"),
                    ]) as first:
                        with self.assertRaisesRegex(OSError, "interrupted"):
                            run_http.main(args)
                        self.assertEqual(first.call_count, 2)
                    partial = here / "results" / "model-resume_B_noul_text.json.partial"
                    original = partial.read_bytes()
                    header, row = original.decode("utf-8").splitlines()
                    corrupted = json.loads(row)
                    corrupted["observed_model"] = altered_model
                    partial.write_text(header + "\n" + json.dumps(corrupted) + "\n")
                    before = partial.read_bytes()
                    with patch.object(run_http, "post") as blocked:
                        expected = "requires a server-reported model" if altered_model is None else "frozen selector"
                        with self.assertRaisesRegex(ValueError, expected):
                            run_http.main(args)
                        blocked.assert_not_called()
                    self.assertEqual(partial.read_bytes(), before)
                    self.assertFalse(
                        (here / "results" / "model-resume_B_noul_text.json").exists()
                    )
                    # The valid original prefix must still resume normally.
                    partial.write_bytes(original)
                    with patch.object(run_http, "post", return_value=(answer, 10.0)) as remaining:
                        run_http.main(args)
                        remaining.assert_called_once()
                    self.assertEqual(len(json.loads(
                        (here / "results" / "model-resume_B_noul_text.json").read_text()
                    )["rows"]), 2)

    def test_frozen_arm_refuses_missing_reported_model_before_appending(self):
        # With no explicit model selector, an absent identity used to be
        # appended and the runner continued spending holdout calls.
        good = {
            "model": "observed-checkpoint",
            "answers": {key: {"type": "noul", "noul": 0.5}
                        for key in self.questions},
            "usage": {"input_tokens": 64, "state_tokens": 32,
                      "state_tokens_used": 32},
            "truncated": False,
        }
        for index in (0, 1):
            with self.subTest(failing_case=index), tempfile.TemporaryDirectory() as temp:
                here = Path(temp)
                (here / "dataset.json").write_bytes(self.dataset_bytes)
                (here / "manifest.json").write_bytes(self.manifest_bytes)
                (here / "server.json").write_text(json.dumps(self.server_config))
                replies = [copy.deepcopy(good), copy.deepcopy(good)]
                del replies[index]["model"]
                args = [
                    "--url", self.endpoint, "--tag", "missing-model",
                    "--server-config", str(here / "server.json"),
                    "--qualification-manifest", str(here / "manifest.json"),
                    "B_noul_text",
                ]
                with patch.object(run_http, "HERE", str(here)), patch.object(
                    run_http, "post", side_effect=[(reply, 10.0) for reply in replies]
                ) as post:
                    with self.assertRaisesRegex(ValueError, "requires a server-reported model"):
                        run_http.main(args)
                    self.assertEqual(post.call_count, index + 1)
                completed = here / "results" / "missing-model_B_noul_text.json"
                self.assertFalse(completed.exists())
                partial = Path(str(completed) + ".partial")
                recorded = partial.read_text().splitlines()
                self.assertEqual(len(recorded), index + 1)
                if index:
                    self.assertEqual(json.loads(recorded[1])["observed_model"],
                                     "observed-checkpoint")

    def test_frozen_resume_refuses_missing_default_model_before_network(self):
        # No --send-model: absent/null/blank names must still be refused before
        # another held-out inference. An unchanged valid prefix can resume.
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", self.endpoint, "--tag", "missing-resume-model",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            reply = {
                "model": "observed-checkpoint",
                "answers": {key: {"type": "noul", "noul": 0.5}
                            for key in self.questions},
                "usage": {"input_tokens": 64, "state_tokens": 32,
                          "state_tokens_used": 32},
                "truncated": False,
            }
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", side_effect=[
                    (reply, 10.0), OSError("interrupted")
                ]):
                    with self.assertRaisesRegex(OSError, "interrupted"):
                        run_http.main(args)
                completed = here / "results" / "missing-resume-model_B_noul_text.json"
                partial = Path(str(completed) + ".partial")
                original = partial.read_bytes()
                header, row = original.decode("utf-8").splitlines()
                for changed in ("absent", "null", "blank"):
                    with self.subTest(changed=changed):
                        forged = json.loads(row)
                        if changed == "absent":
                            del forged["observed_model"]
                        else:
                            forged["observed_model"] = None if changed == "null" else " "
                        partial.write_text(header + "\n" + json.dumps(forged) + "\n")
                        before = partial.read_bytes()
                        with patch.object(run_http, "post") as blocked:
                            with self.assertRaisesRegex(ValueError, "requires a server-reported model"):
                                run_http.main(args)
                            blocked.assert_not_called()
                        self.assertEqual(partial.read_bytes(), before)
                        self.assertFalse(completed.exists())
                partial.write_bytes(original)
                with patch.object(run_http, "post", return_value=(reply, 10.0)) as remaining:
                    run_http.main(args)
                    remaining.assert_called_once()
                self.assertEqual([r["observed_model"] for r in json.loads(
                    completed.read_text()
                )["rows"]], ["observed-checkpoint", "observed-checkpoint"])

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
            ("non_object_row", lambda r: r["rows"].append(None), "held-out case order"),
            ("identity", lambda r: r["experiment"].pop("qualification_manifest_sha256"), "not bound"),
            ("forged_review", lambda r: r["rows"][1].update(context_needed=True), "reviewed labels"),
        ):
            with self.subTest(field=field):
                result = copy.deepcopy(base)
                mutation(result)
                with self.assertRaisesRegex(ValueError, pattern):
                    qualification.qualified_report(result, self.manifest, self.manifest_sha, self.questions, self.dataset_bytes)


    def test_frozen_client_calibration_cannot_be_changed_by_rehashing(self):
        # Rehashing both the declared manifest and experiment must not make
        # an unfrozen post-hoc client temperature qualify.
        for temperature in (0.5, 1.25):
            with self.subTest(temperature=temperature):
                changed = copy.deepcopy(self.manifest)
                changed["noul_temperature"] = temperature
                changed_sha = qualification.sha256_json(changed)
                forged = self.signed_result()
                forged["experiment"]["qualification_manifest_sha256"] = changed_sha
                forged["experiment_sha256"] = score.evidence_digest(forged["experiment"])
                with self.assertRaisesRegex(ValueError, "client calibration differs"):
                    qualification.qualified_report(
                        forged, changed, changed_sha, self.questions, self.dataset_bytes
                    )

    def test_http_runner_requires_frozen_client_calibration_before_network(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            manifest_path = here / "manifest.json"
            manifest_path.write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", "http://127.0.0.1:8009/v1/systemone",
                "--tag", "calibration",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(manifest_path),
                "--noul-temperature", "1.25", "B_noul_text",
            ]
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post") as blocked:
                    with self.assertRaisesRegex(ValueError, "client calibration differs"):
                        run_http.main(args)
                    blocked.assert_not_called()
                self.assertFalse((here / "results").exists())
                frozen = copy.deepcopy(self.manifest)
                frozen["noul_temperature"] = 1.25
                manifest_path.write_text(json.dumps(frozen))
                answer = {"model": "self-reported-test-model", "answers": {
                    key: {"type": "noul", "noul": 0.5}
                    for key in self.questions
                }}
                with patch.object(run_http, "post", return_value=(answer, 12.0)) as infer:
                    run_http.main(args)
                    self.assertEqual(infer.call_count, 2)
                result = json.loads(
                    (here / "results" / "calibration_B_noul_text.json").read_text()
                )
                self.assertEqual(result["experiment"]["noul_temperature"], 1.25)
                self.assertEqual(result["rows"][0]["probs_raw"]["impact"], 0.5)
                report = qualification.qualified_report(
                    result, frozen, qualification.sha256_bytes(manifest_path.read_bytes()),
                    self.questions, self.dataset_bytes,
                )
                self.assertFalse(report["qualified_against_frozen_tolerances"])

    def test_http_runner_asks_only_pre_frozen_holdout_and_resumes_with_exact_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            calls = []

            def answer(url, body, **kwargs):
                calls.append(body)
                return {"model": "self-reported-test-model", "answers": {
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
            self.assertEqual(
                [row["request_size_bytes"] for row in result["rows"]],
                [len(run_http.request_bytes(body)) for body in calls],
            )
            self.assertEqual([r["id"] for r in result["rows"]], [
                "holdout-positive", "holdout-negative"
            ])
            self.assertEqual(
                [r["observed_model"] for r in result["rows"]],
                ["self-reported-test-model", "self-reported-test-model"],
            )
            self.assertEqual(result["experiment"]["qualification_manifest_sha256"], self.manifest_sha)
            self.assertEqual(result["experiment"]["case_ids"], [
                "holdout-positive", "holdout-negative"
            ])

    def test_qualified_sample_is_bound_to_the_frozen_experiment_on_resume(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            sample = here / "first-response.json"
            args = [
                "--url", self.endpoint,
                "--tag", "sample-provenance",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "--sample-out", str(sample),
                "B_noul_text",
            ]
            calls = []

            def answer(url, body, **kwargs):
                calls.append(body)
                if len(calls) == 2:
                    raise OSError("interrupted before second response")
                return {
                    "model": "self-reported-test-model",
                    "answers": {key: {"type": "noul", "noul": 0.2}
                                for key in body["questions"]},
                }, 4.0

            partial = here / "results" / "sample-provenance_B_noul_text.json.partial"
            final = here / "results" / "sample-provenance_B_noul_text.json"
            with patch.object(run_http, "HERE", str(here)), patch.object(
                run_http, "post", side_effect=answer
            ):
                with self.assertRaisesRegex(OSError, "interrupted before second response"):
                    run_http.main(args)
            self.assertEqual(len(calls), 2)
            self.assertTrue(partial.exists())
            self.assertTrue(sample.exists())
            self.assertFalse(final.exists())
            sample_bytes = sample.read_bytes()
            evidence = json.loads(sample_bytes)
            header = json.loads(partial.read_text().splitlines()[0])
            self.assertEqual(evidence["experiment_sha256"], header["experiment_sha256"])
            self.assertEqual(evidence["case_id"], "holdout-positive")
            self.assertEqual(
                evidence["request_sha256"],
                hashlib.sha256(run_http.request_bytes(calls[0])).hexdigest(),
            )
            partial_bytes = partial.read_bytes()
            for field, replacement in (
                ("experiment_sha256", "0" * 64),
                ("request_sha256", "f" * 64),
            ):
                with self.subTest(field=field):
                    forged = copy.deepcopy(evidence)
                    forged[field] = replacement
                    sample.write_text(json.dumps(forged))
                    with patch.object(run_http, "HERE", str(here)), patch.object(
                        run_http, "post"
                    ) as post:
                        with self.assertRaisesRegex(ValueError, "qualified sample disagrees"):
                            run_http.main(args)
                        post.assert_not_called()
                    self.assertEqual(partial.read_bytes(), partial_bytes)
                    self.assertFalse(final.exists())
            sample.write_bytes(sample_bytes)
            with patch.object(run_http, "HERE", str(here)), patch.object(
                run_http, "post", side_effect=lambda url, body, **kw: (
                    {"model": "self-reported-test-model",
                     "answers": {key: {"type": "noul", "noul": 0.1}
                                 for key in body["questions"]}},
                    3.0,
                )
            ) as post:
                run_http.main(args)
                self.assertEqual(post.call_count, 1)
            self.assertTrue(final.exists())
            self.assertFalse(partial.exists())
            self.assertEqual(sample.read_bytes(), sample_bytes)
            self.assertEqual(len(json.loads(final.read_text())["rows"]), 2)

            # A new tag cannot accidentally publish a stale raw response as
            # evidence of a completely separate experiment.
            new_args = args.copy()
            new_args[new_args.index("sample-provenance")] = "other-experiment"
            with patch.object(run_http, "HERE", str(here)), patch.object(
                run_http, "post"
            ) as post:
                with self.assertRaisesRegex(ValueError, "qualified sample already exists"):
                    run_http.main(new_args)
                post.assert_not_called()
            self.assertFalse(
                (here / "results" / "other-experiment_B_noul_text.json.partial").exists()
            )
            self.assertEqual(sample.read_bytes(), sample_bytes)

    def test_runner_refuses_group_leakage_without_network_or_partial_output(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            changed = copy.deepcopy(self.manifest)
            changed["annotations"][2]["project_id"] = "project-a"
            (here / "manifest.json").write_text(json.dumps(changed))
            (here / "server.json").write_text(json.dumps(self.server_config))
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
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", "http://127.0.0.1:8009/v1/systemone",
                "--tag", "interrupted",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(manifest_path),
                "B_noul_text",
            ]
            answer = {"model": "self-reported-test-model", "answers": {
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
                header_line, row_line = frozen_bytes.splitlines()
                falsified = json.loads(row_line)
                falsified["request_size_bytes"] += 1
                corrupted_bytes = header_line + b"\n" + json.dumps(falsified).encode("utf-8") + b"\n"
                partial.write_bytes(corrupted_bytes)
                with patch.object(run_http, "post") as blocked:
                    with self.assertRaisesRegex(ValueError, "mismatched request byte count"):
                        run_http.main(args)
                    blocked.assert_not_called()
                self.assertEqual(partial.read_bytes(), corrupted_bytes)
                partial.write_bytes(frozen_bytes)
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
        # Keep the declaration and the result internally consistent so this
        # exercises reconstruction against the original requests.
        manifest = copy.deepcopy(self.manifest)
        manifest["holdout_requests_sha256"] = altered["experiment"]["requests_sha256"]
        manifest_sha = qualification.sha256_bytes(json.dumps(manifest).encode("utf-8"))
        altered["experiment"]["qualification_manifest_sha256"] = manifest_sha
        altered["experiment_sha256"] = score.evidence_digest(altered["experiment"])
        with self.assertRaisesRegex(ValueError, "request fingerprint disagrees"):
            qualification.qualified_report(
                altered, manifest, manifest_sha, self.questions, self.dataset_bytes,
            )

    def test_runner_can_consume_explicit_frozen_input_file(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            dataset_path = here / "external-cases.json"
            dataset_path.write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            answer = {"model": "self-reported-test-model", "answers": {
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



    def test_qualified_timeout_is_enforced_and_scored_against_frozen_arm(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["arm"]["timeout_ms"] = 250
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_text(json.dumps(manifest))
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", self.endpoint, "--tag", "deadline",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "--timeout-ms", "250", "B_noul_text",
            ]
            response = {
                "model": "self-reported-test-model",
                "answers": {key: {"noul": 0.9} for key in self.questions},
                "usage": {"input_tokens": 32, "state_tokens_dropped": 0},
            }
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", return_value=(response, 5.0)) as post:
                    run_http.main(args)
                self.assertEqual(post.call_count, 2)
                self.assertTrue(all(call.kwargs == {"timeout": 0.25}
                                    for call in post.call_args_list))
                result = json.loads(
                    (here / "results" / "deadline_B_noul_text.json").read_text()
                )
                self.assertEqual(result["experiment"]["timeout_ms"], 250)
                # Rehashing result metadata cannot change the frozen deadline.
                result["experiment"]["timeout_ms"] = 1_800_000
                result["experiment_sha256"] = score.evidence_digest(result["experiment"])
                with self.assertRaisesRegex(ValueError, "HTTP arm differs"):
                    qualification.qualified_report(
                        result, manifest,
                        qualification.sha256_bytes((here / "manifest.json").read_bytes()),
                        self.questions, self.dataset_bytes,
                    )

    def test_frozen_http_arm_is_required_and_well_formed(self):
        mutations = (
            (lambda m: m.pop("arm"), "frozen HTTP arm"),
            (lambda m: m["arm"].pop("config"), "frozen HTTP arm"),
            (lambda m: m["arm"].update(config=""), "renderer config"),
            (lambda m: m["arm"].update(requested_model=""), "requested model"),
            (lambda m: m["arm"].update(endpoint_sha256="x" * 64), "endpoint_sha256"),
            (lambda m: m["arm"].update(server_config_sha256=None), "server_config_sha256"),
            (lambda m: m["arm"].pop("timeout_ms"), "frozen HTTP arm"),
            (lambda m: m["arm"].update(timeout_ms=0), "timeout_ms"),
            (lambda m: m["arm"].update(timeout_ms=1_800_001), "timeout_ms"),
            (lambda m: m["arm"].update(timeout_ms=True), "timeout_ms"),
        )
        for mutate, error in mutations:
            with self.subTest(error=error):
                manifest = copy.deepcopy(self.manifest)
                mutate(manifest)
                with self.assertRaisesRegex(ValueError, error):
                    self.valid(manifest)

    def test_frozen_http_arm_refuses_post_hoc_reselection_before_inference(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            server = here / "server.json"
            normal = [
                "--url", self.endpoint, "--tag", "locked-arm",
                "--server-config", str(server),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            trials = (
                ("renderer", lambda a: a.__setitem__(-1, "A_noul_prompt")),
                ("multiple-renderers", lambda a: a.append("A_noul_prompt")),
                ("model", lambda a: a.__setitem__(slice(-1, -1), ["--send-model", "unfrozen-model"])),
                ("endpoint", lambda a: a.__setitem__(1, "http://127.0.0.1:8010/v1/systemone")),
                ("timeout", lambda a: a.__setitem__(slice(-1, -1), ["--timeout-ms", "250"])),
            )
            with patch.object(run_http, "HERE", str(here)):
                for label, mutation in trials:
                    with self.subTest(label=label):
                        server.write_text(json.dumps(self.server_config))
                        args = normal.copy()
                        mutation(args)
                        with patch.object(run_http, "post") as blocked:
                            with self.assertRaisesRegex(ValueError, "HTTP arm differs"):
                                run_http.main(args)
                            blocked.assert_not_called()
                        self.assertFalse((here / "results").exists())
                drifted = copy.deepcopy(self.server_config)
                drifted["checkpoint"] = "another-checkpoint"
                server.write_text(json.dumps(drifted))
                with patch.object(run_http, "post") as blocked:
                    with self.assertRaisesRegex(ValueError, "HTTP arm differs"):
                        run_http.main(normal)
                    blocked.assert_not_called()
                self.assertFalse((here / "results").exists())

    def test_scorer_refuses_rehashed_unfrozen_http_arm(self):
        mutations = (
            lambda e: e.update(endpoint_sha256="f" * 64),
            lambda e: e["server_config"].update(checkpoint="different-pinned-model"),
        )
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                result = self.signed_result()
                mutate(result["experiment"])
                result["experiment_sha256"] = score.evidence_digest(result["experiment"])
                with self.assertRaisesRegex(ValueError, "HTTP arm differs"):
                    qualification.qualified_report(
                        result, self.manifest, self.manifest_sha,
                        self.questions, self.dataset_bytes,
                    )


    def test_frozen_token_budget_must_be_positive_integer(self):
        for limit in (None, 0, -1, True, 1.5, "8192"):
            with self.subTest(limit=limit):
                manifest = copy.deepcopy(self.manifest)
                manifest["max_input_tokens"] = limit
                with self.assertRaisesRegex(ValueError, "positive integer max_input_tokens"):
                    self.valid(manifest)

    def test_missing_zero_or_overlimit_input_tokens_cannot_qualify(self):
        for reported in (None, 0, 65):
            with self.subTest(reported=reported):
                result = self.signed_result()
                result["rows"][0]["input_tokens"] = reported
                report = qualification.qualified_report(
                    result, self.manifest, self.manifest_sha,
                    self.questions, self.dataset_bytes,
                )
                self.assertFalse(report["qualified_against_frozen_tolerances"])
                self.assertFalse(report["checks"]["reported_input_token_budget"])
                evidence = report["input_token_evidence"]
                self.assertEqual(evidence["measured_cases"], 1 if reported in (None, 0) else 2)
                self.assertEqual(evidence["missing_or_zero_cases"], 1 if reported in (None, 0) else 0)
                self.assertEqual(evidence["cases_over_frozen_limit"], int(reported == 65))

    def test_rehashed_tighter_token_budget_does_not_certify_old_measurements(self):
        restricted = copy.deepcopy(self.manifest)
        restricted["max_input_tokens"] = 63
        restricted_sha = qualification.sha256_json(restricted)
        result = self.signed_result()
        result["experiment"]["qualification_manifest_sha256"] = restricted_sha
        result["experiment_sha256"] = score.evidence_digest(result["experiment"])
        report = qualification.qualified_report(
            result, restricted, restricted_sha, self.questions, self.dataset_bytes,
        )
        self.assertFalse(report["qualified_against_frozen_tolerances"])
        self.assertFalse(report["checks"]["reported_input_token_budget"])
        self.assertEqual(report["input_token_evidence"]["cases_over_frozen_limit"], 2)

    def test_frozen_request_byte_budget_must_be_positive_integer(self):
        for budget in (None, 0, -1, True, 0.5, "4096"):
            with self.subTest(budget=budget):
                changed = copy.deepcopy(self.manifest)
                changed["max_request_bytes"] = budget
                with self.assertRaisesRegex(ValueError, "positive integer max_request_bytes"):
                    self.valid(changed)

    def test_frozen_request_byte_budget_refuses_before_network(self):
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            changed = copy.deepcopy(self.manifest)
            changed["max_request_bytes"] = 1
            (here / "manifest.json").write_text(json.dumps(changed))
            args = [
                "--url", self.endpoint, "--tag", "oversized",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "B_noul_text",
            ]
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post") as post:
                    with self.assertRaisesRegex(ValueError, "exceeds frozen request byte limit"):
                        run_http.main(args)
                    post.assert_not_called()
                self.assertFalse((here / "results").exists())

    def test_qualification_reconstructs_measured_request_sizes(self):
        original = self.signed_result()
        report = qualification.qualified_report(
            original, self.manifest, self.manifest_sha,
            self.questions, self.dataset_bytes,
        )
        self.assertGreater(report["request_size_bytes"]["min"], 0)
        self.assertEqual(
            report["request_size_bytes"]["max"],
            max(row["request_size_bytes"] for row in original["rows"]),
        )
        for value in (None, True, original["rows"][0]["request_size_bytes"] + 1):
            with self.subTest(forged_size=value):
                forged = copy.deepcopy(original)
                forged["rows"][0]["request_size_bytes"] = value
                with self.assertRaisesRegex(ValueError, "request byte count disagrees"):
                    qualification.qualified_report(
                        forged, self.manifest, self.manifest_sha,
                        self.questions, self.dataset_bytes,
                    )
        tighter = copy.deepcopy(self.manifest)
        tighter["max_request_bytes"] = 1
        tighter_sha = qualification.sha256_json(tighter)
        forged = copy.deepcopy(original)
        forged["experiment"]["qualification_manifest_sha256"] = tighter_sha
        forged["experiment_sha256"] = score.evidence_digest(forged["experiment"])
        with self.assertRaisesRegex(ValueError, "exceeds frozen request byte limit"):
            qualification.qualified_report(
                forged, tighter, tighter_sha, self.questions, self.dataset_bytes,
            )


    def test_frozen_rendered_requests_refuse_renderer_drift_before_inference(self):
        # The renderer's name, source cases, and questions may all remain
        # unchanged while the actual outbound model prompt changes.
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            with patch.object(run_http, "HERE", str(here)), patch.dict(
                run_http.STATES, {"B_noul_text": lambda case: "changed renderer output"},
            ), patch.object(run_http, "post") as inference:
                with self.assertRaisesRegex(ValueError, "rendered held-out requests differ"):
                    run_http.main([
                        "--url", self.endpoint, "--tag", "renderer-drift",
                        "--server-config", str(here / "server.json"),
                        "--qualification-manifest", str(here / "manifest.json"),
                        "B_noul_text",
                    ])
                inference.assert_not_called()
            self.assertFalse((here / "results").exists())

    def test_frozen_rendered_request_digest_refuses_rehashed_result(self):
        for bad_digest in (None, 42, "f" * 64):
            with self.subTest(frozen_digest=bad_digest):
                changed = copy.deepcopy(self.manifest)
                changed["holdout_requests_sha256"] = bad_digest
                if bad_digest is None or bad_digest == 42:
                    with self.assertRaisesRegex(ValueError, "frozen held-out request digest"):
                        self.valid(changed)
                    continue
                changed_sha = qualification.sha256_json(changed)
                forged = self.signed_result()
                forged["experiment"]["qualification_manifest_sha256"] = changed_sha
                forged["experiment_sha256"] = score.evidence_digest(forged["experiment"])
                # Every result/experiment fingerprint still agrees internally.
                # The independently frozen request body digest must also match.
                with self.assertRaisesRegex(ValueError, "rendered held-out requests differ"):
                    qualification.qualified_report(
                        forged, changed, changed_sha, self.questions, self.dataset_bytes,
                    )

    def test_frozen_question_order_refuses_a_second_holdout_arm_before_inference(self):
        # Sorted-key JSON digests collide across question reordering although
        # the runner sends dict entries in insertion order. That let the same
        # frozen manifest authorize multiple held-out request sequences.
        reordered = dict(reversed(list(self.questions.items())))
        self.assertEqual(set(reordered), set(self.questions))
        self.assertEqual(
            qualification.sha256_json(reordered),
            qualification.sha256_json(self.questions),
        )
        self.assertNotEqual(
            qualification.sha256_ordered_questions(reordered),
            qualification.sha256_ordered_questions(self.questions),
        )
        # Reordering type/instructions fields within one question does not
        # change the contract: only actual HTTP question sequence is frozen.
        equivalent = {
            key: {"instructions": item["instructions"], "type": item["type"]}
            for key, item in self.questions.items()
        }
        self.assertEqual(
            qualification.sha256_ordered_questions(equivalent),
            qualification.sha256_ordered_questions(self.questions),
        )
        with self.assertRaisesRegex(ValueError, "question vocabulary or instruction"):
            qualification.validate_manifest(
                self.manifest, self.dataset_sha, reordered, self.dataset,
            )

        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_bytes(self.manifest_bytes)
            (here / "questions.json").write_text(json.dumps(reordered))
            (here / "server.json").write_text(json.dumps(self.server_config))
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post") as inference:
                    with self.assertRaisesRegex(ValueError, "question vocabulary or instruction"):
                        run_http.main([
                            "--url", self.endpoint, "--tag", "reordered-holdout",
                            "--dataset", str(here / "dataset.json"),
                            "--server-config", str(here / "server.json"),
                            "--qualification-manifest", str(here / "manifest.json"),
                            "--questions-file", str(here / "questions.json"),
                            "B_noul_text",
                        ])
                    inference.assert_not_called()
                self.assertFalse(
                    (here / "results" / "reordered-holdout_B_noul_text.json.partial").exists()
                )

        # An original run must not be requalified using the altered order,
        # even though the question set and old sorted-object digest agree.
        with self.assertRaisesRegex(ValueError, "question vocabulary or instruction"):
            qualification.qualified_report(
                self.signed_result(), self.manifest, self.manifest_sha,
                reordered, self.dataset_bytes,
            )

    def test_frozen_ten_question_vocabulary_runs_and_scores_without_historical_categories(self):
        # This fixture exercises a declared ten-question contract; it is not a
        # canonical Scout export and never claims Kev inference or parity.
        questions = {
            key: value for key, value in self.questions.items()
            if key != "refactor_edit"
        }
        questions["atomic_edit"] = {
            "type": "noul", "instructions": "Is the user requesting a mechanical edit?"
        }
        questions["build_diagnostics"] = {
            "type": "noul", "instructions": "Does the user need a build error fixed?"
        }
        self.assertEqual(len(questions), 10)
        manifest = copy.deepcopy(self.manifest)
        manifest["questions_sha256"] = qualification.sha256_ordered_questions(questions)
        manifest["thresholds"] = {key: 0.6 for key in questions}
        manifest["holdout_requests_sha256"] = self.holdout_request_digest(questions=questions)
        manifest_sha = qualification.sha256_bytes(json.dumps(manifest).encode())
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_text(json.dumps(manifest))
            (here / "questions.json").write_text(json.dumps(questions))
            (here / "server.json").write_text(json.dumps(self.server_config))

            def inference(_url, body, **kwargs):
                self.assertEqual(set(body["questions"]), set(questions))
                probability = 0.9 if "What changes?" in body["state"] else 0.1
                return ({
                    "model": "kev-declared",
                    "answers": {
                        key: {"noul": probability if key == "impact" else 0.1}
                        for key in body["questions"]
                    },
                    "usage": {"input_tokens": 42, "state_tokens_dropped": 0},
                    "latency_ms": 5.0,
                }, 5.0)

            args = [
                "--url", self.endpoint, "--tag", "ten-questions",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "--questions-file", str(here / "questions.json"),
                "B_noul_text",
            ]
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", side_effect=inference) as post:
                    run_http.main(args)
                self.assertEqual(post.call_count, 2)
                output = here / "results" / "ten-questions_B_noul_text.json"
                result = json.loads(output.read_text())
                report = qualification.qualified_report(
                    result, manifest, manifest_sha, questions, self.dataset_bytes,
                )
                self.assertTrue(report["qualified_against_frozen_tolerances"])
                self.assertEqual(report["cases"], 2)
                self.assertEqual(len(score.score_config(
                    result, fixed_t=manifest["thresholds"], cats=tuple(questions)
                )["per_cat"]), 10)
                self.assertEqual(set(report["thresholds"]), set(questions))
                self.assertNotIn("refactor_edit", report["thresholds"])
                # The default historical vocabulary must not certify ten questions.
                with self.assertRaisesRegex(ValueError, "question vocabulary"):
                    qualification.qualified_report(
                        result, manifest, manifest_sha,
                        self.questions, self.dataset_bytes,
                    )
                altered = copy.deepcopy(questions)
                altered["atomic_edit"]["instructions"] = "A changed instruction"
                (here / "questions.json").write_text(json.dumps(altered))
                args[3] = "another-tag"
                with patch.object(run_http, "post") as blocked:
                    with self.assertRaisesRegex(ValueError, "question vocabulary"):
                        run_http.main(args)
                    blocked.assert_not_called()
                self.assertFalse((here / "results" / "another-tag_B_noul_text.json.partial").exists())

    def test_qualified_question_files_refuse_non_noul_or_ambiguous_vocabulary(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "questions.json"
            for body, expected in (
                ('{"q": {"type": "choice", "instructions": "Pick"}}', "Noul"),
                ('{"q": {"type": "noul", "instructions": ""}}', "Noul"),
                ('{"q": {"type": "noul", "instructions": "Good"}, '
                 '"q": {"type": "noul", "instructions": "Good"}}', "duplicate JSON"),
                ('{"q": {"type": "noul", "instructions": "Good", "extra": 1}}', "Noul"),
            ):
                with self.subTest(body=body):
                    path.write_text(body)
                    with self.assertRaisesRegex(ValueError, expected):
                        qualification.load_questions(path)

    def captured_scout_fixture(self):
        # Rust serde_json::Map emits lexicographically sorted object keys in
        # the Bazel runtime. The settings Vec uses a different question order.
        records = []
        for case in self.dataset[2:]:
            body = {"questions": self.questions, "state": "User: " + case["current_prompt"]}
            records.append({
                "id": case["id"],
                "body": json.dumps(body, ensure_ascii=False, separators=(",", ":"), sort_keys=True),
            })
        capture_bytes = json.dumps(records, ensure_ascii=False).encode("utf-8")
        manifest = copy.deepcopy(self.manifest)
        manifest["arm"]["config"] = qualification.CAPTURED_SCOUT_CONFIG
        manifest["max_state_bytes"] = 4096
        manifest["captured_requests_sha256"] = qualification.sha256_bytes(capture_bytes)
        manifest["holdout_requests_sha256"] = score.evidence_digest([
            [record["id"], hashlib.sha256(record["body"].encode("utf-8")).hexdigest()]
            for record in records
        ])
        return capture_bytes, manifest

    def test_captured_scout_accepts_canonical_wire_order_not_settings_vector_order(self):
        capture, manifest = self.captured_scout_fixture()
        self.assertNotEqual(list(self.questions), sorted(self.questions),
                            "fixture must distinguish settings order from canonical wire order")
        records = qualification.loads_unambiguous_json(capture)
        wire_questions = qualification.loads_unambiguous_json(records[0]["body"])["questions"]
        self.assertEqual(list(wire_questions), sorted(self.questions))
        self.assertNotEqual(list(wire_questions), list(self.questions))
        bodies = qualification.captured_request_bodies(
            capture, ["holdout-positive", "holdout-negative"],
            self.questions, None, manifest["max_request_bytes"],
            manifest["max_state_bytes"],
        )
        self.assertEqual(bodies["holdout-positive"], records[0]["body"].encode("utf-8"))
        self.assertEqual(bodies["holdout-negative"], records[1]["body"].encode("utf-8"))

    def test_captured_scout_requires_production_endpoint_even_after_refreezing(self):
        # A differently routed HTTP service can return valid-looking Noul
        # predictions, but Scout's configured client never sends requests
        # there. Rehash the manifest endpoint to exercise the actual parity
        # guard instead of an incidental digest mismatch.
        capture, manifest = self.captured_scout_fixture()
        self.valid(manifest)
        self.assertIsNone(run_http.require_scout_endpoint(self.endpoint))
        self.assertIsNone(run_http.require_scout_endpoint(
            "https://example.test/adapter/v1/systemone"
        ))
        variants = (
            self.endpoint.replace("/v1/systemone", "/v1/predict"),
            self.endpoint + "?model=other",
            self.endpoint + "#different",
            self.endpoint + "/",
        )
        for endpoint in variants:
            with self.subTest(endpoint=endpoint), tempfile.TemporaryDirectory() as temp:
                here = Path(temp)
                refrozen = copy.deepcopy(manifest)
                refrozen["arm"]["endpoint_sha256"] = qualification.sha256_bytes(
                    endpoint.encode("utf-8")
                )
                (here / "manifest.json").write_text(json.dumps(refrozen))
                (here / "dataset.json").write_bytes(self.dataset_bytes)
                (here / "captured.json").write_bytes(capture)
                (here / "server.json").write_text(json.dumps(self.server_config))
                with patch.object(run_http, "HERE", str(here)), patch.object(
                    run_http, "post"
                ) as post:
                    with self.assertRaisesRegex(
                        ValueError, "canonical /v1/systemone endpoint"
                    ):
                        run_http.main([
                            "--url", endpoint, "--tag", "captured-endpoint",
                            "--server-config", str(here / "server.json"),
                            "--qualification-manifest", str(here / "manifest.json"),
                            "--captured-requests", str(here / "captured.json"),
                            qualification.CAPTURED_SCOUT_CONFIG,
                        ])
                    post.assert_not_called()
                self.assertFalse((here / "results").exists())

    def test_captured_scout_requests_replay_exact_bytes_and_score(self):
        # This proves a captured arm is byte-exact, not that the synthetic
        # fixture came from real Scout or that its labels were reviewed.
        capture_bytes, manifest = self.captured_scout_fixture()
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_text(json.dumps(manifest))
            (here / "captured.json").write_bytes(capture_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            transmitted = []

            def inference(_url, body, **kwargs):
                self.assertIsInstance(body, bytes, "must not reserialize the Rust request")
                transmitted.append(body)
                request = qualification.loads_unambiguous_json(body)
                probability = 0.9 if "What changes?" in request["state"] else 0.1
                return ({
                    "model": "reported-scout-fixture",
                    "answers": {
                        key: {"noul": probability if key == "impact" else 0.1}
                        for key in self.questions
                    },
                    "usage": {"input_tokens": 42, "state_tokens_dropped": 0},
                }, 5.0)

            args = [
                "--url", self.endpoint, "--tag", "captured",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "--captured-requests", str(here / "captured.json"),
                qualification.CAPTURED_SCOUT_CONFIG,
            ]
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post", side_effect=inference) as post:
                    run_http.main(args)
                self.assertEqual(post.call_count, 2, "only frozen heldout cases may be inferred")
                expected = [
                    record["body"].encode("utf-8")
                    for record in qualification.loads_unambiguous_json(capture_bytes)
                ]
                self.assertEqual(transmitted, expected)
                result = json.loads((here / "results" /
                                    "captured_S_scout_captured.json").read_text())
                report = qualification.qualified_report(
                    result, manifest, qualification.sha256_bytes(
                        (here / "manifest.json").read_bytes()),
                    self.questions, self.dataset_bytes, capture_bytes,
                )
                self.assertTrue(report["qualified_against_frozen_tolerances"])
                self.assertEqual(report["cases"], 2)
                self.assertEqual([row["request_size_bytes"] for row in result["rows"]],
                                 [len(body) for body in expected])
                with self.assertRaisesRegex(ValueError, "original frozen Scout request capture"):
                    qualification.qualified_report(
                        result, manifest, qualification.sha256_bytes(
                            (here / "manifest.json").read_bytes()),
                        self.questions, self.dataset_bytes,
                    )
                altered = capture_bytes.replace(b"What changes?", b"What changed?")
                with self.assertRaisesRegex(ValueError, "original frozen Scout request capture"):
                    qualification.qualified_report(
                        result, manifest, qualification.sha256_bytes(
                            (here / "manifest.json").read_bytes()),
                        self.questions, self.dataset_bytes, altered,
                    )

    def test_capture_drift_and_invalid_contract_refuse_before_network(self):
        capture_bytes, manifest = self.captured_scout_fixture()
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            (here / "manifest.json").write_text(json.dumps(manifest))
            (here / "captured.json").write_bytes(capture_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            args = [
                "--url", self.endpoint, "--tag", "capture-refusal",
                "--server-config", str(here / "server.json"),
                "--qualification-manifest", str(here / "manifest.json"),
                "--captured-requests", str(here / "captured.json"),
                qualification.CAPTURED_SCOUT_CONFIG,
            ]
            records = qualification.loads_unambiguous_json(capture_bytes)
            invalid = []
            reordered = copy.deepcopy(records)
            reordered.reverse()
            invalid.append(("ordering", reordered))
            injected = copy.deepcopy(records)
            injected[0]["body"] = injected[0]["body"].replace(
                '"type":"noul"', '"type":"choice"', 1
            )
            invalid.append(("question contract", injected))
            extra = copy.deepcopy(records)
            extra[0]["body"] = extra[0]["body"].replace(
                '{"questions":', '{"debug":"injected","questions":', 1
            )
            invalid.append(("extra request field", extra))
            for name, changed in invalid:
                with self.subTest(name=name):
                    changed_bytes = json.dumps(changed, ensure_ascii=False).encode("utf-8")
                    with self.assertRaises(ValueError):
                        qualification.captured_request_bodies(
                            changed_bytes, ["holdout-positive", "holdout-negative"],
                            self.questions, None, manifest["max_request_bytes"],
                            manifest["max_state_bytes"],
                        )
            for name, changed in [
                ("changed text", capture_bytes.replace(b"What changes?", b"What changed?")),
                ("unexpected case", json.dumps(records[:-1]).encode("utf-8")),
            ]:
                with self.subTest(name=name):
                    (here / "captured.json").write_bytes(changed)
                    with patch.object(run_http, "HERE", str(here)):
                        with patch.object(run_http, "post") as post:
                            with self.assertRaises(ValueError):
                                run_http.main(args)
                            post.assert_not_called()
                    self.assertFalse((here / "results" /
                                      "capture-refusal_S_scout_captured.json.partial").exists())
            (here / "captured.json").write_bytes(capture_bytes)
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post") as post:
                    with self.assertRaisesRegex(ValueError, "--captured-requests"):
                        run_http.main(args[:-3] + [qualification.CAPTURED_SCOUT_CONFIG])
                    post.assert_not_called()

    def test_captured_question_order_cannot_be_changed_even_when_all_hashes_are_refrozen(self):
        # Dict equality ignores insertion order, but the model receives ordered
        # question bytes. Rehash every capture/manifest/result identity to
        # isolate the missing order check rather than a digest mismatch.
        capture, manifest = self.captured_scout_fixture()
        records = qualification.loads_unambiguous_json(capture)
        for record in records:
            body = qualification.loads_unambiguous_json(record["body"])
            body["questions"] = dict(reversed(list(body["questions"].items())))
            self.assertEqual(body["questions"], self.questions)
            self.assertNotEqual(list(body["questions"]), sorted(self.questions))
            record["body"] = json.dumps(body, ensure_ascii=False, separators=(",", ":"))
        reordered = json.dumps(records, ensure_ascii=False).encode("utf-8")
        manifest["captured_requests_sha256"] = qualification.sha256_bytes(reordered)
        manifest["holdout_requests_sha256"] = score.evidence_digest([
            [record["id"], qualification.sha256_bytes(record["body"].encode("utf-8"))]
            for record in records
        ])
        self.valid(manifest)
        with self.assertRaisesRegex(ValueError, "canonical Scout wire order"):
            qualification.captured_request_bodies(
                reordered, ["holdout-positive", "holdout-negative"],
                self.questions, None, manifest["max_request_bytes"],
                manifest["max_state_bytes"],
            )

        # Runner refusal precedes network traffic and evidence-file creation.
        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            manifest_bytes = json.dumps(manifest).encode("utf-8")
            (here / "manifest.json").write_bytes(manifest_bytes)
            (here / "server.json").write_text(json.dumps(self.server_config))
            (here / "captured.json").write_bytes(reordered)
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post") as post:
                    with self.assertRaisesRegex(ValueError, "question order"):
                        run_http.main([
                            "--url", self.endpoint, "--tag", "reordered-capture",
                            "--server-config", str(here / "server.json"),
                            "--qualification-manifest", str(here / "manifest.json"),
                            "--captured-requests", str(here / "captured.json"),
                            qualification.CAPTURED_SCOUT_CONFIG,
                        ])
                    post.assert_not_called()
            self.assertFalse((here / "results").exists())

        # An independently supplied result with matching rewritten request
        # fingerprints and experiment SHA must be refused by the scorer too.
        result = self.signed_result()
        for row, record in zip(result["rows"], records, strict=True):
            body_bytes = record["body"].encode("utf-8")
            row["request_sha256"] = qualification.sha256_bytes(body_bytes)
            row["request_size_bytes"] = len(body_bytes)
        result["config"] = "trial_" + qualification.CAPTURED_SCOUT_CONFIG
        experiment = result["experiment"]
        experiment["config"] = qualification.CAPTURED_SCOUT_CONFIG
        experiment["requests_sha256"] = manifest["holdout_requests_sha256"]
        experiment["qualification_manifest_sha256"] = qualification.sha256_bytes(manifest_bytes)
        result["experiment_sha256"] = score.evidence_digest(experiment)
        with self.assertRaisesRegex(ValueError, "question order"):
            qualification.qualified_report(
                result, manifest, qualification.sha256_bytes(manifest_bytes),
                self.questions, self.dataset_bytes, reordered,
            )

    def test_captured_state_budget_is_utf8_bytes_and_independently_checked(self):
        capture, manifest = self.captured_scout_fixture()
        records = qualification.loads_unambiguous_json(capture)
        # Two Unicode scalar values are four UTF-8 bytes, not two bytes.
        # Recompute every capture and request digest, so the refusal is
        # caused by the runtime state limit, not by stale provenance hashes.
        for record in records:
            body = qualification.loads_unambiguous_json(record["body"])
            body["state"] = "éé"
            record["body"] = json.dumps(
                body, sort_keys=True, ensure_ascii=False, separators=(",", ":"),
            )
        capture = json.dumps(records, ensure_ascii=False).encode("utf-8")
        manifest["captured_requests_sha256"] = qualification.sha256_bytes(capture)
        manifest["holdout_requests_sha256"] = score.evidence_digest([
            [record["id"], qualification.sha256_bytes(record["body"].encode("utf-8"))]
            for record in records
        ])
        for limit in (None, True, 0, "4096"):
            with self.subTest(invalid_limit=limit):
                invalid = copy.deepcopy(manifest)
                invalid["max_state_bytes"] = limit
                with self.assertRaisesRegex(ValueError, "max_state_bytes"):
                    self.valid(invalid)
        unexpected = copy.deepcopy(self.manifest)
        unexpected["max_state_bytes"] = 4096
        with self.assertRaisesRegex(ValueError, "only captured Scout"):
            self.valid(unexpected)

        manifest["max_state_bytes"] = 3
        self.valid(manifest)
        with self.assertRaisesRegex(ValueError, "state byte budget"):
            qualification.captured_request_bodies(
                capture, ["holdout-positive", "holdout-negative"],
                self.questions, None, manifest["max_request_bytes"],
                manifest["max_state_bytes"],
            )

        with tempfile.TemporaryDirectory() as temp:
            here = Path(temp)
            (here / "dataset.json").write_bytes(self.dataset_bytes)
            manifest_bytes = json.dumps(manifest).encode("utf-8")
            (here / "manifest.json").write_bytes(manifest_bytes)
            (here / "captured.json").write_bytes(capture)
            (here / "server.json").write_text(json.dumps(self.server_config))
            with patch.object(run_http, "HERE", str(here)):
                with patch.object(run_http, "post") as inference:
                    with self.assertRaisesRegex(ValueError, "state byte budget"):
                        run_http.main([
                            "--url", self.endpoint, "--tag", "oversized-state",
                            "--server-config", str(here / "server.json"),
                            "--qualification-manifest", str(here / "manifest.json"),
                            "--captured-requests", str(here / "captured.json"),
                            qualification.CAPTURED_SCOUT_CONFIG,
                        ])
                    inference.assert_not_called()
            self.assertFalse((here / "results").exists())

            result = self.signed_result()
            for row, record in zip(result["rows"], records, strict=True):
                encoded = record["body"].encode("utf-8")
                row["request_sha256"] = qualification.sha256_bytes(encoded)
                row["request_size_bytes"] = len(encoded)
            result["config"] = "trial_" + qualification.CAPTURED_SCOUT_CONFIG
            experiment = result["experiment"]
            experiment["config"] = qualification.CAPTURED_SCOUT_CONFIG
            experiment["requests_sha256"] = manifest["holdout_requests_sha256"]
            experiment["qualification_manifest_sha256"] = qualification.sha256_bytes(manifest_bytes)
            result["experiment_sha256"] = score.evidence_digest(experiment)
            with self.assertRaisesRegex(ValueError, "state byte budget"):
                qualification.qualified_report(
                    result, manifest, qualification.sha256_bytes(manifest_bytes),
                    self.questions, self.dataset_bytes, capture,
                )

        # Boundary acceptance is equally important: the runtime would send
        # this four-byte state unchanged with a four-byte budget.
        manifest["max_state_bytes"] = 4
        self.valid(manifest)
        accepted = qualification.captured_request_bodies(
            capture, ["holdout-positive", "holdout-negative"],
            self.questions, None, manifest["max_request_bytes"],
            manifest["max_state_bytes"],
        )
        self.assertEqual(len(accepted), 2)
        self.assertEqual(
            qualification.loads_unambiguous_json(accepted["holdout-positive"])["state"],
            "éé",
        )

    def test_noncanonical_wire_bytes_refuse_even_after_rehashing_all_evidence(self):
        # A syntactically different request can decode to identical Noul
        # semantics, yet cannot be the original Rust send_json wire payload.
        # Freeze new capture/holdout/result digests to bypass hash-mismatch
        # guards and exercise the actual production-wire-shape check.
        capture, _ = self.captured_scout_fixture()
        original = qualification.loads_unambiguous_json(capture)
        for encoding in ("top_level_order", "json_spacing"):
            with self.subTest(encoding=encoding):
                records = copy.deepcopy(original)
                for record in records:
                    decoded = qualification.loads_unambiguous_json(record["body"])
                    if encoding == "top_level_order":
                        changed = json.dumps(
                            {"state": decoded["state"], "questions": decoded["questions"]},
                            ensure_ascii=False, separators=(",", ":"),
                        )
                    else:
                        changed = json.dumps(decoded, sort_keys=True, ensure_ascii=False)
                    self.assertEqual(qualification.loads_unambiguous_json(changed), decoded)
                    self.assertNotEqual(changed, record["body"])
                    record["body"] = changed
                altered = json.dumps(records, ensure_ascii=False).encode("utf-8")
                _, manifest = self.captured_scout_fixture()
                manifest["captured_requests_sha256"] = qualification.sha256_bytes(altered)
                manifest["holdout_requests_sha256"] = score.evidence_digest([
                    [record["id"], qualification.sha256_bytes(record["body"].encode("utf-8"))]
                    for record in records
                ])
                self.valid(manifest)
                with self.assertRaisesRegex(ValueError, "canonical Rust JSON wire bytes"):
                    qualification.captured_request_bodies(
                        altered, ["holdout-positive", "holdout-negative"],
                        self.questions, None, manifest["max_request_bytes"],
                        manifest["max_state_bytes"],
                    )

                with tempfile.TemporaryDirectory() as temp:
                    here = Path(temp)
                    (here / "dataset.json").write_bytes(self.dataset_bytes)
                    manifest_bytes = json.dumps(manifest).encode("utf-8")
                    (here / "manifest.json").write_bytes(manifest_bytes)
                    (here / "server.json").write_text(json.dumps(self.server_config))
                    (here / "captured.json").write_bytes(altered)
                    with patch.object(run_http, "HERE", str(here)):
                        with patch.object(run_http, "post") as post:
                            with self.assertRaisesRegex(ValueError, "canonical Rust JSON wire bytes"):
                                run_http.main([
                                    "--url", self.endpoint, "--tag", "noncanonical",
                                    "--server-config", str(here / "server.json"),
                                    "--qualification-manifest", str(here / "manifest.json"),
                                    "--captured-requests", str(here / "captured.json"),
                                    qualification.CAPTURED_SCOUT_CONFIG,
                                ])
                            post.assert_not_called()
                    self.assertFalse((here / "results").exists())

                    result = self.signed_result()
                    for row, record in zip(result["rows"], records, strict=True):
                        raw = record["body"].encode("utf-8")
                        row["request_sha256"] = qualification.sha256_bytes(raw)
                        row["request_size_bytes"] = len(raw)
                    result["config"] = "trial_" + qualification.CAPTURED_SCOUT_CONFIG
                    experiment = result["experiment"]
                    experiment["config"] = qualification.CAPTURED_SCOUT_CONFIG
                    experiment["requests_sha256"] = manifest["holdout_requests_sha256"]
                    experiment["qualification_manifest_sha256"] = qualification.sha256_bytes(
                        manifest_bytes
                    )
                    result["experiment_sha256"] = score.evidence_digest(experiment)
                    with self.assertRaisesRegex(ValueError, "canonical Rust JSON wire bytes"):
                        qualification.qualified_report(
                            result, manifest,
                            qualification.sha256_bytes(manifest_bytes),
                            self.questions, self.dataset_bytes, altered,
                        )


if __name__ == "__main__":
    unittest.main()
