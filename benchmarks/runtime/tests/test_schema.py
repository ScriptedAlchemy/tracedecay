from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from benchmarks.runtime.schema import (  # noqa: E402
    SCHEMA_VERSION,
    RuntimeArtifact,
    SchemaValidationError,
    generated_artifact_schema,
    read_jsonl,
    validate_report,
    validate_sample,
    write_jsonl,
)


CRATE_LANES = (
    "tracedecay-api",
    "tracedecay-application",
    "tracedecay-capture",
    "tracedecay-domain",
    "tracedecay-hooks",
    "tracedecay-policy",
    "tracedecay-rusqlite-parity",
    "tracedecay-rusqlite-runtime",
    "tracedecay-sdk",
    "tracedecay-store",
    "tracedecay-tool-catalog",
    "integrated-v2",
)


def valid_sample(**overrides: object) -> dict:
    sample = {
        "schema_version": SCHEMA_VERSION,
        "identity": {
            "candidate_id": "candidate-1",
            "run_id": "run-1",
            "capture_id": "capture-1",
            "crate_id": "tracedecay-application",
            "journey_id": "indexed-query",
            "workload_id": "exact-symbol",
            "variant": "baseline",
            "machine_fingerprint": "machine-a",
            "platform": "linux-x86_64",
            "shard": "runtime-0",
            "storage_mode": "sqlite",
            "state": "warm",
            "temperature": "warm",
            "surface": "mcp",
            "concurrency": 1,
            "round_index": 0,
            "abba_position": 0,
        },
        "evidence": {
            "sample_count": 1,
            "evidence_class": "regression_sample",
        },
        "availability": {"state": "available", "detail": None},
        "timing": {
            "started_ns": 10,
            "elapsed_ns": 20,
            "cli_wall_ns": None,
            "mcp_wall_ns": 20,
            "hook_wall_ns": None,
            "host_wall_ns": None,
            "handler_us": 7,
            "daemon_us": 3,
            "admission_us": 2,
            "stages_us": {"dispatch": 1},
            "shutdown_total_ns": None,
            "abort_offset_ns": None,
        },
        "size": {
            "process_count": 1,
            "request_bytes": 3,
            "response_bytes": 5,
            "content_bytes": 4,
        },
        "lifecycle": {
            "timeout_phase": None,
            "activation_state": "active",
            "restart_state": "not_required",
            "daemon_survived": True,
        },
        "observations": {},
        "outcome": {
            "status": "success",
            "expected_digest": "a" * 64,
            "actual_digest": "a" * 64,
            "result_digest": "a" * 64,
            "error": None,
        },
    }
    sample.update(overrides)
    return sample


def valid_report() -> dict:
    return {
        "schema_version": SCHEMA_VERSION,
        "identity": {
            "report_id": "report-1",
            "candidate_id": "candidate-1",
            "run_id": "run-1",
            "capture_id": "capture-1",
            "crate_id": "tracedecay-application",
            "journey_id": "indexed-query",
            "workload_id": "exact-symbol",
            "variant": "baseline",
            "machine_fingerprint": "machine-a",
            "platform": "linux-x86_64",
            "shard": "runtime-0",
            "storage_mode": "sqlite",
            "state": "warm",
            "temperature": "warm",
            "surface": "mcp",
            "concurrency": 1,
            "samples_sha256": "b" * 64,
        },
        "evidence": {
            "sample_count": 1,
            "evidence_class": "regression_sample",
        },
        "timing": {"started_ns": 10, "ended_ns": 30},
        "size": {
            "sample_count": 1,
            "process_count": 1,
            "request_bytes": 3,
            "response_bytes": 5,
            "content_bytes": 4,
        },
        "availability": {
            "available_count": 1,
            "unavailable_count": 0,
            "unsupported_count": 0,
            "partial_count": 0,
            "failed_count": 0,
        },
        "outcome": {
            "success_count": 1,
            "error_count": 0,
            "timeout_count": 0,
            "digest_mismatch_count": 0,
            "daemon_death_count": 0,
        },
        "statistics": {
            "latency_ns": {
                "sample_count": 1,
                "p50": 20,
                "p95": {
                    "available": False,
                    "value": None,
                    "minimum_samples": 40,
                },
                "p99": {
                    "available": False,
                    "value": None,
                    "minimum_samples": 100,
                },
            }
        },
    }


class SampleSchemaTests(unittest.TestCase):
    def test_valid_sample_is_returned_unchanged(self) -> None:
        sample = valid_sample()

        self.assertIs(validate_sample(sample), sample)

    def test_missing_required_section_is_rejected(self) -> None:
        sample = valid_sample()
        del sample["timing"]

        with self.assertRaisesRegex(SchemaValidationError, "timing"):
            validate_sample(sample)

    def test_unknown_fields_and_wrong_schema_version_are_rejected(self) -> None:
        sample = valid_sample(unexpected=True)
        with self.assertRaisesRegex(SchemaValidationError, "unexpected"):
            validate_sample(sample)

        sample = valid_sample(schema_version=SCHEMA_VERSION + 1)
        with self.assertRaisesRegex(SchemaValidationError, "schema_version"):
            validate_sample(sample)

    def test_outcome_consistency_is_enforced(self) -> None:
        sample = valid_sample()
        sample["outcome"]["status"] = "error"

        with self.assertRaisesRegex(SchemaValidationError, "error"):
            validate_sample(sample)

    def test_reviewed_runtime_evidence_fields_are_required(self) -> None:
        sample = valid_sample()

        self.assertIs(validate_sample(sample), sample)
        self.assertEqual(sample["identity"]["surface"], "mcp")
        self.assertEqual(sample["timing"]["mcp_wall_ns"], 20)
        self.assertEqual(sample["timing"]["handler_us"], 7)
        self.assertEqual(sample["size"]["process_count"], 1)
        self.assertEqual(sample["outcome"]["result_digest"], "a" * 64)

        del sample["identity"]["capture_id"]
        with self.assertRaisesRegex(SchemaValidationError, "capture_id"):
            validate_sample(sample)

    def test_incident_observations_are_preserved_in_raw_samples(self) -> None:
        sample = valid_sample(
            observations={
                "daemon_cpu_time_ns": 10,
                "daemon_peak_rss_bytes": 20,
                "wal_bytes": 30,
                "queue_depth": 2,
                "generation": 4,
            }
        )

        self.assertIs(validate_sample(sample), sample)
        self.assertEqual(sample["observations"]["wal_bytes"], 30)

    def test_impossible_incident_observations_fail_closed(self) -> None:
        sample = valid_sample(
            observations={
                "renderer_event_count": 1,
                "consumer_event_count": 2,
            }
        )

        with self.assertRaisesRegex(SchemaValidationError, "consumer"):
            validate_sample(sample)

    def test_unavailable_incident_can_record_no_daemon_to_survive(self) -> None:
        sample = valid_sample()
        sample["availability"] = {
            "state": "unavailable",
            "detail": "daemon socket is absent",
        }
        sample["lifecycle"]["daemon_survived"] = None
        sample["outcome"] = {
            "status": "error",
            "expected_digest": "a" * 64,
            "actual_digest": "a" * 64,
            "result_digest": "a" * 64,
            "error": "expected daemon unavailable",
        }

        self.assertIs(validate_sample(sample), sample)

    def test_stable_workload_identity_supports_every_crate_lane_and_state(self) -> None:
        for crate_id in CRATE_LANES:
            with self.subTest(crate_id=crate_id):
                sample = valid_sample()
                sample["identity"]["crate_id"] = crate_id
                self.assertIs(validate_sample(sample), sample)

        for state in ("cold", "warm", "no_op", "contention", "recovery"):
            with self.subTest(state=state):
                sample = valid_sample()
                sample["identity"]["state"] = state
                sample["identity"]["temperature"] = (
                    "cold" if state == "cold" else "warm"
                )
                self.assertIs(validate_sample(sample), sample)

    def test_runtime_normalization_dimensions_are_required_and_typed(self) -> None:
        dimensions = {
            "platform": "linux-x86_64",
            "shard": "runtime-0",
            "storage_mode": "sqlite",
            "concurrency": 1,
            "temperature": "warm",
        }
        sample = valid_sample()
        self.assertEqual(
            {field: sample["identity"][field] for field in dimensions},
            dimensions,
        )

        for field in dimensions:
            with self.subTest(field=field):
                missing = valid_sample()
                del missing["identity"][field]
                with self.assertRaisesRegex(SchemaValidationError, field):
                    validate_sample(missing)

        sample["identity"]["temperature"] = "hot"
        with self.assertRaisesRegex(SchemaValidationError, "temperature"):
            validate_sample(sample)

    def test_pr_stage_identity_and_milestone_budget_data_are_rejected(self) -> None:
        for field in ("crate_id", "journey_id", "workload_id"):
            with self.subTest(field=field):
                sample = valid_sample()
                sample["identity"][field] = "pr14-stage"
                with self.assertRaisesRegex(SchemaValidationError, "PR-stage"):
                    validate_sample(sample)

        sample = valid_sample()
        sample["identity"]["scenario"] = "pr14"
        with self.assertRaisesRegex(SchemaValidationError, "scenario"):
            validate_sample(sample)

    def test_unavailable_evidence_is_typed_and_never_successful_zero(self) -> None:
        for availability_state in ("unavailable", "unsupported", "partial", "failed"):
            with self.subTest(availability_state=availability_state):
                sample = valid_sample()
                sample["availability"] = {
                    "state": availability_state,
                    "detail": f"{availability_state} evidence",
                }
                sample["timing"].update(
                    {
                        "elapsed_ns": None,
                        "mcp_wall_ns": None,
                        "handler_us": None,
                        "daemon_us": None,
                        "admission_us": None,
                        "stages_us": {},
                    }
                )
                sample["size"].update(
                    {
                        "process_count": None,
                        "request_bytes": None,
                        "response_bytes": None,
                        "content_bytes": None,
                    }
                )
                sample["outcome"].update(
                    {
                        "status": "error",
                        "actual_digest": None,
                        "result_digest": None,
                        "error": f"{availability_state} evidence",
                    }
                )
                self.assertIs(validate_sample(sample), sample)

                sample["outcome"].update(
                    {
                        "status": "success",
                        "actual_digest": "a" * 64,
                        "result_digest": "a" * 64,
                        "error": None,
                    }
                )
                sample["timing"]["elapsed_ns"] = 0
                sample["timing"]["mcp_wall_ns"] = 0
                sample["size"].update(
                    {
                        "process_count": 0,
                        "request_bytes": 0,
                        "response_bytes": 0,
                        "content_bytes": 0,
                    }
                )
                with self.assertRaisesRegex(
                    SchemaValidationError, "cannot have a successful outcome"
                ):
                    validate_sample(sample)

    def test_timeout_phase_and_daemon_survival_are_truthful(self) -> None:
        timeout = valid_sample()
        timeout["lifecycle"]["timeout_phase"] = "response"
        timeout["outcome"].update(
            {
                "status": "timeout",
                "actual_digest": None,
                "result_digest": None,
                "error": "deadline exceeded",
            }
        )
        self.assertIs(validate_sample(timeout), timeout)

        timeout["lifecycle"]["timeout_phase"] = None
        with self.assertRaisesRegex(SchemaValidationError, "timeout_phase"):
            validate_sample(timeout)

        daemon_death = valid_sample()
        daemon_death["lifecycle"]["daemon_survived"] = False
        with self.assertRaisesRegex(SchemaValidationError, "daemon_survived"):
            validate_sample(daemon_death)

    def test_shutdown_total_and_abort_offset_remain_distinct(self) -> None:
        for total_seconds, abort_seconds in ((89, 81), (57, 52)):
            with self.subTest(total_seconds=total_seconds):
                sample = valid_sample()
                sample["timing"].update(
                    {
                        "elapsed_ns": total_seconds * 1_000_000_000,
                        "mcp_wall_ns": total_seconds * 1_000_000_000,
                        "shutdown_total_ns": total_seconds * 1_000_000_000,
                        "abort_offset_ns": abort_seconds * 1_000_000_000,
                    }
                )

                validated = validate_sample(sample)

                self.assertEqual(
                    validated["timing"]["shutdown_total_ns"],
                    total_seconds * 1_000_000_000,
                )
                self.assertEqual(
                    validated["timing"]["abort_offset_ns"],
                    abort_seconds * 1_000_000_000,
                )
                self.assertGreater(
                    validated["timing"]["shutdown_total_ns"],
                    validated["timing"]["abort_offset_ns"],
                )

        sample["timing"]["abort_offset_ns"] = sample["timing"]["shutdown_total_ns"]
        with self.assertRaisesRegex(SchemaValidationError, "abort_offset_ns"):
            validate_sample(sample)


class ReportSchemaTests(unittest.TestCase):
    def test_valid_report_is_returned_unchanged(self) -> None:
        report = valid_report()

        self.assertIs(validate_report(report), report)

    def test_report_count_and_timing_invariants_are_enforced(self) -> None:
        report = valid_report()
        report["outcome"]["error_count"] = 1
        with self.assertRaisesRegex(SchemaValidationError, "counts"):
            validate_report(report)

        report = valid_report()
        report["timing"]["ended_ns"] = 9
        with self.assertRaisesRegex(SchemaValidationError, "ended_ns"):
            validate_report(report)

    def test_n1_regression_evidence_rejects_slo_gate_metadata(self) -> None:
        report = valid_report()
        report["evidence"]["baseline_policy_met"] = True
        report["evidence"]["slo_eligible"] = True

        with self.assertRaisesRegex(SchemaValidationError, "unexpected"):
            validate_report(report)

    def test_percentile_history_eligibility_is_explicit(self) -> None:
        report = valid_report()
        report["statistics"]["latency_ns"]["p99"] = {
            "available": True,
            "value": 20,
            "minimum_samples": 100,
        }

        with self.assertRaisesRegex(SchemaValidationError, "p99"):
            validate_report(report)

    def test_availability_and_daemon_counts_are_consistent(self) -> None:
        report = valid_report()
        report["availability"]["available_count"] = 0
        report["availability"]["unavailable_count"] = 1
        report["outcome"]["success_count"] = 0
        report["outcome"]["error_count"] = 1
        self.assertIs(validate_report(report), report)

        report["outcome"]["daemon_death_count"] = 2
        with self.assertRaisesRegex(SchemaValidationError, "daemon_death_count"):
            validate_report(report)

    def test_report_rejects_milestone_budget_statistics(self) -> None:
        report = valid_report()
        report["statistics"]["milestone_budget_ns"] = 1

        with self.assertRaisesRegex(SchemaValidationError, "milestone"):
            validate_report(report)


class JsonLinesTests(unittest.TestCase):
    def test_jsonl_output_is_canonical_and_deterministic(self) -> None:
        sample = valid_sample()
        expected = json.dumps(
            sample,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ) + "\n"
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "first.jsonl"
            second = Path(directory) / "second.jsonl"

            write_jsonl(first, [sample])
            write_jsonl(second, [sample])

            self.assertEqual(first.read_bytes(), second.read_bytes())
            self.assertEqual(first.read_text(encoding="utf-8"), expected)
            self.assertEqual(read_jsonl(first), [sample])

    def test_malformed_json_and_invalid_schema_name_the_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "samples.jsonl"
            path.write_text('{"broken":\n', encoding="utf-8")
            with self.assertRaisesRegex(SchemaValidationError, r"line 1"):
                read_jsonl(path)

            path.write_text('{"schema_version":1}\n', encoding="utf-8")
            with self.assertRaisesRegex(SchemaValidationError, r"line 1"):
                read_jsonl(path)

    def test_blank_jsonl_lines_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "samples.jsonl"
            path.write_text("\n", encoding="utf-8")

            with self.assertRaisesRegex(SchemaValidationError, "blank"):
                read_jsonl(path)


class GeneratedArtifactModelTests(unittest.TestCase):
    def test_one_typed_model_validates_samples_and_reports(self) -> None:
        sample = RuntimeArtifact.from_document(valid_sample())
        report = RuntimeArtifact.from_document(valid_report())

        self.assertEqual(sample.kind, "sample")
        self.assertEqual(report.kind, "report")
        self.assertEqual(sample.identity["workload_id"], "exact-symbol")
        self.assertEqual(report.identity["crate_id"], "tracedecay-application")

    def test_generated_schema_uses_canonical_model_sections(self) -> None:
        schema = generated_artifact_schema()

        self.assertEqual(schema["$id"], "tracedecay.runtime-artifact.v1")
        self.assertEqual(schema["oneOf"][0]["title"], "runtime-sample")
        self.assertEqual(schema["oneOf"][1]["title"], "runtime-report")
        self.assertIn("lifecycle", schema["oneOf"][0]["required"])
        self.assertNotIn("statistics", schema["oneOf"][0]["required"])
        self.assertIn("statistics", schema["oneOf"][1]["required"])
        self.assertNotIn("lifecycle", schema["oneOf"][1]["required"])


if __name__ == "__main__":
    unittest.main()
