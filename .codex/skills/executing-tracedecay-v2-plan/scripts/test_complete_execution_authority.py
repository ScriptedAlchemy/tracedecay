#!/usr/bin/env python3
"""Completion-recorder and V2-to-V2 CAS contracts."""

from __future__ import annotations

import copy
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import bootstrap_execution
import complete_execution_authority as complete
import execution_state


def predecessor(_root: Path | None = None) -> complete.Predecessor:
    manifest = {"graph_revision": 7}
    state = {
        "canonical_dag": {"graph_revision": 7, "marker": "graph"},
        "completion_ledger": {"entries": []},
        "dispatch_policy": {"marker": "policy"},
        "dispatch_specs": [{
            "slice_id": "PR 1",
            "acceptance_commands": [complete.PR1_TEST_COMMAND, "git diff --check"],
            "required_tests": ["alpha", "beta"],
        }],
        "dispatch_blocks": [{"slice_id": f"PR {index}"} for index in range(2, 258)],
    }
    manifest_bytes = complete._bytes(manifest)
    state_bytes = complete._bytes(state)
    generation = (
        f"r7-{hashlib.sha256(manifest_bytes).hexdigest()[:16]}-"
        f"{hashlib.sha256(state_bytes).hexdigest()[:16]}"
    )
    pointer = complete._generation_pointer(generation, manifest_bytes, state_bytes)
    return complete.Predecessor(
        generation=generation,
        pointer_bytes=complete._bytes(pointer),
        manifest_bytes=manifest_bytes,
        state=state,
    )


class CompletionAuthorityTests(unittest.TestCase):
    def test_candidate_appends_only_pr1_and_preserves_partition(self) -> None:
        prior = predecessor()
        entry = {"slice_id": "PR 1", "evidence": "sealed"}
        candidate = complete.build_candidate(prior, entry)
        self.assertEqual(candidate["completion_ledger"]["entries"], [entry])
        for field in ("canonical_dag", "dispatch_policy", "dispatch_specs", "dispatch_blocks"):
            self.assertEqual(candidate[field], prior.state[field])
        self.assertEqual(len(candidate["dispatch_blocks"]), 256)

    def test_candidate_rejects_non_pr1_completion(self) -> None:
        with self.assertRaisesRegex(ValueError, "must be for PR 1"):
            complete.build_candidate(predecessor(), {"slice_id": "PR 2"})

    @mock.patch.object(complete.subprocess, "run")
    @mock.patch.object(complete, "_candidate_worktree")
    def test_recorder_runs_each_command_in_candidate_worktree_and_groups_tests(
        self, candidate_worktree: mock.Mock, run: mock.Mock
    ) -> None:
        def execute(*args: object, **kwargs: object) -> SimpleNamespace:
            output = kwargs["stdout"]
            command = args[0]
            if str(command).endswith("-- --list"):
                output.write(b"alpha: test\nbeta: test\nfocused_extra: test\n")
            elif command == complete.PR1_TEST_COMMAND:
                output.write(b"test alpha ... ok\ntest beta ... ok\ntest focused_extra ... ok\n")
            return SimpleNamespace(returncode=0)
        run.side_effect = execute
        candidate_worktree.return_value = Path("/candidate/worktree")
        packet = {
            "acceptance_commands": [complete.PR1_TEST_COMMAND, "git diff --check"],
            "required_tests": ["alpha", "beta"],
        }
        entry = {"candidate": {"commit": "a" * 40, "digest": "sha256:" + "b" * 64}}
        receipts = complete._run_acceptance(packet, entry)
        self.assertEqual(run.call_count, 3)
        self.assertTrue(all(call.kwargs["cwd"] == Path("/candidate/worktree") for call in run.call_args_list))
        self.assertEqual(receipts[0]["tests"], ["alpha", "beta"])
        self.assertEqual(receipts[1]["tests"], [])
        self.assertEqual([item["command"] for item in receipts], packet["acceptance_commands"])
        for receipt in receipts:
            self.assertEqual(receipt["receipt_digest"], execution_state.receipt_digest(receipt))

    def test_candidate_worktree_rejects_packet_mismatch(self) -> None:
        packet = {"workspace": {"branch": "expected", "worktree": "/expected"}}
        entry = {"candidate": {
            "branch": "other", "worktree": "/expected", "commit": "a" * 40,
        }}
        with self.assertRaisesRegex(ValueError, "branch differs"):
            complete._candidate_worktree(packet, entry)

    def test_forged_fixed_review_event_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            event_path = root / complete.REVIEW_EVENT
            event_path.parent.mkdir(parents=True)
            event_path.write_text(json.dumps({
                "schema": complete.REVIEW_EVENT_SCHEMA,
                "reviewer_task": complete.REVIEWER_TASK,
                "receipt_path": complete.REVIEW_RECEIPT.as_posix(),
                "receipt_digest": "sha256:" + "1" * 64,
                "candidate_commit": "a" * 40,
                "candidate_digest": "sha256:" + "b" * 64,
                "verdict": "approved",
                "observed_at": "2026-07-13T00:00:00Z",
                "event_digest": "sha256:" + "0" * 64,
            }), encoding="utf-8")
            event_path.chmod(0o600)
            receipt_path = root / complete.REVIEW_RECEIPT
            receipt_path.write_text("{}", encoding="utf-8")
            receipt_path.chmod(0o600)
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                complete._load_review_event(root, {
                    "candidate": {"commit": "a" * 40, "digest": "sha256:" + "b" * 64}
                })

    def test_review_event_rejects_non_owner_mode_and_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            event = root / complete.REVIEW_EVENT
            receipt = root / complete.REVIEW_RECEIPT
            event.parent.mkdir(parents=True)
            event.write_text("{}", encoding="utf-8")
            receipt.write_text("{}", encoding="utf-8")
            event.chmod(0o644)
            receipt.chmod(0o600)
            with self.assertRaisesRegex(ValueError, "owner-only"):
                complete._load_review_event(root, {"candidate": {}})
            event.unlink()
            event.symlink_to(receipt)
            with self.assertRaisesRegex(ValueError, "owner-only"):
                complete._load_review_event(root, {"candidate": {}})

    @mock.patch.object(complete.subprocess, "run")
    @mock.patch.object(complete, "_candidate_worktree")
    def test_missing_enumerated_test_blocks_receipts(
        self, candidate_worktree: mock.Mock, run: mock.Mock
    ) -> None:
        candidate_worktree.return_value = Path("/candidate/worktree")
        def list_only(*args: object, **kwargs: object) -> SimpleNamespace:
            kwargs["stdout"].write(b"alpha: test\n")
            return SimpleNamespace(returncode=0)
        run.side_effect = list_only
        with self.assertRaisesRegex(ValueError, "do not contain unique reviewed"):
            complete._run_acceptance(
                {
                    "acceptance_commands": [complete.PR1_TEST_COMMAND],
                    "required_tests": ["alpha", "beta"],
                },
                {"candidate": {"commit": "a" * 40, "digest": "sha256:" + "b" * 64}},
            )

    @mock.patch.object(complete, "_record_observations")
    @mock.patch.object(complete, "_candidate_worktree")
    @mock.patch.object(complete, "_run_acceptance")
    @mock.patch.object(complete, "_load_review_event")
    def test_post_command_worktree_mutation_blocks_observation_write(
        self, load_review: mock.Mock, run_acceptance: mock.Mock,
        candidate_worktree: mock.Mock, record: mock.Mock,
    ) -> None:
        review = {
            "review_task": "review", "reviewer": "reviewer",
            "reviewer_principal": "reviewer", "reviewer_authority": "review-authority",
            "implementation_authority": "implementation-authority", "independent": True,
            "verdict": "approved", "candidate_commit": "a" * 40,
            "candidate_digest": "sha256:" + "b" * 64, "receipt_digest": "", "anchors": ["a"],
        }
        review["receipt_digest"] = execution_state.receipt_digest(review)
        load_review.return_value = review
        run_acceptance.return_value = []
        candidate_worktree.side_effect = ValueError("candidate worktree HEAD, branch, or cleanliness mismatch")
        entry = {
            "test_receipts": [], "review": review,
            "task_lineage": {"implementation_actor": "implementer"},
        }
        with self.assertRaisesRegex(ValueError, "worktree HEAD"):
            complete._observe_completion(Path("/canonical"), {}, entry)
        record.assert_not_called()

    def test_recorder_rejects_caller_authored_test_receipts(self) -> None:
        with self.assertRaisesRegex(ValueError, "caller-authored"):
            complete._observe_completion(
                Path("/canonical"),
                {},
                {"test_receipts": [{"receipt_digest": "forged"}]},
            )

    def test_bootstrap_reconciliation_rejects_historical_attempt_claim(self) -> None:
        with self.assertRaisesRegex(ValueError, "historical attempt"):
            complete._bootstrap_reconciliation(
                predecessor(),
                {"attempt": {"attempt_id": "invented"}},
                mock.Mock(),
            )

    def test_bootstrap_reconciliation_is_deterministic_new_evidence(self) -> None:
        prior = predecessor()
        prior.state["canonical_dag"].update({
            "source_commit": "c" * 40,
            "source_set_digest": "sha256:" + "d" * 64,
            "graph_digest": "sha256:" + "e" * 64,
        })
        commit = "a" * 40
        digest = "sha256:" + "b" * 64
        entry = {
            "candidate": {"commit": commit, "digest": digest},
            "task_lineage": {"integration_task": ""},
            "attempt": None,
            "steering_directives": [],
            "steering_receipts": [],
            "integration": None,
        }
        ancestry = {"status": "ancestor", "command_exit_code": 0}
        live = SimpleNamespace(
            ancestry={commit: ancestry},
            canonical_ref="refs/heads/codex/tracedecay-total-redesign-plan",
        )
        first = complete._bootstrap_reconciliation(prior, entry, live)
        second = complete._bootstrap_reconciliation(prior, entry, live)
        self.assertEqual(first, second)
        self.assertTrue(first["attempt"]["attempt_id"].startswith("bootstrap-reconciliation:PR1:"))
        self.assertEqual(first["attempt"]["terminal_cas_sequence"], 0)
        self.assertEqual(first["steering_directives"], [])
        self.assertEqual(first["steering_receipts"], [])
        self.assertEqual(
            first["integration"]["receipt_digest"],
            execution_state.receipt_digest(first["integration"]),
        )

    def test_prepare_fence_rejects_stale_pointer_before_side_effects(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prior = predecessor()
            active = root / bootstrap_execution.ACTIVE_POINTER
            active.parent.mkdir(parents=True, exist_ok=True)
            active.write_text('{"generation":"stale"}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "neither expected predecessor"):
                complete._assert_active_fence(root, prior, None)

    def test_install_is_atomic_and_exact_replay_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prior = predecessor()
            prior_dir = root / bootstrap_execution.GENERATIONS / prior.generation
            prior_dir.mkdir(parents=True)
            (prior_dir / "manifest.json").write_bytes(prior.manifest_bytes)
            (prior_dir / "state.json").write_bytes(complete._bytes(prior.state))
            active = root / bootstrap_execution.ACTIVE_POINTER
            active.parent.mkdir(parents=True, exist_ok=True)
            active.write_bytes(prior.pointer_bytes)
            candidate = complete.build_candidate(
                prior, {"slice_id": "PR 1", "evidence": "sealed"}
            )

            state_path, pointer_path, replay = complete._install(root, prior, candidate, complete._bytes(candidate))
            self.assertFalse(replay)
            self.assertEqual(state_path.read_bytes(), complete._bytes(candidate))
            first_pointer = pointer_path.read_bytes()

            replay_state, replay_pointer, replay = complete._install(root, prior, candidate, complete._bytes(candidate))
            self.assertTrue(replay)
            self.assertEqual(replay_state, state_path)
            self.assertEqual(replay_pointer.read_bytes(), first_pointer)

    def test_install_rejects_stale_active_pointer_without_writing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prior = predecessor()
            active = root / bootstrap_execution.ACTIVE_POINTER
            active.parent.mkdir(parents=True, exist_ok=True)
            active.write_text(json.dumps({"generation": "stale"}), encoding="utf-8")
            candidate = complete.build_candidate(
                prior, {"slice_id": "PR 1", "evidence": "sealed"}
            )
            with self.assertRaisesRegex(ValueError, "changed before completion"):
                complete._install(root, prior, candidate, complete._bytes(candidate))
            self.assertFalse((root / bootstrap_execution.GENERATIONS).exists())


if __name__ == "__main__":
    unittest.main()
