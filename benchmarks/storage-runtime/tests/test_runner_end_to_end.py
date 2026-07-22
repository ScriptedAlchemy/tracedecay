"""Unit tests for the S0 storage-runtime baseline runner.

Stdlib unittest only; no live daemon, no live profile, no product fixtures.
Run from this directory:

    python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import json
import os
import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import run_storage_baseline as rsb  # noqa: E402



class EndToEndDryRunTests(unittest.TestCase):
    HERE = Path(__file__).resolve().parent.parent

    def test_dry_run_produces_valid_result(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            output_dir = root / "output"
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-dry-run.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(output_dir),
                ]
            )
            self.assertEqual(rc, 0)
            result = json.loads(
                (output_dir / "storage-runtime-baseline-result.json").read_text()
            )
            self.assertEqual(rsb.validate_result(result), [])
            self.assertEqual(rsb.result_contains_absolute_paths(result), [])
            self.assertEqual(result["status"], "not_evidence")
            self.assertEqual(result["evidence_status"]["state"], "not_evidence")
            phases = {run["phase"] for run in result["runs"]}
            self.assertTrue(
                {
                    "current",
                    "ten_x",
                    "overload",
                    "crash",
                    "recovery",
                    "fts",
                    "backup_restore",
                    "aa_noise",
                }
                <= phases
            )
            overload = next(
                run for run in result["runs"] if run["phase"] == "overload"
            )
            counts = overload["counts"]
            self.assertEqual(counts["offered"], 20)
            self.assertGreater(counts["completed"], 0)
            latency = overload["latency"]["response_ns"]
            self.assertIsNotNone(latency["p99_ns"])
            self.assertIn("schedule_lag_ns", overload["latency"])
            self.assertEqual(len(overload["requests"]), counts["offered"])
            self.assertTrue(all(request["terminal"] for request in overload["requests"]))
            self.assertGreater(counts["shed"]["runner_in_flight_cap"], 0)
            runner_shed = next(
                request
                for request in overload["requests"]
                if request["outcome"] == "shed_runner_in_flight_cap"
            )
            self.assertIsNone(runner_shed["admitted_at_ns"])
            self.assertIsNone(runner_shed["started_at_ns"])
            self.assertTrue(
                all(
                    key in request
                    for request in overload["requests"]
                    for key in (
                        "scheduled_at_ns",
                        "admitted_at_ns",
                        "started_at_ns",
                        "terminal_at_ns",
                        "outcome",
                    )
                )
            )
            fts = next(run for run in result["runs"] if run["phase"] == "fts")
            self.assertTrue(
                all(
                    evidence["output"]["redacted"]
                    for evidence in fts["evidence"].values()
                )
            )
            aa = next(
                run
                for run in result["runs"]
                if run["phase"] == "aa_noise" and "aa" in run
            )
            floor = aa["aa"]["noise_floor"]["p50_response_ns"]
            self.assertIsNotNone(floor["aa_noise_floor_relative"])
            self.assertIsNotNone(floor["regression_margin_relative"])

    def test_s0_workload_fails_closed_without_allow_pending(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-s0.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(root / "output"),
                ]
            )
            self.assertEqual(rc, 2)

    def test_only_output_is_explicitly_partial_not_evidence(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            output_dir = root / "output"
            self.assertEqual(
                rsb.main(
                    [
                        "run",
                        "--workload",
                        str(self.HERE / "workload-dry-run.json"),
                        "--input",
                        str(input_dir),
                        "--output",
                        str(output_dir),
                        "--only",
                        "current",
                    ]
                ),
                0,
            )
            result = json.loads((output_dir / "storage-runtime-baseline-result.json").read_text())
            self.assertEqual(result["status"], "not_evidence")
            self.assertEqual(result["execution_scope"]["mode"], "partial")
            self.assertIn("--only", " ".join(result["evidence_status"]["reasons"]))

    def test_failed_operations_return_nonzero_and_cannot_be_evidence(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            workload = json.loads((self.HERE / "workload-dry-run.json").read_text())
            workload["phases"] = [workload["phases"][0]]
            workload["phases"][0]["warmup"] = 0
            workload["phases"][0]["repetitions"] = 1
            workload["phases"][0]["work"] = {
                "argv": ["__PYTHON__", "-c", "import sys; sys.exit(1)"]
            }
            workload_path = root / "workload.json"
            workload_path.write_text(json.dumps(workload))
            output = root / "output"
            self.assertEqual(
                rsb.main(
                    [
                        "run",
                        "--workload",
                        str(workload_path),
                        "--input",
                        str(input_dir),
                        "--output",
                        str(output),
                    ]
                ),
                2,
            )
            result = json.loads((output / "storage-runtime-baseline-result.json").read_text())
            self.assertEqual(result["status"], "failed_validation")
            self.assertTrue(any("failed operations" in item for item in result["validation_problems"]))

    def test_s0_workload_records_pending_with_allow_pending_and_identity(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            binary = root / "fake-binary"
            binary.write_text("fake")
            evidence_binary = root / "fake-evidence-binary"
            evidence_binary.write_text("evidence")
            config = root / "config.toml"
            config.write_text("mode = 'pending'\n")
            identity = root / "frozen-identity.json"
            self.assertEqual(
                rsb.main(
                    [
                        "freeze",
                        "--product-binary",
                        str(binary),
                        "--evidence-binary",
                        str(evidence_binary),
                        "--product-commit-sha",
                        "a" * 40,
                        "--product-binary-version-argv",
                        "--schema-manifest",
                        str(binary),
                        "--workload",
                        str(self.HERE / "workload-s0.json"),
                        "--corpus",
                        str(input_dir),
                        "--config",
                        str(config),
                        "--output",
                        str(identity),
                    ]
                ),
                0,
            )
            output_dir = root / "output"
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-s0.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(output_dir),
                    "--frozen-identity",
                    str(identity),
                    "--product-binary",
                    str(binary),
                    "--evidence-binary",
                    str(evidence_binary),
                    "--schema-manifest",
                    str(binary),
                    "--config",
                    str(config),
                    "--allow-pending",
                ]
            )
            self.assertEqual(rc, 0)
            result = json.loads(
                (output_dir / "storage-runtime-baseline-result.json").read_text()
            )
            self.assertTrue(
                all(run["status"] == "pending" for run in result["runs"])
            )
            self.assertEqual(result["frozen_identity"]["status"], "supplied")
            self.assertEqual(result["identity_binding"]["status"], "bound")
            self.assertEqual(result["status"], "not_evidence")

    def test_run_refuses_live_profile_output(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            live = root / "home" / ".tracedecay"
            live.mkdir(parents=True)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            old_env = os.environ.get("TRACEDECAY_DATA_DIR")
            os.environ["TRACEDECAY_DATA_DIR"] = str(live)
            try:
                rc = rsb.main(
                    [
                        "run",
                        "--workload",
                        str(self.HERE / "workload-dry-run.json"),
                        "--input",
                        str(input_dir),
                        "--output",
                        str(live / "out"),
                    ]
                )
            finally:
                if old_env is None:
                    os.environ.pop("TRACEDECAY_DATA_DIR", None)
                else:
                    os.environ["TRACEDECAY_DATA_DIR"] = old_env
            self.assertEqual(rc, 2)
            self.assertFalse((live / "out").exists())

    def test_absolute_host_label_prevents_result_publication(self):
        with tempfile.TemporaryDirectory(prefix="s0-e2e-") as tmp:
            root = Path(tmp)
            input_dir = root / "input"
            shutil.copytree(self.HERE / "fixtures" / "dry-run-input", input_dir)
            output = root / "output"
            rc = rsb.main(
                [
                    "run",
                    "--workload",
                    str(self.HERE / "workload-dry-run.json"),
                    "--input",
                    str(input_dir),
                    "--output",
                    str(output),
                    "--host-label",
                    str(root),
                ]
            )
            self.assertEqual(rc, 2)
            self.assertFalse((output / "storage-runtime-baseline-result.json").exists())


if __name__ == "__main__":
    unittest.main()
