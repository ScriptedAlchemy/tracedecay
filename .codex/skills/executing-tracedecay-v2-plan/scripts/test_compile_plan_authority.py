#!/usr/bin/env python3
"""Contract tests for immutable checked V2 plan-authority compilation."""

from __future__ import annotations

import copy
import hashlib
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import bootstrap_execution
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
            self.assertTrue(record.owner.path, slice_id)
            self.assertGreater(record.owner.start_line, 0, slice_id)
            self.assertGreaterEqual(record.owner.end_line, record.owner.start_line, slice_id)
            self.assertRegex(record.owner.block_sha256, r"^[0-9a-f]{64}$", slice_id)
            self.assertTrue(record.owner_heading, slice_id)
            self.assertIn(record.owner.ref(), {anchor.ref() for anchor in record.source_anchors})
            self.assertIn(record.phase, range(6), slice_id)
            self.assertTrue(record.commit_subject, slice_id)
            self.assertTrue(record.acceptance, slice_id)
            self.assertTrue(all(criterion.source_anchors for criterion in record.acceptance), slice_id)
            self.assertTrue(all(dependency.source_anchors for dependency in record.dependencies), slice_id)

    def test_corrected_critical_edges_remain_checked(self) -> None:
        required = {
            "PR 35H": {"PR 35G", "PR 35I", "PR 35J"},
            "PR 36S": {"PR 33I", "PR 35"},
            "PR 37L": {"PR 36S", "PR 37K"},
            "PR 38I": {"PR 38D", "PR 24F", "PR 24P", "PR 24S", "PR 22F-LE", "PR 30J"},
            "PR 38K": {"PR 38J", "PR 33", "PR 34", "PR 35", "PR 36"},
        }
        for child, parents in required.items():
            self.assertTrue(parents <= set(self.registry.slices[child].parent_ids))

    def test_typed_edges_and_representative_provenance_are_checked(self) -> None:
        for slice_id in ("PR 4E", "PR 14A", "PR 38I"):
            record = self.registry.slices[slice_id]
            source_refs = {anchor.ref() for anchor in record.source_anchors}
            self.assertTrue(record.acceptance, slice_id)
            for dependency in record.dependencies:
                self.assertEqual(dependency.kind, "requires_success", slice_id)
                self.assertEqual(dict(dependency.payload), {}, slice_id)
                self.assertTrue(set(dependency.source_anchors) <= source_refs, slice_id)

    def _rejects_registry_mutation(self, mutate, message: str) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / compiler.REGISTRY_PATH
            destination.parent.mkdir(parents=True)
            document = json.loads((ROOT / compiler.REGISTRY_PATH).read_text())
            mutate(document)
            destination.write_text(json.dumps(document), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, message):
                compiler.load_registry(root)

    def test_v1_registry_is_rejected(self) -> None:
        self._rejects_registry_mutation(
            lambda document: document.__setitem__(
                "schema", "tracedecay.v2.plan-authority-registry/v1"
            ),
            "registry.schema",
        )

    def test_string_dependency_is_rejected(self) -> None:
        self._rejects_registry_mutation(
            lambda document: document["slices"]["PR 10"].__setitem__(
                "dependencies", ["PR 9"]
            ),
            "expected fields",
        )

    def test_zero_acceptance_is_rejected(self) -> None:
        self._rejects_registry_mutation(
            lambda document: document["slices"]["PR 1"].__setitem__("acceptance", []),
            "non-empty array",
        )

    def test_edge_payload_is_typed(self) -> None:
        def mutate(document: dict) -> None:
            dependency = document["slices"]["PR 10"]["dependencies"][0]
            dependency["payload"] = {"unexpected": True}

        self._rejects_registry_mutation(mutate, "invalid typed payload")

    def test_edge_provenance_is_required(self) -> None:
        def mutate(document: dict) -> None:
            dependency = document["slices"]["PR 10"]["dependencies"][0]
            dependency["source_anchors"] = []

        self._rejects_registry_mutation(mutate, "source_anchors")

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

    def test_compilation_is_byte_stable(self) -> None:
        again, _ = compiler.compile_from_ref(self.fixture.root, "refs/heads/main")
        self.assertEqual(
            compiler._canonical_json_bytes(again.manifest),
            compiler._canonical_json_bytes(self.compiled.manifest),
        )
        self.assertEqual(
            compiler._canonical_json_bytes(again.state),
            compiler._canonical_json_bytes(self.compiled.state),
        )

    def test_stale_checked_owner_anchor_is_rejected(self) -> None:
        fixture = GitFixture()
        try:
            registry_path = fixture.root / compiler.REGISTRY_PATH
            document = json.loads(registry_path.read_text())
            owner = document["slices"]["PR 1"]["owner"]
            plan_path = fixture.root / owner["path"]
            lines = plan_path.read_text().splitlines()
            lines[owner["start_line"]] += " drift"
            plan_path.write_text("\n".join(lines) + "\n")
            fixture.git("add", owner["path"])
            fixture.git("commit", "-m", "test: stale owner anchor")
            with self.assertRaisesRegex(ValueError, "checked owner anchor resolves 0"):
                compiler.compile_from_ref(fixture.root, "refs/heads/main")
        finally:
            fixture.close()

    def test_check_byte_compares_committed_canonical_manifest(self) -> None:
        process = subprocess.run(
            [
                "python3", str(Path(compiler.__file__)), "--root", str(self.fixture.root),
                "--canonical-ref", "refs/heads/main", "--check",
            ],
            check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.assertEqual(process.returncode, 0, process.stdout + process.stderr)

    def test_check_rejects_committed_canonical_manifest_mismatch(self) -> None:
        fixture = GitFixture()
        try:
            manifest = fixture.root / compiler.CANONICAL_MANIFEST_PATH
            manifest.write_text("{}\n", encoding="utf-8")
            fixture.git("add", compiler.CANONICAL_MANIFEST_PATH)
            fixture.git("commit", "-m", "test: stale authority")
            process = subprocess.run(
                [
                    "python3", str(Path(compiler.__file__)), "--root", str(fixture.root),
                    "--canonical-ref", "refs/heads/main", "--check",
                ],
                check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            self.assertEqual(process.returncode, 2, process.stdout + process.stderr)
            self.assertIn("canonical manifest mismatch", process.stdout)
        finally:
            fixture.close()


class BootstrapProvenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = GitFixture()
        self.compiled, _ = compiler.compile_from_ref(self.fixture.root, "refs/heads/main")
        self.manifest = self.fixture.root / "candidate-manifest.json"
        self.state = self.fixture.root / "candidate-state.json"
        compiler._atomic_json(self.manifest, self.compiled.manifest)
        compiler._atomic_json(self.state, self.compiled.state)

    def tearDown(self) -> None:
        self.fixture.close()

    def run_bootstrap(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "python3", str(Path(bootstrap_execution.__file__)),
                "--manifest", str(self.manifest), "--state-export", str(self.state),
                "--root", str(self.fixture.root), "--canonical-ref", "refs/heads/main",
            ],
            check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

    def test_compiler_exact_activation_and_identical_replay(self) -> None:
        first = self.run_bootstrap()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        pointer = self.fixture.root / bootstrap_execution.ACTIVE_POINTER
        before = pointer.read_bytes()
        replay = self.run_bootstrap()
        self.assertEqual(replay.returncode, 0, replay.stdout + replay.stderr)
        self.assertEqual(pointer.read_bytes(), before)

    def test_forged_node_order_is_rejected_before_activation(self) -> None:
        forged = copy.deepcopy(self.compiled.state)
        forged["canonical_dag"]["nodes"][0:2] = reversed(forged["canonical_dag"]["nodes"][0:2])
        compiler._atomic_json(self.state, forged)
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("supplied state bytes differ from canonical compiler output", result.stdout)
        self.assertFalse((self.fixture.root / bootstrap_execution.ACTIVE_POINTER).exists())

    def test_cas_rejects_revision_regression_and_equal_revision_different_bytes(self) -> None:
        revision_two, _ = compiler.compile_from_ref(
            self.fixture.root, "refs/heads/main", revision=2
        )
        bootstrap_execution._install_generation(
            self.fixture.root, revision_two.manifest, revision_two.state
        )
        with self.assertRaisesRegex(ValueError, "revision regression"):
            bootstrap_execution._install_generation(
                self.fixture.root, self.compiled.manifest, self.compiled.state
            )
        changed = copy.deepcopy(revision_two.state)
        changed["retired_obligations"].append("FM-999")
        with self.assertRaisesRegex(ValueError, "already activated with different bytes"):
            bootstrap_execution._install_generation(
                self.fixture.root, revision_two.manifest, changed
            )

    def test_receipt_binds_compiler_manifest_and_counts(self) -> None:
        receipt = self.compiled.state["canonical_dag"]["activation_receipt"]
        self.assertEqual(receipt["compiler_version"], compiler.COMPILER_VERSION)
        self.assertEqual(receipt["validator_version"], es.VALIDATOR_VERSION)
        self.assertEqual(receipt["slice_count"], 257)
        self.assertEqual(receipt["series_count"], 8)
        self.assertEqual(receipt["edge_count"], 1917)
        self.assertEqual(
            receipt["manifest_digest"],
            "sha256:" + hashlib.sha256(
                compiler._canonical_json_bytes(self.compiled.manifest)
            ).hexdigest(),
        )


if __name__ == "__main__":
    unittest.main()
