#!/usr/bin/env python3
"""Focused predecessor CAS, trusted-review, and immutable-generation transition tests."""

from __future__ import annotations

import copy
import dataclasses
import hashlib
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from typing import Any

import bootstrap_execution
import compile_plan_authority
import execution_state_v2 as v2
import transition_execution_authority as transition
from test_compile_plan_authority import GitFixture

ROOT = Path(__file__).resolve().parents[4]
REF = "refs/heads/codex/tracedecay-total-redesign-plan"
SOURCE_PATH = Path(__file__).with_name("staged_dispatch_pr1.json")


def make_review(candidate: dict[str, Any]) -> dict[str, Any]:
    authority = candidate["authority_transition"]
    review = {
        "schema": v2.REVIEW_SCHEMA,
        "receipt_id": "review:pr1-stage:independent",
        "candidate_state_digest": authority["candidate_state_digest"],
        "packet_source_blob_oid": authority["packet_source_blob_oid"],
        "packet_source_digest": authority["packet_source_digest"],
        "prior_generation": authority["expected_prior_generation"],
        "prior_state_sha256": authority["prior_state_sha256"],
        "prior_graph_revision": authority["prior_graph_revision"],
        "prior_graph_digest": authority["prior_graph_digest"],
        "reviewer": "independent-authority-reviewer",
        "reviewer_principal": "principal:authority-review",
        "reviewer_authority": "authority:independent-review",
        "implementation_authority": "authority:gpt-5.6-sol-lifecycle",
        "independent": True,
        "verdict": "approved",
        "reviewed_at": "2026-07-13T12:00:00Z",
        "receipt_digest": "",
    }
    review["receipt_digest"] = v2.authority_review_digest(review)
    return review


class TransitionHarness:
    def __init__(self) -> None:
        self.fixture = GitFixture()
        checked_source = self.fixture.root / transition.PACKET_SOURCE_PATH
        checked_source.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(SOURCE_PATH, checked_source)
        self.fixture.git("add", transition.PACKET_SOURCE_PATH.as_posix())
        self.fixture.git("commit", "-m", "test: add reviewed staged source")
        compiled, live = compile_plan_authority.compile_from_ref(
            self.fixture.root, "refs/heads/main", revision=5
        )
        state_bytes = compile_plan_authority._canonical_json_bytes(compiled.state)
        self.compiled = compiled
        self.live = live
        self.source = transition.load_reviewed_source(
            self.fixture.root, live.canonical_commit or ""
        )
        manifest_hex = hashlib.sha256(
            compile_plan_authority._canonical_json_bytes(compiled.manifest)
        ).hexdigest()
        state_hex = hashlib.sha256(state_bytes).hexdigest()
        generation = f"r5-{manifest_hex[:16]}-{state_hex[:16]}"
        self.predecessor = transition.Predecessor(
            generation=generation,
            pointer_bytes=b"",
            manifest=compiled.manifest,
            state=compiled.state,
            state_sha256="sha256:" + hashlib.sha256(state_bytes).hexdigest(),
            live=live,
        )

    def candidate(self) -> dict[str, Any]:
        candidate = transition.build_candidate(
            self.predecessor, self.source, activated_at="2026-07-13T12:00:00Z"
        )
        candidate["authority_transition"]["authority_review"] = make_review(candidate)
        return candidate

    def install_root(self) -> tuple[tempfile.TemporaryDirectory[str], Path, transition.Predecessor]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        generation = self.predecessor.generation
        directory = root / bootstrap_execution.GENERATIONS / generation
        directory.mkdir(parents=True)
        manifest_bytes = compile_plan_authority._canonical_json_bytes(self.compiled.manifest)
        state_bytes = compile_plan_authority._canonical_json_bytes(self.compiled.state)
        (directory / "manifest.json").write_bytes(manifest_bytes)
        (directory / "state.json").write_bytes(state_bytes)
        pointer = {
            "schema": bootstrap_execution.POINTER_SCHEMA,
            "generation": generation,
            "manifest": f"v2-execution-generations/{generation}/manifest.json",
            "state": f"v2-execution-generations/{generation}/state.json",
            "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
            "state_sha256": hashlib.sha256(state_bytes).hexdigest(),
        }
        pointer_bytes = compile_plan_authority._canonical_json_bytes(pointer)
        active = root / bootstrap_execution.ACTIVE_POINTER
        active.parent.mkdir(parents=True, exist_ok=True)
        active.write_bytes(pointer_bytes)
        predecessor = transition.Predecessor(
            generation=generation,
            pointer_bytes=pointer_bytes,
            manifest=self.compiled.manifest,
            state=self.compiled.state,
            state_sha256=self.predecessor.state_sha256,
            live=self.live,
        )
        return temporary, root, predecessor


HARNESS: TransitionHarness


def setUpModule() -> None:
    global HARNESS
    HARNESS = TransitionHarness()


def tearDownModule() -> None:
    HARNESS.fixture.close()


class ReviewedSourceTests(unittest.TestCase):
    def test_checked_source_matches_revision5_manifest_and_exact_pr1_authority(self) -> None:
        self.assertEqual(
            HARNESS.source.blob_sha256,
            v2.packet_source_digest(HARNESS.source.raw_bytes),
        )
        transition._validate_packet_against_manifest(
            HARNESS.source.document, HARNESS.compiled.manifest
        )
        self.assertEqual(HARNESS.source.document["authorized_slice_ids"], ["PR 1"])
        self.assertEqual(HARNESS.source.document["authority_revision"], 6)
        self.assertEqual(HARNESS.source.document["checked_manifest_revision"], 5)

    def test_revision_skip_manifest_drift_and_packet_drift_are_rejected(self) -> None:
        cases = []
        skipped = copy.deepcopy(HARNESS.source.document)
        skipped["authority_revision"] = 7
        cases.append((dataclasses.replace(HARNESS.source, document=skipped), "exact predecessor successor"))
        stale_manifest = copy.deepcopy(HARNESS.source.document)
        stale_manifest["checked_manifest_digest"] = "sha256:" + "0" * 64
        cases.append((dataclasses.replace(HARNESS.source, document=stale_manifest), "manifest digest"))
        forged_packet = copy.deepcopy(HARNESS.source.document)
        forged_packet["packet"]["content_digest"] = "sha256:" + "1" * 64
        cases.append((dataclasses.replace(HARNESS.source, document=forged_packet), "complete packet bytes"))
        for source, expected in cases:
            with self.subTest(expected=expected):
                with self.assertRaisesRegex(ValueError, expected):
                    transition.build_candidate(
                        HARNESS.predecessor, source, activated_at="2026-07-13T12:00:00Z"
                    )


class TrustedReviewTests(unittest.TestCase):
    def test_review_must_be_regular_observed_and_exactly_bound(self) -> None:
        candidate = transition.build_candidate(
            HARNESS.predecessor, HARNESS.source, activated_at="2026-07-13T12:00:00Z"
        )
        review = make_review(candidate)
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "review.json"
            receipt.write_text(json.dumps(review), encoding="utf-8")
            os.chmod(receipt, 0o600)
            observed = frozenset({review["receipt_digest"]})
            self.assertEqual(
                transition.load_authority_review(receipt, candidate, observed), review
            )

            with self.assertRaisesRegex(ValueError, "absent from trusted observation set"):
                transition.load_authority_review(receipt, candidate, frozenset())

            review["candidate_state_digest"] = "sha256:" + "2" * 64
            receipt.write_text(json.dumps(review), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "candidate_state_digest mismatch"):
                transition.load_authority_review(receipt, candidate, observed)

    def test_missing_changed_or_self_review_receipt_cannot_activate_candidate(self) -> None:
        candidate = transition.build_candidate(
            HARNESS.predecessor, HARNESS.source, activated_at="2026-07-13T12:00:00Z"
        )
        review = make_review(candidate)
        review["reviewer_authority"] = review["implementation_authority"]
        review["receipt_digest"] = v2.authority_review_digest(review)
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "review.json"
            receipt.write_text(json.dumps(review), encoding="utf-8")
            os.chmod(receipt, 0o600)
            with self.assertRaisesRegex(ValueError, "must differ"):
                transition.load_authority_review(
                    receipt, candidate, frozenset({review["receipt_digest"]})
                )

    def test_fixed_observation_ledger_is_owner_only_and_not_caller_selected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger = root / transition.live_evidence.AUTHORITY_REVIEW_OBSERVATIONS
            ledger.parent.mkdir(parents=True)
            ledger.write_text(json.dumps({
                "schema": transition.live_evidence.AUTHORITY_REVIEW_OBSERVATIONS_SCHEMA,
                "receipt_digests": ["sha256:" + "1" * 64],
            }), encoding="utf-8")
            os.chmod(ledger, 0o600)
            self.assertEqual(
                transition.live_evidence.load_authority_review_observations(root),
                frozenset({"sha256:" + "1" * 64}),
            )
            os.chmod(ledger, 0o644)
            with self.assertRaisesRegex(ValueError, "mode 0600"):
                transition.live_evidence.load_authority_review_observations(root)


class AtomicTransitionTests(unittest.TestCase):
    def test_loads_stored_predecessor_for_end_to_end_exact_replay(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name) / "checkout"
        shutil.copytree(HARNESS.fixture.root, root)
        generation = HARNESS.predecessor.generation
        directory = root / bootstrap_execution.GENERATIONS / generation
        directory.mkdir(parents=True)
        manifest_bytes = compile_plan_authority._canonical_json_bytes(HARNESS.compiled.manifest)
        state_bytes = compile_plan_authority._canonical_json_bytes(HARNESS.compiled.state)
        (directory / "manifest.json").write_bytes(manifest_bytes)
        (directory / "state.json").write_bytes(state_bytes)
        pointer = {
            "schema": bootstrap_execution.POINTER_SCHEMA,
            "generation": generation,
            "manifest": f"v2-execution-generations/{generation}/manifest.json",
            "state": f"v2-execution-generations/{generation}/state.json",
            "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
            "state_sha256": hashlib.sha256(state_bytes).hexdigest(),
        }
        active = root / bootstrap_execution.ACTIVE_POINTER
        active.parent.mkdir(parents=True, exist_ok=True)
        active.write_bytes(compile_plan_authority._canonical_json_bytes(pointer))

        predecessor = transition.load_predecessor(root, "refs/heads/main", generation)
        source = transition.load_reviewed_source(root, predecessor.live.canonical_commit or "")
        candidate = transition.build_candidate(
            predecessor, source, activated_at="2026-07-13T12:00:00Z"
        )
        candidate["authority_transition"]["authority_review"] = make_review(candidate)
        state_path, _ = transition._install(root, predecessor, candidate)
        first_pointer = active.read_bytes()

        replay_predecessor = transition.load_predecessor(root, "refs/heads/main", generation)
        replay = transition.build_candidate(
            replay_predecessor, source, activated_at="2026-07-13T12:00:00Z"
        )
        replay["authority_transition"]["authority_review"] = make_review(replay)
        replay_path, _ = transition._install(root, replay_predecessor, replay)
        self.assertEqual(replay_path, state_path)
        self.assertEqual(active.read_bytes(), first_pointer)

    def test_success_switches_generation_and_exact_replay_preserves_pointer_bytes(self) -> None:
        temporary, root, predecessor = HARNESS.install_root()
        self.addCleanup(temporary.cleanup)
        candidate = transition.build_candidate(
            predecessor, HARNESS.source, activated_at="2026-07-13T12:00:00Z"
        )
        candidate["authority_transition"]["authority_review"] = make_review(candidate)
        state_path, active = transition._install(root, predecessor, candidate)
        first_pointer = active.read_bytes()
        self.assertEqual(json.loads(state_path.read_text())["canonical_dag"]["graph_revision"], 6)
        self.assertEqual(
            json.loads((state_path.parent / "manifest.json").read_text())["graph_revision"], 5
        )
        replay_state, replay_active = transition._install(root, predecessor, candidate)
        self.assertEqual(replay_state, state_path)
        self.assertEqual(replay_active.read_bytes(), first_pointer)

    def test_stale_or_concurrent_predecessor_cannot_overwrite_active_pointer(self) -> None:
        temporary, root, predecessor = HARNESS.install_root()
        self.addCleanup(temporary.cleanup)
        candidate = transition.build_candidate(
            predecessor, HARNESS.source, activated_at="2026-07-13T12:00:00Z"
        )
        candidate["authority_transition"]["authority_review"] = make_review(candidate)
        active = root / bootstrap_execution.ACTIVE_POINTER
        changed = json.loads(active.read_text())
        changed["generation"] = "r5-concurrent-winner"
        active.write_bytes(compile_plan_authority._canonical_json_bytes(changed))
        before = active.read_bytes()
        with self.assertRaisesRegex(ValueError, "changed before compare-and-swap"):
            transition._install(root, predecessor, candidate)
        self.assertEqual(active.read_bytes(), before)

    def test_same_target_revision_with_different_bytes_cannot_replay(self) -> None:
        temporary, root, predecessor = HARNESS.install_root()
        self.addCleanup(temporary.cleanup)
        candidate = transition.build_candidate(
            predecessor, HARNESS.source, activated_at="2026-07-13T12:00:00Z"
        )
        candidate["authority_transition"]["authority_review"] = make_review(candidate)
        transition._install(root, predecessor, candidate)
        conflicting = copy.deepcopy(candidate)
        conflicting["authority_transition"]["activated_at"] = "2026-07-13T12:01:00Z"
        conflicting["authority_transition"]["candidate_state_digest"] = v2.candidate_state_digest(conflicting)
        conflicting["authority_transition"]["authority_review"] = make_review(conflicting)
        with self.assertRaisesRegex(ValueError, "changed before compare-and-swap"):
            transition._install(root, predecessor, conflicting)


if __name__ == "__main__":
    unittest.main()
