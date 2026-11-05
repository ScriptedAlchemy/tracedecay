"""Behavioral coverage for sparse evaluation slices and their CLI reports."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

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


def signed_http_result(config, rows, temperature=None):
    # Fixture independently reproduces the fingerprint scheme in run_http.py.
    rows = [
        {**item, "request_sha256": hashlib.sha256(item["id"].encode()).hexdigest()}
        for item in rows
    ]

    def fingerprint(value):
        return hashlib.sha256(
            json.dumps(value, sort_keys=True, allow_nan=False).encode("utf-8")
        ).hexdigest()

    experiment = {
        "version": 2, "config": config, "dataset_sha256": "a" * 64,
        "case_ids": [item["id"] for item in rows],
        "requests_sha256": fingerprint(
            [[item["id"], item["request_sha256"]] for item in rows]
        ),
        "endpoint_sha256": "b" * 64, "requested_model": None,
        "noul_temperature": temperature,
        "server_config": {"checkpoint": "weights@immutable", "server_revision": "server@one",
                          "backend": "cpu", "dtype": "float32", "temperature": 1.0},
    }
    return {
        "config": config, "experiment": experiment,
        "experiment_sha256": fingerprint(experiment),
        "dataset_sha256": experiment["dataset_sha256"],
        "noul_temperature": temperature, "rows": rows,
    }


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

    def test_ranked_auc_matches_exhaustive_pairwise_ties(self):
        for pos, neg in (
            ([], []), ([0.5], []), ([], [0.5]),
            ([0.5], [0.5]), ([0.0, 0.5, 1.0], [1.0, 0.5, 0.0]),
            ([0.1, 0.1, 0.4, 0.8, 1.0], [0.0, 0.1, 0.3, 0.8, 0.8]),
        ):
            with self.subTest(pos=pos, neg=neg):
                expected = (sum((p > n) + 0.5 * (p == n)
                                for p in pos for n in neg) / (len(pos) * len(neg))
                            if pos and neg else None)
                self.assertEqual(score.auc(pos, neg), expected)

    def test_leave_one_out_matches_independent_training_rescans(self):
        levels = [0, 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0]
        for count in (2, 3, 11, 24):
            rows = []
            for index in range(count):
                labels = [cat for j, cat in enumerate(score.CATS)
                          if (index + 3 * j) % 5 == 0]
                item = row(str(index), labels, index % 2 == 0)
                item["probs"] = {
                    cat: levels[(3 * index + 7 * j) % len(levels)]
                    for j, cat in enumerate(score.CATS)
                }
                rows.append(item)
            actual = score.score_config({"config": "loo-check", "rows": rows})
            self.assertEqual(actual["loo_evaluated_cases"], count)
            for category in score.CATS:
                tp = fp = fn = 0
                for index, item in enumerate(rows):
                    train = rows[:index] + rows[index + 1:]
                    threshold = max(
                        score.TH,
                        key=lambda t: (score.prf(train, category, t)["f1"] or 0, t),
                    )
                    hit = item["probs"][category] >= threshold
                    positive = category in item["labels"]
                    tp += hit and positive
                    fp += hit and not positive
                    fn += not hit and positive
                expected = {
                    "loo_f1": 2 * tp / (2 * tp + fp + fn) if tp + fp + fn else None,
                    "loo_p": tp / (tp + fp) if tp + fp else None,
                    "loo_r": tp / (tp + fn) if tp + fn else None,
                }
                for metric, value in expected.items():
                    self.assertEqual(actual["per_cat"][category][metric], value)

    def test_scoring_large_held_out_cohort_without_pairwise_rescans(self):
        rows = [
            row(str(i), [score.CATS[0]] if i % 2 else [], bool(i % 3))
            for i in range(1200)
        ]
        result = score.score_config({"config": "large", "rows": rows})
        self.assertEqual(result["loo_evaluated_cases"], len(rows))
        self.assertEqual(result["latency"]["total_cases"], len(rows))
        self.assertEqual(result["per_cat"][score.CATS[0]]["auc"], 1.0)

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


    def test_prior_http_telemetry_is_not_scored_as_measured_usage(self):
        for experiment in ({"version": 1}, {"version": 3}, {}, None):
            with self.subTest(experiment=experiment):
                with self.assertRaisesRegex(ValueError, "telemetry version"):
                    score.score_config({
                        "config": "old-http", "experiment": experiment,
                        "rows": [row("a")],
                    })

    def test_partial_usage_reports_observed_coverage_not_invented_zeroes(self):
        rows = [
            {**row("missing"), "input_tokens": 12, "state_tokens": None, "dropped": None},
            {**row("truncated"), "input_tokens": 100, "state_tokens": 40, "dropped": 2},
            {**row("zero"), "input_tokens": None, "state_tokens": 0, "dropped": 0},
        ]
        timing = score.score_config({"config": "partial", "rows": rows})["latency"]
        self.assertEqual(timing["total_cases"], 3)
        self.assertEqual(timing["median_state_tokens"], 20)
        self.assertEqual(timing["max_state_tokens"], 40)
        self.assertEqual(timing["state_tokens_measured_cases"], 2)
        self.assertEqual(timing["median_input_tokens"], 56)
        self.assertEqual(timing["max_input_tokens"], 100)
        self.assertEqual(timing["input_tokens_measured_cases"], 2)
        self.assertEqual(timing["truncation_measured_cases"], 2)
        self.assertEqual(timing["observed_cases_truncated"], 1)
        self.assertIsNone(timing["cases_truncated"])
        self.assertEqual(json.loads(json.dumps(timing, allow_nan=False)), timing)

    def test_absent_usage_remains_unknown_in_cli_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            (root / "results" / "unknown.json").write_text(json.dumps({
                "config": "unknown",
                "rows": [{**row("a"), "state_tokens": None, "input_tokens": None, "dropped": None}],
            }), encoding="utf-8")
            scored = self.run_script(root, "score.py")
            self.assertEqual(scored.returncode, 0, scored.stderr)
            timing = json.loads((root / "summary.json").read_text())["unknown"]["latency"]
            for field in ("state_tokens", "input_tokens"):
                self.assertIsNone(timing[f"median_{field}"])
                self.assertIsNone(timing[f"max_{field}"])
                self.assertEqual(timing[f"{field}_measured_cases"], 0)
            self.assertIsNone(timing["cases_truncated"])
            self.assertEqual(timing["truncation_measured_cases"], 0)
            self.assertEqual(timing["observed_cases_truncated"], 0)
            compared = self.run_script(root, "compare.py", "unknown")
            self.assertEqual(compared.returncode, 0, compared.stderr)

    def test_result_rows_require_valid_predictions_and_verifiable_case_labels(self):
        original = row("a", [score.CATS[0]])
        cases = [
            ({"probs": {**original["probs"], score.CATS[0]: True}}, "probs value"),
            ({"probs": {**original["probs"], score.CATS[0]: float("nan")}}, "probs value"),
            ({"probs": {**original["probs"], score.CATS[0]: float("inf")}}, "probs value"),
            ({"probs": {**original["probs"], score.CATS[0]: -0.1}}, "probs value"),
            ({"probs": {**original["probs"], score.CATS[0]: 1.1}}, "probs value"),
            ({"probs": {k: v for k, v in original["probs"].items() if k != score.CATS[0]}}, "probs coverage"),
            ({"probs": {**original["probs"], "unexpected": 0.2}}, "probs coverage"),
            ({"probs_raw": {**original["probs"], score.CATS[0]: False}}, "probs_raw value"),
            ({"labels": ["not_an_intent"]}, "invalid labels"),
            ({"labels": [score.CATS[0], score.CATS[0]]}, "invalid labels"),
            ({"context_needed": "true"}, "context_needed"),
            ({"choice": "not_an_option"}, "invalid choice"),
            ({"ms": True}, "elapsed ms"),
            ({"ms": float("nan")}, "elapsed ms"),
        ]
        for changed, message in cases:
            with self.subTest(changed=changed):
                with self.assertRaisesRegex(ValueError, message):
                    score.score_config({"config": "invalid", "rows": [{**original, **changed}]})
        with self.assertRaisesRegex(ValueError, "duplicate case ID"):
            score.score_config({"config": "duplicate", "rows": [original, dict(original)]})
        choice = {**original, "choice": "none",
                  "probs": {**original["probs"], "none": 0.2}}
        self.assertEqual(score.score_config({"config": "choice", "rows": [choice]})["latency"]["total_cases"], 1)

    def test_http_calibration_must_match_raw_evidence(self):
        original = row("a", [score.CATS[0]])
        raw = dict(original["probs"])
        temperature = 2.0
        measured = {cat: score.calibrated(p, temperature) for cat, p in raw.items()}
        calibrated_row = {**original, "probs_raw": raw, "probs": measured}
        result = signed_http_result("http_calibrated", [calibrated_row], temperature)
        self.assertEqual(score.score_config(result)["latency"]["total_cases"], 1)

        conflicting = {**result["rows"][0], "probs": raw}
        with self.assertRaisesRegex(ValueError, "calibrated probabilities disagree"):
            score.score_config({**result, "rows": [conflicting]})
        missing_raw = {k: v for k, v in result["rows"][0].items() if k != "probs_raw"}
        with self.assertRaisesRegex(ValueError, "missing raw probabilities"):
            score.score_config({**result, "rows": [missing_raw]})
        with self.assertRaisesRegex(ValueError, "temperature differs"):
            score.score_config({**result, "noul_temperature": 3.0})
        with self.assertRaisesRegex(ValueError, "invalid HTTP calibration temperature"):
            score.score_config({
                **result, "experiment": {"version": 2, "noul_temperature": 0.0},
                "noul_temperature": 0.0,
            })
        with self.assertRaisesRegex(ValueError, "uncalibrated HTTP result"):
            score.score_config({
                **result, "experiment": {"version": 2, "noul_temperature": None},
                "noul_temperature": None,
            })

        # A corrupt calibrated result must not replace a previous scored summary.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            (root / "results" / "bad.json").write_text(
                json.dumps({**result, "rows": [conflicting]}), encoding="utf-8",
            )
            summary = root / "summary.json"
            summary.write_bytes(b"last verified summary\n")
            attempt = self.run_script(root, "score.py")
            self.assertNotEqual(attempt.returncode, 0)
            self.assertIn("calibrated probabilities disagree", attempt.stderr)
            self.assertEqual(summary.read_bytes(), b"last verified summary\n")

    def test_http_provenance_cannot_be_downgraded_to_local_scoring(self):
        original = signed_http_result("B_noul_text", [row("first"), row("second")])
        self.assertEqual(score.score_config(original)["latency"]["total_cases"], 2)
        for missing in (
            ("experiment",),
            ("experiment", "experiment_sha256"),
        ):
            with self.subTest(missing=missing):
                damaged = json.loads(json.dumps(original))
                for field in missing:
                    del damaged[field]
                with self.assertRaisesRegex(ValueError, "missing its experiment identity"):
                    score.score_config(damaged)

        # Local evaluations record dataset_sha256 too; preserve their format.
        local = {
            "config": "local", "dataset_sha256": "a" * 64,
            "rows": [row("local")],
        }
        self.assertEqual(score.score_config(local)["latency"]["total_cases"], 1)

        # Failed HTTP provenance checks cannot overwrite a scored snapshot.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            damaged = json.loads(json.dumps(original))
            del damaged["experiment"]
            del damaged["experiment_sha256"]
            (root / "results" / "damaged.json").write_text(
                json.dumps(damaged), encoding="utf-8",
            )
            summary = root / "summary.json"
            summary.write_bytes(b"last verified summary\n")
            attempt = self.run_script(root, "score.py")
            self.assertNotEqual(attempt.returncode, 0)
            self.assertIn("missing its experiment identity", attempt.stderr)
            self.assertEqual(summary.read_bytes(), b"last verified summary\n")

    def test_http_result_fingerprints_bind_case_order_and_experiment(self):
        original = signed_http_result("B_noul_text", [row("first"), row("second")])
        self.assertEqual(score.score_config(original)["latency"]["total_cases"], 2)
        for mutation, diagnostic in [
            ("identity", "identity digest"),
            ("missing_digest", "identity digest"),
            ("request", "request fingerprints"),
            ("case_order", "case IDs or ordering"),
            ("dataset", "dataset digest"),
            ("config", "configuration disagrees"),
        ]:
            changed = json.loads(json.dumps(original))
            if mutation == "identity":
                changed["experiment"]["server_config"]["checkpoint"] = "different"
            elif mutation == "missing_digest":
                del changed["experiment_sha256"]
            elif mutation == "request":
                changed["rows"][0]["request_sha256"] = "c" * 64
            elif mutation == "case_order":
                changed["rows"].reverse()
            elif mutation == "dataset":
                changed["dataset_sha256"] = "f" * 64
            elif mutation == "config":
                changed["config"] = "different"
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ValueError, diagnostic):
                    score.score_config(changed)

        # An altered completed result cannot destroy the last scored summary.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            changed = json.loads(json.dumps(original))
            changed["rows"][1]["request_sha256"] = "c" * 64
            (root / "results" / "bad.json").write_text(
                json.dumps(changed), encoding="utf-8"
            )
            summary = root / "summary.json"
            summary.write_bytes(b"last verified summary\n")
            attempt = self.run_script(root, "score.py")
            self.assertNotEqual(attempt.returncode, 0)
            self.assertIn("request fingerprints", attempt.stderr)
            self.assertEqual(summary.read_bytes(), b"last verified summary\n")

    def test_invalid_probability_cannot_replace_previous_cli_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            (root / "results").mkdir()
            invalid = row("a", [score.CATS[0]])
            invalid["probs"][score.CATS[0]] = True
            (root / "results" / "bad.json").write_text(
                json.dumps({"config": "bad", "rows": [invalid]}), encoding="utf-8",
            )
            summary = root / "summary.json"
            summary.write_bytes(b"previous valid summary\n")
            result = self.run_script(root, "score.py")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid probs value", result.stderr)
            self.assertEqual(summary.read_bytes(), b"previous valid summary\n")

    def test_invalid_usage_is_not_silently_discarded_as_missing(self):
        for field in ("state_tokens", "input_tokens", "dropped"):
            for value in (True, -1, 1.5, "12", float("nan")):
                with self.subTest(field=field, value=value):
                    with self.assertRaisesRegex(ValueError, field):
                        score.lat([{**row("invalid"), field: value}])

    def test_cli_does_not_invent_an_unmeasured_hybrid_arm(self):
        for same_cases in (True, False):
            with self.subTest(same_cases=same_cases), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.stage_scripts(root)
                results = root / "results"
                results.mkdir()
                configurations = {
                    "B_noul_text": {"config": "B_noul_text", "rows": [row("a")]},
                    "C_choice_text": {
                        "config": "C_choice_text",
                        "rows": [row("a" if same_cases else "b", choice="none")],
                    },
                }
                for name, result in configurations.items():
                    (results / f"{name}.json").write_text(json.dumps(result), encoding="utf-8")
                scored = self.run_script(root, "score.py")
                self.assertEqual(scored.returncode, 0, scored.stderr)
                summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
                self.assertEqual(set(summary), set(configurations))
                for name, result in configurations.items():
                    self.assertEqual(summary[name], score.score_config(result))
                self.assertNotIn("H_hybrid_text", scored.stdout)

    def test_cli_preserves_a_recorded_hybrid_instead_of_replacing_its_measurements(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            results = root / "results"
            results.mkdir()
            hybrid = signed_http_result("H_hybrid_text", [{
                **row("a", [score.CATS[0]], True), "ms": 17,
                "input_tokens": 151, "state_tokens": 93, "dropped": 4,
            }])
            configurations = [
                {"config": "B_noul_text", "rows": [row("a")]},
                {"config": "C_choice_text", "rows": [row("a", choice="none")]},
                hybrid,
            ]
            for result in configurations:
                (results / f"{result['config']}.json").write_text(
                    json.dumps(result), encoding="utf-8",
                )
            original_inputs = {path: path.read_bytes() for path in results.iterdir()}
            scored = self.run_script(root, "score.py")
            self.assertEqual(scored.returncode, 0, scored.stderr)
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(summary["H_hybrid_text"], score.score_config(hybrid))
            self.assertEqual({path: path.read_bytes() for path in results.iterdir()}, original_inputs)
            compared = self.run_script(root, "compare.py", "H_hybrid_text")
            self.assertEqual(compared.returncode, 0, compared.stderr)
            self.assertIn("| median ms / call | 17 |", compared.stdout)

    def test_duplicate_configuration_files_are_refused_before_summary_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.stage_scripts(root)
            results = root / "results"
            results.mkdir()
            for name, ms in [("first", 10), ("second", 900)]:
                (results / f"{name}.json").write_text(json.dumps({
                    "config": "same", "rows": [{**row(name), "ms": ms}],
                }), encoding="utf-8")
            summary = root / "summary.json"
            summary.write_bytes(b"existing summary\n")
            scored = self.run_script(root, "score.py")
            self.assertNotEqual(scored.returncode, 0)
            self.assertIn("duplicate configuration", scored.stderr)
            self.assertIn("same", scored.stderr)
            self.assertEqual(summary.read_bytes(), b"existing summary\n")

    def test_summary_publication_replaces_only_a_complete_scored_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary.json"
            previous = b'{"prior": true}\n'
            summary.write_bytes(previous)
            expected = {"measured": {"macro_f1": 0.91}}
            replace = score.os.replace

            def inspect_replace(staged, destination):
                self.assertEqual(summary.read_bytes(), previous)
                self.assertEqual(
                    json.loads(Path(staged).read_text(encoding="utf-8")), expected
                )
                replace(staged, destination)

            with mock.patch.object(score.os, "replace", side_effect=inspect_replace):
                score.publish_summary(summary, expected)
            self.assertEqual(json.loads(summary.read_text(encoding="utf-8")), expected)
            self.assertEqual(set(Path(directory).iterdir()), {summary})

    def test_summary_publication_errors_preserve_previous_bytes_and_remove_staging(self):
        for failure in ("serialize", "fsync", "replace"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                summary = Path(directory) / "summary.json"
                previous = b'{"completed": true}\n'
                summary.write_bytes(previous)
                if failure == "serialize":
                    with self.assertRaisesRegex(ValueError, "Out of range"):
                        score.publish_summary(summary, {"invalid": float("nan")})
                elif failure == "fsync":
                    with mock.patch.object(
                        score.os, "fsync", side_effect=OSError("sync failed")
                    ):
                        with self.assertRaisesRegex(OSError, "sync failed"):
                            score.publish_summary(summary, {"new": 1})
                else:
                    with mock.patch.object(
                        score.os, "replace", side_effect=PermissionError("publish denied")
                    ):
                        with self.assertRaisesRegex(PermissionError, "publish denied"):
                            score.publish_summary(summary, {"new": 1})
                self.assertEqual(summary.read_bytes(), previous)
                self.assertEqual(set(Path(directory).iterdir()), {summary})

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
