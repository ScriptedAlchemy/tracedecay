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



class PercentileTests(unittest.TestCase):
    def test_nearest_rank_known_values(self):
        samples = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100]
        summary = rsb.summarize_latency(samples)
        self.assertEqual(summary["count"], 10)
        self.assertEqual(summary["min_ns"], 10)
        self.assertEqual(summary["p50_ns"], 50)
        self.assertEqual(summary["p95_ns"], 100)
        self.assertEqual(summary["p99_ns"], 100)
        self.assertEqual(summary["max_ns"], 100)
        self.assertEqual(summary["percentile_method"], "nearest_rank")

    def test_empty_samples(self):
        summary = rsb.summarize_latency([])
        self.assertEqual(summary["count"], 0)
        self.assertIsNone(summary["p50_ns"])

    def test_single_sample_zero_stddev(self):
        summary = rsb.summarize_latency([42])
        self.assertEqual(summary["p50_ns"], 42)
        self.assertEqual(summary["sample_stddev_ns"], 0.0)


class CountsInvariantTests(unittest.TestCase):
    def test_consistent_counts_pass(self):
        counts = rsb.new_counts()
        counts.update({"offered": 10, "admitted": 9, "completed": 8, "failed": 1})
        counts["shed"]["runner_in_flight_cap"] = 1
        self.assertEqual(rsb.counts_invariants_ok(counts), [])

    def test_offered_mismatch_detected(self):
        counts = rsb.new_counts()
        counts.update({"offered": 10, "admitted": 10, "completed": 10})
        counts["shed"]["runner_in_flight_cap"] = 1
        problems = rsb.counts_invariants_ok(counts)
        self.assertTrue(any("offered" in p for p in problems))

    def test_admitted_mismatch_detected(self):
        counts = rsb.new_counts()
        counts.update({"offered": 10, "admitted": 10, "completed": 8, "failed": 1})
        problems = rsb.counts_invariants_ok(counts)
        self.assertTrue(any("admitted" in p for p in problems))



class SubstitutionTests(unittest.TestCase):
    def test_tokens_replaced(self):
        argv = rsb.substitute_argv(
            ["__INPUT__/store.db", "literal", "__REPETITION__"],
            {"INPUT": "/in", "REPETITION": "7"},
        )
        self.assertEqual(argv, ["/in/store.db", "literal", "7"])


class FingerprintTests(unittest.TestCase):
    def test_deterministic_and_path_independent(self):
        with tempfile.TemporaryDirectory(prefix="s0-fp-") as tmp:
            root_a = Path(tmp) / "a"
            root_b = Path(tmp) / "b"
            for root in (root_a, root_b):
                root.mkdir()
                (root / "x.txt").write_text("one")
                sub = root / "sub"
                sub.mkdir()
                (sub / "y.txt").write_text("two")
            fp_a = rsb.fingerprint_tree(root_a)
            fp_b = rsb.fingerprint_tree(root_b)
            self.assertEqual(fp_a["aggregate_sha256"], fp_b["aggregate_sha256"])
            self.assertEqual(fp_a["file_count"], 2)



class LogicalSqliteEvidenceTests(unittest.TestCase):
    def test_logical_sqlite_evidence_redacts_rows_and_schema_text(self):
        with tempfile.TemporaryDirectory(prefix="s0-sqlite-") as tmp:
            database = Path(tmp) / "store.sqlite"
            connection = sqlite3.connect(database)
            connection.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, secret TEXT)")
            connection.execute("INSERT INTO items (secret) VALUES ('token-alpha')")
            connection.commit()
            connection.close()
            evidence = rsb.capture_logical_sqlite_evidence(database, {"tables": ["items"]})
            self.assertEqual(evidence["schema"], rsb.LOGICAL_SQLITE_EVIDENCE_SCHEMA)
            self.assertEqual(evidence["integrity"]["status"], "ok")
            self.assertEqual(evidence["tables"], [{"table_id": "items", "row_count": 1}])
            self.assertNotIn("token-alpha", json.dumps(evidence))
            self.assertNotIn("CREATE TABLE", json.dumps(evidence))

    def test_fts_rank_and_snippet_are_hashed_not_published(self):
        with tempfile.TemporaryDirectory(prefix="s0-sqlite-") as tmp:
            database = Path(tmp) / "store.sqlite"
            connection = sqlite3.connect(database)
            try:
                connection.execute("CREATE VIRTUAL TABLE docs USING fts5(body)")
            except sqlite3.OperationalError as exc:
                self.skipTest(f"SQLite FTS5 unavailable: {exc}")
            connection.execute("INSERT INTO docs (body) VALUES ('token-alpha token-beta')")
            connection.commit()
            connection.close()
            evidence = rsb.capture_logical_sqlite_evidence(
                database,
                {
                    "fts_probes": [
                        {
                            "name": "query",
                            "table": "docs",
                            "query": "token",
                            "projection": "rowid_rank_snippet",
                        }
                    ]
                },
            )
            probe = evidence["fts"][0]
            self.assertEqual(probe["projection"], "rowid_rank_snippet")
            self.assertEqual(len(probe["result_sha256"]), 64)
            self.assertNotIn("token-alpha", json.dumps(evidence))


class WorkloadValidationTests(unittest.TestCase):
    def _write(self, root: Path, doc: dict) -> Path:
        path = root / "workload.json"
        path.write_text(json.dumps(doc))
        return path

    def _minimal(self) -> dict:
        return {
            "schema_version": 1,
            "workload_id": "t",
            "store_families": ["graph"],
            "phases": [
                {
                    "name": "current",
                    "kind": "closed_loop",
                    "families": ["graph"],
                    "work": {"argv": ["true"]},
                }
            ],
        }

    def test_minimal_workload_loads(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            workload = rsb.load_workload(self._write(Path(tmp), self._minimal()))
            self.assertEqual(workload["workload_id"], "t")

    def test_wrong_schema_version_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["schema_version"] = 99
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_case_insensitive_family_collision_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["store_families"] = ["graph", "GRAPH"]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_windows_reserved_identifier_rejected(self):
        with self.assertRaises(rsb.ConfigError):
            rsb.require_safe_identifier("CON.json", "test identifier")

    def test_malformed_argv_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["phases"][0]["work"]["argv"] = "true"
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_product_evidence_rejects_synthetic_logical_file(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["evidence_eligible"] = True
            doc["phases"][0]["evidence"] = [
                {"name": "state", "capture": "logical_file", "path": "__RUN_DIR__/state"}
            ]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_unknown_family_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["phases"][0]["families"] = ["nope"]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_recovery_requires_depends_on(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            doc = self._minimal()
            doc["phases"] = [
                {"name": "recovery", "kind": "recovery", "families": ["graph"]}
            ]
            with self.assertRaises(rsb.ConfigError):
                rsb.load_workload(self._write(Path(tmp), doc))

    def test_null_argv_is_pending(self):
        phase = {"name": "p", "kind": "closed_loop", "work": {"argv": None}}
        self.assertIsNotNone(rsb.phase_pending_reason(phase))

    def test_concrete_argv_not_pending(self):
        phase = {"name": "p", "kind": "closed_loop", "work": {"argv": ["true"]}}
        self.assertIsNone(rsb.phase_pending_reason(phase))


class CheckedInWorkloadTests(unittest.TestCase):
    HERE = Path(__file__).resolve().parent.parent

    def test_s0_workload_parses_and_is_fully_pending(self):
        workload = rsb.load_workload(self.HERE / "workload-s0.json")
        self.assertEqual(workload["workload_id"], "storage-runtime-s0-baseline-v1")
        self.assertTrue(workload["evidence_eligible"])
        self.assertIn("product_binary", workload)
        self.assertIn("evidence_binary", workload)
        self.assertNotIn("binary", workload)
        self.assertEqual(
            workload["store_families"], ["graph", "profile", "project", "session"]
        )
        names = [phase["name"] for phase in workload["phases"]]
        self.assertEqual(
            names,
            [
                "current",
                "ten_x",
                "overload",
                "crash",
                "recovery",
                "fts",
                "backup_restore",
                "aa_noise",
            ],
        )
        for phase in workload["phases"]:
            if phase["kind"] == "aa_pairs":
                continue
            self.assertIsNotNone(
                rsb.phase_pending_reason(phase),
                f"S0 phase {phase['name']!r} must remain pending until wired",
            )

    def test_s0_pending_reasons_name_executable_prerequisites(self):
        workload = rsb.load_workload(self.HERE / "workload-s0.json")
        reasons = {
            phase["name"]: rsb.phase_pending_reason(phase) or ""
            for phase in workload["phases"]
            if phase["kind"] != "aa_pairs"
        }
        self.assertIn("explicit fixture paths", reasons["current"])
        self.assertIn("documented saturation outcomes", reasons["overload"])
        self.assertIn("MaintenanceCoordinator", reasons["crash"])
        self.assertIn("storage-runtime-crash-recovery-repair-v1", reasons["crash"])
        self.assertIn("SqliteCorruptionProbe", reasons["recovery"])
        self.assertIn(
            "storage-runtime-crash-recovery-repair-v1", reasons["recovery"]
        )
        self.assertIn("storage-runtime-fixture-v1.json", reasons["fts"])
        self.assertIn(
            "BackupRestoreOrchestrator", reasons["backup_restore"]
        )

    def test_dry_run_workload_has_no_pending_phases(self):
        workload = rsb.load_workload(self.HERE / "workload-dry-run.json")
        self.assertFalse(workload["evidence_eligible"])
        for phase in workload["phases"]:
            self.assertIsNone(
                rsb.phase_pending_reason(phase),
                f"dry-run phase {phase['name']!r} must be executable",
            )

    def test_legacy_ambiguous_binary_field_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="s0-wl-") as tmp:
            path = Path(tmp) / "workload.json"
            document = json.loads((self.HERE / "workload-dry-run.json").read_text())
            document["binary"] = "/ambiguous"
            path.write_text(json.dumps(document))
            with self.assertRaisesRegex(rsb.ConfigError, "ambiguous"):
                rsb.load_workload(path)
