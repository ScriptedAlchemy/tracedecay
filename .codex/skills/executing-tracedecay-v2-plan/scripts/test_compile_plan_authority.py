#!/usr/bin/env python3
"""Contract tests for immutable checked V2 plan-authority compilation."""

from __future__ import annotations

import copy
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import compile_plan_authority as compiler
import execution_state as es
import live_evidence
import slice_authority as sa


ROOT = Path(__file__).resolve().parents[4]


class GitFixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copytree(ROOT / "docs/plans", self.root / "docs/plans")
        registry = self.root / compiler.REGISTRY_PATH
        registry.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / compiler.REGISTRY_PATH, registry)
        self.git("init", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "TraceDecay Test")
        self.git("remote", "add", "origin", "https://example.invalid/tracedecay.git")
        self.git("add", ".")
        self.git("commit", "-m", "test: pin plans")
        self.commit = self.git("rev-parse", "HEAD").stdout.strip()

    def git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args], cwd=self.root, check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

    def close(self) -> None:
        self.temporary.cleanup()


class RegistryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.registry = compiler.load_registry(ROOT)

    def test_registry_has_only_executable_ids_and_explicit_series(self) -> None:
        self.assertEqual(len(self.registry.slices), 257)
        self.assertEqual(len(self.registry.series), 8)
        aggregate_ids = {series_id.removesuffix(" series") for series_id in self.registry.series}
        self.assertEqual(
            aggregate_ids,
            {"PR 13", "PR 14", "PR 23", "PR 24", "PR 24E", "PR 28", "PR 30", "PR 31"},
        )
        self.assertFalse(aggregate_ids & set(self.registry.slices))
        self.assertEqual(
            set(self.registry.series["PR 24E series"]),
            {"PR 24E-API5", *{f"PR 24E{value}" for value in range(9)}},
        )

    def test_every_slice_carries_checked_owner_phase_subject_and_dependencies(self) -> None:
        for slice_id, record in self.registry.slices.items():
            self.assertTrue(record.owner_path, slice_id)
            self.assertGreater(record.owner_line, 0, slice_id)
            self.assertTrue(record.owner_heading, slice_id)
            self.assertIn(record.phase, range(6), slice_id)
            self.assertTrue(record.commit_subject, slice_id)
            self.assertEqual(record.dependencies, tuple(sorted(set(record.dependencies))), slice_id)

    def test_corrected_critical_edges_remain_checked(self) -> None:
        required = {
            "PR 35H": {"PR 35G", "PR 35I", "PR 35J"},
            "PR 36S": {"PR 33I", "PR 35"},
            "PR 37L": {"PR 36S", "PR 37K"},
            "PR 38I": {"PR 38D", "PR 24F", "PR 24P", "PR 24S", "PR 22F-LE", "PR 30J"},
            "PR 38K": {"PR 38J", "PR 33", "PR 34", "PR 35", "PR 36"},
        }
        for child, parents in required.items():
            self.assertTrue(parents <= set(self.registry.slices[child].dependencies))

    def test_series_aggregate_cannot_be_reintroduced_as_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / compiler.REGISTRY_PATH
            destination.parent.mkdir(parents=True)
            document = json.loads((ROOT / compiler.REGISTRY_PATH).read_text())
            document["slices"]["PR 13"] = copy.deepcopy(document["slices"]["PR 13A"])
            destination.write_text(json.dumps(document), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "series aggregates are not executable"):
                compiler.load_registry(root)


class ImmutableCompileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.fixture = GitFixture()
        cls.compiled, cls.live = compiler.compile_from_ref(
            cls.fixture.root, "refs/heads/main"
        )

    @classmethod
    def tearDownClass(cls) -> None:
        cls.fixture.close()

    def test_manifest_round_trips_against_pinned_git_blobs(self) -> None:
        observations = live_evidence.source_set(self.fixture.root, self.fixture.commit)
        diagnostics = sa.reconcile_against_authority(
            self.compiled.records,
            self.compiled.manifest,
            "bootstrap",
            observations,
            self.compiled.registry.series,
        )
        self.assertEqual(diagnostics, [])

    def test_verify_only_state_has_complete_order_and_no_dispatch(self) -> None:
        validation = es.validate(self.compiled.state, self.live)
        self.assertTrue(validation.valid, validation.errors)
        view = es.next_ready(validation)
        self.assertEqual(view["activation_mode"], "verify_only")
        self.assertEqual(len(view["execution_order"]), 257)
        self.assertEqual(view["next_ready"], [])
        self.assertEqual(self.compiled.state["dispatch_specs"], [])
        self.assertEqual(self.compiled.state["completion_ledger"]["entries"], [])

    def test_hand_authored_dispatch_mode_is_not_valid_authority(self) -> None:
        forged = copy.deepcopy(self.compiled.state)
        forged["activation_mode"] = "dispatch"
        validation = es.validate(forged, self.live)
        self.assertFalse(validation.valid)
        self.assertTrue(any("missing bounded worker packet" in error for error in validation.errors))

    def test_dirty_working_tree_cannot_change_compiled_authority(self) -> None:
        master = self.fixture.root / compiler.MASTER_PATH
        master.write_text(master.read_text(encoding="utf-8") + "\nDIRTY SENTINEL\n", encoding="utf-8")
        (self.fixture.root / compiler.REGISTRY_PATH).write_text(
            '{"dirty":"not authority"}\n', encoding="utf-8"
        )
        after, live = compiler.compile_from_ref(self.fixture.root, "refs/heads/main")
        self.assertEqual(after.manifest, self.compiled.manifest)
        self.assertEqual(after.state, self.compiled.state)
        self.assertEqual(live.canonical_commit, self.fixture.commit)


if __name__ == "__main__":
    unittest.main()
