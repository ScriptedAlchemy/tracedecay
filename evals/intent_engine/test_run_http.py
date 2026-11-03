"""Runner persistence tests. HTTP inference is substituted, not qualified here."""
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_http
import score


class RunHttpTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dataset = [
            {"id": "first", "labels": ["impact"], "context_needed": True,
             "current_prompt": "What breaks?", "turns": [{"role": "user", "text": "Change the API."}]},
            {"id": "second", "labels": [], "context_needed": False,
             "current_prompt": "Hello", "turns": []},
        ]
        self.write("dataset.json", self.dataset)
        self.server = {"checkpoint": "weights@immutable-revision", "server_revision": "server@rev1",
                       "backend": "cpu", "dtype": "float32", "temperature": 1.0}
        self.write("server.json", self.server)
        self.base = ["--url", "http://localhost:8009/v1/systemone", "--tag", "probe",
                     "--server-config", str(self.root / "server.json")]
        self.config = "B_noul_text"
        self.output = self.root / "results" / f"probe_{self.config}.json"
        self.partial = Path(str(self.output) + ".partial")
        self.addCleanup(patch.stopall)
        patch.object(run_http, "HERE", str(self.root)).start()

    def write(self, name, value):
        (self.root / name).write_text(json.dumps(value), encoding="utf-8")

    def response(self, url, body):
        return {"answers": {key: {"noul": 0.25} for key in body["questions"]},
                "usage": {"input_tokens": 12}, "latency_ms": 0}, 5.0

    def run_main(self, extra=()):
        with contextlib.redirect_stdout(io.StringIO()):
            run_http.main(self.base + list(extra) + [self.config])

    def interrupt(self, extra=()):
        count = 0
        def inference(url, body):
            nonlocal count
            count += 1
            if count == 2:
                raise OSError("interrupted")
            return self.response(url, body)
        with patch.object(run_http, "post", side_effect=inference):
            with self.assertRaisesRegex(OSError, "interrupted"):
                self.run_main(extra)
        self.assertTrue(self.partial.exists())
        self.assertFalse(self.output.exists())

    def assert_refused_unchanged(self, extra=()):
        before = self.partial.read_bytes()
        with patch.object(run_http, "post") as post:
            with self.assertRaises(ValueError):
                self.run_main(extra)
            post.assert_not_called()
        self.assertEqual(self.partial.read_bytes(), before)
        self.assertFalse(self.output.exists())

    def test_exact_resume_only_requests_the_missing_case(self):
        self.interrupt()
        with patch.object(run_http, "post", side_effect=self.response) as post:
            self.run_main()
        self.assertEqual(post.call_count, 1)
        self.assertEqual(post.call_args.args[1]["state"], "User's latest message: Hello")
        result = json.loads(self.output.read_text())
        self.assertEqual([row["id"] for row in result["rows"]], ["first", "second"])
        self.assertEqual(result["experiment"]["server_config"], self.server)
        self.assertEqual(result["experiment_sha256"], run_http.digest(result["experiment"]))
        self.assertEqual(result["rows"][0]["server_ms"], 0)
        self.assertFalse(self.partial.exists())


    def test_input_tokens_do_not_fabricate_state_or_truncation_measurements(self):
        self.interrupt()
        with patch.object(run_http, "post", side_effect=self.response):
            self.run_main()
        result = json.loads(self.output.read_text())
        self.assertEqual(result["experiment"]["version"], 2)
        for row in result["rows"]:
            self.assertEqual(row["input_tokens"], 12)
            self.assertIsNone(row["state_tokens"])
            self.assertIsNone(row["dropped"])
        timing = score.score_config(result)["latency"]
        self.assertEqual(timing["median_input_tokens"], 12)
        self.assertEqual(timing["input_tokens_measured_cases"], 2)
        self.assertIsNone(timing["median_state_tokens"])
        self.assertIsNone(timing["cases_truncated"])
        self.assertEqual(timing["truncation_measured_cases"], 0)

    def test_reported_usage_and_zero_counts_survive_resume(self):
        response, elapsed = self.response("", {"questions": run_http.noul_questions()})
        response["usage"] = {"input_tokens": 12, "state_tokens": 8, "state_tokens_dropped": 3}
        with patch.object(self, "response", return_value=(response, elapsed)):
            self.interrupt()
        response = copy.deepcopy(response)
        response["usage"] = {"input_tokens": 0, "state_tokens": 0, "state_tokens_dropped": 0}
        with patch.object(run_http, "post", return_value=(response, elapsed)) as post:
            self.run_main()
        post.assert_called_once()
        result = json.loads(self.output.read_text())
        self.assertEqual(
            [(row["input_tokens"], row["state_tokens"], row["dropped"]) for row in result["rows"]],
            [(12, 8, 3), (0, 0, 0)],
        )
        timing = score.score_config(result)["latency"]
        self.assertEqual(timing["median_state_tokens"], 4)
        self.assertEqual(timing["median_input_tokens"], 6)
        self.assertEqual(timing["truncation_measured_cases"], 2)
        self.assertEqual(timing["cases_truncated"], 1)

    def test_missing_usage_survives_resume_and_scoring(self):
        response, elapsed = self.response("", {"questions": run_http.noul_questions()})
        response["usage"] = None
        with patch.object(self, "response", return_value=(response, elapsed)):
            self.interrupt()
        del response["usage"]
        with patch.object(run_http, "post", return_value=(response, elapsed)) as post:
            self.run_main()
        post.assert_called_once()
        result = json.loads(self.output.read_text())
        for row in result["rows"]:
            for field in ("input_tokens", "state_tokens", "dropped"):
                self.assertIsNone(row[field], field)
        timing = score.score_config(result)["latency"]
        self.assertEqual(timing["total_cases"], 2)
        self.assertEqual(timing["median_ms"], 5)
        self.assertEqual(timing["state_tokens_measured_cases"], 0)
        self.assertEqual(timing["input_tokens_measured_cases"], 0)
        self.assertEqual(timing["truncation_measured_cases"], 0)
        self.assertIsNone(timing["cases_truncated"])

    def test_resume_refuses_prior_telemetry_contract(self):
        self.interrupt()
        header, row = map(json.loads, self.partial.read_text().splitlines())
        header["experiment"]["version"] = 1
        header["experiment_sha256"] = run_http.digest(header["experiment"])
        # Old logs mislabeled input tokens and invented zero dropped tokens.
        row.pop("input_tokens", None)
        row.update(state_tokens=12, dropped=0)
        self.partial.write_text(json.dumps(header) + "\n" + json.dumps(row) + "\n")
        self.assert_refused_unchanged()

    def test_malformed_usage_is_refused_without_writes(self):
        self.interrupt()
        before = self.partial.read_bytes()
        for usage in ([], "unknown", 0, False):
            response, elapsed = self.response("", {"questions": run_http.noul_questions()})
            response["usage"] = usage
            with self.subTest(usage=usage):
                with patch.object(run_http, "post", return_value=(response, elapsed)):
                    with self.assertRaisesRegex(ValueError, "usage"):
                        self.run_main()
                self.assertEqual(self.partial.read_bytes(), before)
                self.assertFalse(self.output.exists())

    def test_model_endpoint_and_calibration_changes_refuse_resume(self):
        self.interrupt()
        for extra in (["--send-model", "another-model"], ["--noul-temperature", "2"],
                      ["--url", "http://localhost:8010/v1/systemone"]):
            with self.subTest(extra=extra):
                self.assert_refused_unchanged(extra)

    def test_serving_identity_changes_refuse_resume(self):
        self.interrupt()
        for field, value in (("checkpoint", "other-weights"), ("server_revision", "rev2"),
                             ("backend", "gpu"), ("dtype", "float16"), ("temperature", 2.0)):
            with self.subTest(field=field):
                self.write("server.json", {**self.server, field: value})
                self.assert_refused_unchanged()
        self.write("server.json", self.server)

    def test_dataset_and_label_changes_refuse_resume(self):
        self.interrupt()
        for field, value in (("current_prompt", "Changed prompt"), ("labels", []),
                             ("context_needed", False)):
            with self.subTest(field=field):
                dataset = copy.deepcopy(self.dataset)
                dataset[0][field] = value
                self.write("dataset.json", dataset)
                self.assert_refused_unchanged()
        self.write("dataset.json", self.dataset)

    def test_subset_content_is_bound_not_just_its_filename(self):
        self.write("subset.json", {"ids": ["first", "second"]})
        extra = ["--subset", str(self.root / "subset.json")]
        self.interrupt(extra)
        self.write("subset.json", {"ids": ["first"]})
        self.assert_refused_unchanged(extra)

    def test_questions_and_renderer_output_are_bound(self):
        self.interrupt()
        questions = run_http.noul_questions()
        questions["impact"]["instructions"] = "Different instructions"
        with patch.object(run_http, "noul_questions", return_value=questions):
            self.assert_refused_unchanged()
        with patch.dict(run_http.STATES, {self.config: lambda case: "different state"}):
            self.assert_refused_unchanged()

    def test_question_order_is_part_of_the_http_request_identity(self):
        self.interrupt()
        reordered = dict(reversed(list(run_http.noul_questions().items())))
        with patch.object(run_http, "noul_questions", return_value=reordered):
            self.assert_refused_unchanged()

    def test_headerless_partial_is_never_imported_as_current_evidence(self):
        self.partial.parent.mkdir()
        self.partial.write_text(json.dumps({"id": "first"}) + "\n")
        self.assert_refused_unchanged()

    def test_invalid_partial_rows_are_rejected_before_network_or_writes(self):
        self.interrupt()
        original = self.partial.read_text()
        lines = original.splitlines()
        row = json.loads(lines[1])
        bad_rows = [
            {**row, "id": "foreign"}, {**row, "request_sha256": "wrong"},
            {**row, "labels": []}, {**row, "probs": {}},
            {key: value for key, value in row.items() if key != "ms"},
            {key: value for key, value in row.items() if key != "input_tokens"},
        ]
        for bad in bad_rows:
            with self.subTest(row=bad):
                self.partial.write_text(lines[0] + "\n" + json.dumps(bad) + "\n")
                self.assert_refused_unchanged()
        for contents in (original + lines[1] + "\n", original.rstrip("\n"), original + "{broken\n"):
            with self.subTest(contents=contents[-40:]):
                self.partial.write_text(contents)
                self.assert_refused_unchanged()

    def test_resume_rejects_invalid_metrics_before_network_or_publication(self):
        self.interrupt()
        header, saved = self.partial.read_text().splitlines()
        row = json.loads(saved)
        for field, value in (
            ("ms", float("nan")), ("server_ms", float("inf")),
            ("ms", -1), ("ms", "5"), ("server_ms", False),
            ("state_tokens", "12"), ("state_tokens", 1.5),
            ("state_tokens", True), ("dropped", -1), ("dropped", "0"),
            ("input_tokens", -1), ("input_tokens", True), ("input_tokens", 1.5),
            ("extra", {"nonfinite": float("nan")}),
        ):
            with self.subTest(field=field, value=value):
                invalid = {**row, field: value}
                self.partial.write_text(header + "\n" + json.dumps(invalid) + "\n")
                self.assert_refused_unchanged()

    def test_new_invalid_metrics_do_not_publish_samples_or_completed_rows(self):
        self.interrupt()
        before = self.partial.read_bytes()
        sample = self.root / "first-response.json"
        for field, value in (("latency_ms", -1), ("latency_ms", "0"),
                             ("latency_ms", True), ("input_tokens", -1),
                             ("input_tokens", 1.5), ("input_tokens", False),
                             ("state_tokens", -1), ("state_tokens", "12"),
                             ("state_tokens_dropped", True), ("state_tokens_dropped", -1)):
            response, elapsed = self.response("", {"questions": run_http.noul_questions()})
            if field in ("input_tokens", "state_tokens", "state_tokens_dropped"):
                response["usage"][field] = value
            else:
                response[field] = value
            with self.subTest(field=field, value=value):
                with patch.object(run_http, "post", return_value=(response, elapsed)):
                    with self.assertRaises(ValueError):
                        self.run_main(["--sample-out", str(sample)])
                self.assertEqual(self.partial.read_bytes(), before)
                self.assertFalse(self.output.exists())
                self.assertFalse(sample.exists())

    def test_unknown_server_latency_and_zero_token_count_resume(self):
        self.interrupt()
        header, saved = self.partial.read_text().splitlines()
        row = json.loads(saved)
        row.update(server_ms=None, state_tokens=0, dropped=0)
        self.partial.write_text(header + "\n" + json.dumps(row) + "\n")
        with patch.object(run_http, "post", side_effect=self.response) as post:
            self.run_main()
        post.assert_called_once()
        result = json.loads(self.output.read_text())
        self.assertIsNone(result["rows"][0]["server_ms"])
        self.assertEqual(result["rows"][0]["state_tokens"], 0)
        self.assertFalse(self.partial.exists())

    def test_calibrated_resume_requires_verifiable_raw_probabilities(self):
        extra = ["--noul-temperature", "2"]
        self.interrupt(extra)
        header, saved = self.partial.read_text().splitlines()
        original = json.loads(saved)
        for defect in ("missing", "wrong", "extra", "calibrated"):
            with self.subTest(defect=defect):
                row = copy.deepcopy(original)
                if defect == "missing":
                    del row["probs_raw"]
                elif defect == "wrong":
                    row["probs_raw"]["impact"] = 0.9
                elif defect == "extra":
                    row["probs_raw"]["unrequested"] = 0.5
                else:
                    row["probs"]["impact"] = 0.99
                self.partial.write_text(header + "\n" + json.dumps(row) + "\n")
                self.assert_refused_unchanged(extra)
        # Unmodified calibrated evidence can still resume the missing case.
        self.partial.write_text(header + "\n" + saved + "\n")
        with patch.object(run_http, "post", side_effect=self.response) as post:
            self.run_main(extra)
        self.assertEqual(post.call_count, 1)
        result = json.loads(self.output.read_text())
        self.assertEqual(len(result["rows"]), 2)
        self.assertEqual(result["rows"][0]["probs_raw"], original["probs_raw"])

    def test_uncalibrated_resume_rejects_fabricated_raw_probabilities(self):
        self.interrupt()
        header, saved = self.partial.read_text().splitlines()
        row = json.loads(saved)
        row["probs_raw"] = row["probs"]
        self.partial.write_text(header + "\n" + json.dumps(row) + "\n")
        self.assert_refused_unchanged()

    def test_invalid_settings_are_refused_without_inference(self):
        for value in ("0", "-1", "nan", "inf"):
            with self.subTest(temperature=value), patch.object(run_http, "post") as post:
                with self.assertRaises(ValueError):
                    self.run_main(["--noul-temperature", value])
                post.assert_not_called()
        self.assertFalse(self.partial.exists())
        for field in self.server:
            with self.subTest(missing=field), patch.object(run_http, "post") as post:
                self.write("server.json", {key: value for key, value in self.server.items() if key != field})
                with self.assertRaises(ValueError):
                    self.run_main()
                post.assert_not_called()

    def test_invalid_subsets_cannot_silently_change_the_cohort(self):
        for ids in ([], ["absent"], ["first", "first"], "first"):
            with self.subTest(ids=ids), patch.object(run_http, "post") as post:
                self.write("subset.json", {"ids": ids})
                with self.assertRaises(ValueError):
                    self.run_main(["--subset", str(self.root / "subset.json")])
                post.assert_not_called()

    def test_completed_outputs_are_not_overwritten(self):
        with patch.object(run_http, "post", side_effect=self.response):
            self.run_main()
        before = self.output.read_bytes()
        with patch.object(run_http, "post") as post:
            with self.assertRaisesRegex(ValueError, "already exists"):
                self.run_main()
            post.assert_not_called()
        self.assertEqual(self.output.read_bytes(), before)

    def test_malformed_answers_do_not_become_completed_rows(self):
        self.interrupt()
        before = self.partial.read_bytes()
        for probability in (True, "0.2", -0.1, 1.1, float("nan")):
            response, elapsed = self.response("", {"questions": run_http.noul_questions()})
            response["answers"]["impact"]["noul"] = probability
            with self.subTest(probability=probability):
                with patch.object(run_http, "post", return_value=(response, elapsed)):
                    with self.assertRaises(ValueError):
                        self.run_main()
                self.assertEqual(self.partial.read_bytes(), before)

    def test_malformed_response_does_not_poison_first_sample_on_resume(self):
        sample = self.root / "first-response.json"
        response, elapsed = self.response("", {"questions": run_http.noul_questions()})
        response["answers"]["impact"]["noul"] = float("nan")
        with patch.object(run_http, "post", return_value=(response, elapsed)):
            with self.assertRaises(ValueError):
                self.run_main(["--sample-out", str(sample)])
        self.assertFalse(sample.exists(), "an invalid response must not publish a sample")
        self.assertEqual(len(self.partial.read_text().splitlines()), 1)

        with patch.object(run_http, "post", side_effect=self.response):
            self.run_main(["--sample-out", str(sample)])
        captured = json.loads(sample.read_text())
        self.assertEqual(captured["response"]["answers"]["impact"]["noul"], 0.25)
        self.assertEqual(len(json.loads(self.output.read_text())["rows"]), 2)
        self.assertFalse(self.partial.exists())

    def test_nonfinite_raw_response_does_not_publish_sample_or_result(self):
        sample = self.root / "first-response.json"
        response, elapsed = self.response("", {"questions": run_http.noul_questions()})
        # The accepted probability map alone is not proof the entire raw
        # response can be written as strict JSON.
        response["debug"] = {"nonfinite": float("nan")}
        with patch.object(run_http, "post", return_value=(response, elapsed)):
            with self.assertRaises(ValueError):
                self.run_main(["--sample-out", str(sample)])
        self.assertFalse(sample.exists())
        self.assertEqual(len(self.partial.read_text().splitlines()), 1)

    def test_existing_sample_is_never_replaced(self):
        sample = self.root / "first-response.json"
        sample.write_text('{"previous": true}')
        with patch.object(run_http, "post", side_effect=self.response):
            self.run_main(["--sample-out", str(sample)])
        self.assertEqual(sample.read_text(), '{"previous": true}')
        self.assertEqual(len(json.loads(self.output.read_text())["rows"]), 2)

    @unittest.skipUnless(os.name == "posix", "dangling links require portable POSIX support")
    def test_dangling_sample_link_refuses_without_inference_or_partial_output(self):
        sample = self.root / "first-response.json"
        sample.symlink_to("missing-capture.json")
        with patch.object(run_http, "post") as post:
            with self.assertRaisesRegex(ValueError, "not a regular file"):
                self.run_main(["--sample-out", str(sample)])
            post.assert_not_called()
        self.assertTrue(sample.is_symlink())
        self.assertFalse(sample.exists())
        self.assertFalse(self.partial.exists())
        # Also guard the atomic-publish race after the initial preflight.
        with self.assertRaisesRegex(ValueError, "not a regular file"):
            run_http.publish_sample(str(sample), '{"valid": true}')
        self.assertEqual(list(self.root.glob(".noul-sample-*")), [])

    @unittest.skipUnless(os.name == "posix", "POSIX umask semantics")
    def test_published_sample_matches_normal_exclusive_creation_mode(self):
        sample = self.root / "first-response.json"
        previous_umask = os.umask(0o022)
        try:
            with patch.object(run_http, "post", side_effect=self.response):
                self.run_main(["--sample-out", str(sample)])
        finally:
            os.umask(previous_umask)
        self.assertEqual(stat.S_IMODE(sample.stat().st_mode), 0o644)
        self.assertEqual(len(json.loads(self.output.read_text())["rows"]), 2)

    def test_calibration_remains_finite_at_extreme_positive_temperatures(self):
        for temperature in (1e-300, 1.0, 1e300):
            for probability in (0, 1e-300, 0.25, 0.5, 0.75, 1):
                with self.subTest(temperature=temperature, probability=probability):
                    scaled = run_http.calibrated(probability, temperature)
                    self.assertTrue(0 <= scaled <= 1)
        self.assertAlmostEqual(run_http.calibrated(0.25, 2), 1 / (1 + 3 ** 0.5))

    def test_transmitted_bytes_match_the_recorded_request_fingerprint(self):
        response = io.BytesIO(json.dumps({"answers": {}}).encode())
        body = {"state": "Unicode λ", "questions": {"a": {}, "b": {}}}
        with patch("urllib.request.urlopen", return_value=response) as urlopen:
            run_http.post("http://localhost:8009/v1/systemone", body)
        sent = urlopen.call_args.args[0].data
        self.assertEqual(sent, run_http.request_bytes(body))
        self.assertEqual(hashlib.sha256(sent).hexdigest(), hashlib.sha256(run_http.request_bytes(body)).hexdigest())


if __name__ == "__main__":
    unittest.main()
