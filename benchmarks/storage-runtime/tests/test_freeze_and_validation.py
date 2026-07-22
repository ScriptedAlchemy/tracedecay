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



class FreezeTests(unittest.TestCase):
    def test_freeze_captures_hashes_without_paths(self):
        with tempfile.TemporaryDirectory(prefix="s0-freeze-") as tmp:
            root = Path(tmp)
            binary = root / "fake-binary"
            binary.write_text("#!/bin/sh\nexit 0\n")
            evidence_binary = root / "storage-runtime-evidence"
            evidence_binary.write_text("evidence")
            schema = root / "schema.sql"
            schema.write_text("-- operator supplied released schema export\n")
            workload = root / "workload.json"
            workload.write_text('{"schema_version": 1}\n')
            corpus = root / "corpus"
            corpus.mkdir()
            (corpus / "fixture.txt").write_text("fixture")
            config = root / "config.toml"
            config.write_text("mode = 'isolated'\n")
            out = root / "frozen-identity.json"
            rc = rsb.main(
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
                    str(schema),
                    "--workload",
                    str(workload),
                    "--corpus",
                    str(corpus),
                    "--config",
                    str(config),
                    "--output",
                    str(out),
                ]
            )
            self.assertEqual(rc, 0)
            identity = json.loads(out.read_text())
            self.assertEqual(
                identity["artifact_id"], "storage-runtime-frozen-identity-v3"
            )
            self.assertEqual(identity["product_commit_sha"], "a" * 40)
            self.assertEqual(identity["product_binary"]["basename"], "fake-binary")
            self.assertEqual(
                identity["evidence_binary"]["basename"], "storage-runtime-evidence"
            )
            self.assertNotIn(str(root), json.dumps(identity["product_binary"]))
            self.assertEqual(len(identity["product_binary"]["sha256"]), 64)
            self.assertEqual(len(identity["evidence_binary"]["sha256"]), 64)
            self.assertEqual(len(identity["schema_manifest"]["sha256"]), 64)
            self.assertEqual(len(identity["workload"]["sha256"]), 64)
            self.assertEqual(identity["corpus"]["kind"], "tree")
            self.assertEqual(len(identity["config"]["sha256"]), 64)

    def test_freeze_refuses_one_artifact_for_both_binary_roles(self):
        with tempfile.TemporaryDirectory(prefix="s0-freeze-") as tmp:
            root = Path(tmp)
            binary = root / "binary"
            binary.write_text("same artifact")
            schema = root / "schema.sql"
            schema.write_text("-- schema\n")
            workload = root / "workload.json"
            workload.write_text('{"schema_version": 1}\n')
            corpus = root / "corpus"
            corpus.mkdir()
            config = root / "config.toml"
            config.write_text("mode = 'isolated'\n")
            self.assertEqual(
                rsb.main(
                    [
                        "freeze",
                        "--product-binary",
                        str(binary),
                        "--evidence-binary",
                        str(binary),
                        "--product-commit-sha",
                        "a" * 40,
                        "--product-binary-version-argv",
                        "--schema-manifest",
                        str(schema),
                        "--workload",
                        str(workload),
                        "--corpus",
                        str(corpus),
                        "--config",
                        str(config),
                        "--output",
                        str(root / "identity.json"),
                    ]
                ),
                2,
            )


class FrozenIdentityBindingTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "workload execution needs POSIX tree verification")
    def test_bound_synthetic_run_remains_not_evidence(self):
        with tempfile.TemporaryDirectory(prefix="s0-binding-") as tmp:
            root = Path(tmp)
            corpus = root / "corpus"
            corpus.mkdir()
            (corpus / "seed.txt").write_text("seed")
            binary = root / "binary"
            binary.write_text("not executed")
            evidence_binary = root / "evidence-binary"
            evidence_binary.write_text("evidence not executed")
            schema = root / "schema.sql"
            schema.write_text("-- schema\n")
            config = root / "config.toml"
            config.write_text("mode = 'baseline'\n")
            workload = root / "workload.json"
            workload.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "workload_id": "bound-run",
                        "store_families": ["sample"],
                        "phases": [
                            {
                                "name": "current",
                                "kind": "closed_loop",
                                "families": ["sample"],
                                "warmup": 0,
                                "repetitions": 1,
                                "setup": {
                                    "argv": [
                                        "__PYTHON__",
                                        "-c",
                                        "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('seed')",
                                        "__RUN_DIR__/state.txt",
                                    ]
                                },
                                "work": {
                                    "argv": [
                                        "__PYTHON__",
                                        "-c",
                                        "import pathlib,sys; p=pathlib.Path(sys.argv[1]); p.write_text(p.read_text()+'x')",
                                        "__RUN_DIR__/state.txt",
                                    ]
                                },
                                "evidence": [
                                    {
                                        "name": "state",
                                        "capture": "logical_file",
                                        "path": "__RUN_DIR__/state.txt",
                                    }
                                ],
                            }
                        ],
                    }
                )
            )
            identity = root / "identity.json"
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
                        str(schema),
                        "--workload",
                        str(workload),
                        "--corpus",
                        str(corpus),
                        "--config",
                        str(config),
                        "--output",
                        str(identity),
                    ]
                ),
                0,
            )
            output = root / "output"
            self.assertEqual(
                rsb.main(
                    [
                        "run",
                        "--workload",
                        str(workload),
                        "--input",
                        str(corpus),
                        "--output",
                        str(output),
                        "--frozen-identity",
                        str(identity),
                        "--product-binary",
                        str(binary),
                        "--evidence-binary",
                        str(evidence_binary),
                        "--schema-manifest",
                        str(schema),
                        "--config",
                        str(config),
                    ]
                ),
                0,
            )
            result = json.loads((output / "storage-runtime-baseline-result.json").read_text())
            self.assertEqual(result["status"], "not_evidence")
            self.assertEqual(result["evidence_status"]["state"], "not_evidence")
            self.assertIn(
                "workload is explicitly ineligible for product evidence",
                result["evidence_status"]["reasons"],
            )
            self.assertEqual(result["identity_binding"]["status"], "bound")
            self.assertEqual(
                result["identity_binding"]["product_commit_sha"], "a" * 40
            )
            tampered = json.loads(json.dumps(result))
            tampered["status"] = "completed"
            tampered["evidence_status"] = {"state": "evidence", "reasons": []}
            tampered["execution_scope"]["mode"] = "partial"
            self.assertTrue(
                any("partial" in problem for problem in rsb.validate_result(tampered))
            )
            current = result["runs"][0]
            self.assertTrue((output / current["run_dir"] / "store" / "seed.txt").is_file())
            self.assertEqual(
                (output / current["run_dir"] / "sandbox" / "config" / "config.toml").read_text(),
                "mode = 'baseline'\n",
            )
            self.assertEqual((corpus / "seed.txt").read_text(), "seed")

            config.write_text("mode = 'mutated'\n")
            identity_doc = json.loads(identity.read_text())
            with self.assertRaises(rsb.ConfigError):
                rsb.bind_frozen_identity(
                    identity_doc,
                    product_binary_path=binary,
                    evidence_binary_path=evidence_binary,
                    schema_manifest_path=schema,
                    workload_path=workload,
                    corpus_root=corpus,
                    config_path=config,
                )


class ResultValidationTests(unittest.TestCase):
    def test_absolute_path_leak_detected(self):
        result = {"runs": [{"run_dir": "/abs/leak"}]}
        hits = rsb.result_contains_absolute_paths(result)
        self.assertTrue(hits)

    def test_relative_paths_pass(self):
        result = {"runs": [{"run_dir": "work/current/graph"}]}
        self.assertEqual(rsb.result_contains_absolute_paths(result), [])

    def test_windows_unc_path_leak_detected(self):
        result = {"runs": [{"run_dir": "\\\\server\\share\\leak"}]}
        self.assertTrue(rsb.result_contains_absolute_paths(result))

    def test_open_loop_ledger_must_bind_retries_to_requests(self):
        counts = rsb.new_counts()
        counts.update({"offered": 1, "admitted": 1, "completed": 1, "retried": 1})
        run = {
            "phase": "overload",
            "requests": [
                {
                    "request_id": 0,
                    "terminal": True,
                    "scheduled_at_ns": 0,
                    "admitted_at_ns": 1,
                    "started_at_ns": 2,
                    "terminal_at_ns": 3,
                    "outcome": "completed",
                    "attempts": 1,
                }
            ],
        }
        self.assertTrue(rsb.validate_open_loop_ledger(run, counts))
