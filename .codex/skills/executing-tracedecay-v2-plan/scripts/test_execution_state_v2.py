#!/usr/bin/env python3
"""Focused staged-dispatch schema, partition, packet, and rendering contracts."""

from __future__ import annotations

import copy
import dataclasses
import hashlib
import html
import json
import shutil
import unittest
from pathlib import Path
from typing import Any

import compile_plan_authority
import execution_state as v1
import execution_state_v2 as v2
import plan_execution
import transition_execution_authority as transition
from test_compile_plan_authority import GitFixture

ROOT = Path(__file__).resolve().parents[4]
REF = "refs/heads/codex/tracedecay-total-redesign-plan"
SOURCE = Path(__file__).with_name("staged_dispatch_pr1.json")


def reseal_local_state(candidate: dict[str, Any]) -> None:
    graph = candidate["canonical_dag"]
    policy = candidate["dispatch_policy"]
    packets = {item["slice_id"]: item for item in candidate["dispatch_specs"]}
    blocks = {item["slice_id"]: item for item in candidate["dispatch_blocks"]}
    entries: dict[str, str] = {}
    for node in graph["nodes"]:
        if node["id"] in packets:
            digest = v2.dispatch_entry_digest(
                kind="authorized_packet", slice_id=node["id"], payload=packets[node["id"]],
                graph=graph, policy=policy,
            )
        else:
            digest = v2.dispatch_entry_digest(
                kind="blocked_node", slice_id=node["id"], payload=blocks[node["id"]],
                graph=graph, policy=policy,
            )
        node["dispatch_digest"] = digest
        entries[node["id"]] = digest
    graph["dispatch_contract_set_digest"] = v2.dispatch_contract_set_digest(entries)
    graph["graph_digest"] = v2.graph_digest(graph)
    receipt = graph["activation_receipt"]
    receipt["graph_digest"] = graph["graph_digest"]
    receipt["dispatch_contract_set_digest"] = graph["dispatch_contract_set_digest"]
    candidate["completion_ledger"]["graph_digest"] = graph["graph_digest"]
    authority = candidate["authority_transition"]
    authority["target_graph_digest"] = graph["graph_digest"]
    authority["dispatch_contract_set_digest"] = graph["dispatch_contract_set_digest"]
    authority["authority_review"] = None
    authority["candidate_state_digest"] = v2.candidate_state_digest(candidate)
    authority["authority_review"] = review_for(candidate)


def review_for(candidate: dict[str, Any]) -> dict[str, Any]:
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


class StagedAuthorityHarness:
    def __init__(self) -> None:
        self.fixture = GitFixture()
        checked_source = self.fixture.root / transition.PACKET_SOURCE_PATH
        checked_source.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(SOURCE, checked_source)
        self.fixture.git("add", transition.PACKET_SOURCE_PATH.as_posix())
        self.fixture.git("commit", "-m", "test: add reviewed staged source")
        compiled, live = compile_plan_authority.compile_from_ref(
            self.fixture.root, "refs/heads/main", revision=6
        )
        self.compiled = compiled
        self.live = live
        state_bytes = compile_plan_authority._canonical_json_bytes(compiled.state)
        self.predecessor = transition.Predecessor(
            generation="r6-test-predecessor",
            pointer_bytes=b"{}\n",
            manifest=compiled.manifest,
            state=compiled.state,
            state_sha256="sha256:" + hashlib.sha256(state_bytes).hexdigest(),
            live=live,
        )
        self.source = transition.load_reviewed_source(
            self.fixture.root, live.canonical_commit or ""
        )

    def candidate(self) -> dict[str, Any]:
        candidate = transition.build_candidate(
            self.predecessor,
            self.source,
            activated_at="2026-07-13T12:00:00Z",
        )
        candidate["authority_transition"]["authority_review"] = review_for(candidate)
        return candidate

    def view(self, candidate: dict[str, Any], *, trust_review: bool = True) -> dict[str, Any]:
        review = candidate.get("authority_transition", {}).get("authority_review")
        receipts = (
            frozenset({review["receipt_digest"]})
            if trust_review and isinstance(review, dict)
            else frozenset()
        )
        live = dataclasses.replace(self.live, authority_review_receipts=receipts)
        return plan_execution.analyze(candidate, live)


HARNESS: StagedAuthorityHarness


def setUpModule() -> None:
    global HARNESS
    HARNESS = StagedAuthorityHarness()


def tearDownModule() -> None:
    HARNESS.fixture.close()


class PositiveStagedDispatchTests(unittest.TestCase):
    def test_exactly_pr1_is_ready_and_every_other_slice_is_explicitly_blocked(self) -> None:
        candidate = HARNESS.candidate()
        view = HARNESS.view(candidate)
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual(view["schema"], v2.VIEW_SCHEMA)
        self.assertEqual([item["slice_id"] for item in view["next_ready"]], ["PR 1"])
        self.assertEqual(len(view["blocked"]), 256)
        self.assertEqual(len(view["execution_order"]), 257)
        self.assertTrue(
            all(item["reasons"] == [v2.BLOCK_REASON] for item in view["blocked"])
        )
        packet = view["next_ready"][0]
        self.assertEqual(packet["content_digest"], "sha256:76ade28de35388e20604f8aaf61bed4fe2563e5fdfd514d80f6a2cd1b5d03333")
        self.assertEqual(len(packet["acceptance"]), 8)
        self.assertEqual(len(packet["exact_files"]), 15)
        self.assertEqual(len(packet["required_tests"]), 10)
        self.assertEqual(packet["prerequisites"], [])
        self.assertEqual(packet["lane"]["lifecycle_owner"], "gpt-5.6-sol")
        self.assertFalse(packet["claude_adversarial_review"]["enabled"])
        self.assertEqual(packet["claude_adversarial_review"]["max_steps"], 0)
        self.assertEqual(packet["claude_adversarial_review"]["runtime"], "none")

    def test_json_and_markdown_expose_identical_fence_policy_and_blockers(self) -> None:
        view = HARNESS.view(HARNESS.candidate())
        rendered = v2.markdown(view)
        for field in (
            "graph_revision", "graph_digest", "source_commit", "source_set_digest",
            "dispatch_policy_digest", "packet_source_digest", "prior_generation",
            "prior_state_sha256",
        ):
            self.assertIn(json.dumps(view[field]), rendered)
        self.assertEqual(rendered.count(v2.BLOCK_REASON), 256)
        self.assertIn("### PR 1", rendered)
        for field in ("acceptance", "source_blocks", "prohibitions"):
            for item in view["next_ready"][0][field]:
                self.assertIn(
                    html.escape(json.dumps(item, ensure_ascii=False), quote=False),
                    rendered,
                )

    def test_checked_packet_names_all_current_architecture_boundary_tests(self) -> None:
        packet_tests = set(HARNESS.source.document["packet"]["required_tests"])
        expected = {
            "architecture_manifest_has_bounded_acyclic_owners",
            "transports_are_isolated_from_storage_and_business_implementations",
            "generated_views_and_release_gates_are_checked_in",
            "machine_authority_has_complete_governance_schema",
            "cargo_and_source_policy_enforce_materialized_boundaries",
            "forbidden_source_pattern_is_rejected_by_focused_fixture",
            "dependency_policy_generator_escapes_apostrophes_as_valid_toml",
            "forbidden_owner_import_is_rejected_by_focused_fixture",
            "forbidden_real_cargo_edge_is_rejected_by_focused_fixture",
            "seven_v2_adrs_lock_the_phase_zero_decisions",
        }
        self.assertEqual(packet_tests, expected)
        source = (ROOT / "tests/architecture_boundaries.rs").read_text(encoding="utf-8")
        for name in expected:
            self.assertIn(f"fn {name}()", source)


class FailClosedPartitionTests(unittest.TestCase):
    def assert_invalid(self, candidate: dict[str, Any], text: str | None = None) -> None:
        view = HARNESS.view(candidate)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])
        if text is not None:
            self.assertTrue(any(text in error for error in view["errors"]), view["errors"])

    def test_missing_duplicate_overlap_extra_packet_and_tampered_block_suppress_all_packets(self) -> None:
        cases: list[tuple[dict[str, Any], str]] = []
        missing = HARNESS.candidate()
        missing["dispatch_blocks"].pop()
        cases.append((missing, "missing explicit packet/block"))

        duplicate = HARNESS.candidate()
        duplicate["dispatch_blocks"].append(copy.deepcopy(duplicate["dispatch_blocks"][0]))
        cases.append((duplicate, "duplicate block"))

        overlap = HARNESS.candidate()
        overlap["dispatch_blocks"].append({
            "slice_id": "PR 1",
            "stage_id": overlap["dispatch_policy"]["stage_id"],
            "reason_code": v2.BLOCK_REASON,
            "authority_revision": overlap["canonical_dag"]["graph_revision"],
        })
        cases.append((overlap, "overlapping packet/block"))

        extra = HARNESS.candidate()
        second = copy.deepcopy(extra["dispatch_specs"][0])
        second["slice_id"] = "PR 2"
        extra["dispatch_specs"].append(second)
        cases.append((extra, "exactly equal reviewed authorized"))

        tampered = HARNESS.candidate()
        tampered["dispatch_blocks"][0]["stage_id"] = "forged-stage"
        cases.append((tampered, "stage_id"))

        for candidate, expected in cases:
            with self.subTest(expected=expected):
                self.assert_invalid(candidate, expected)

    def test_root_order_never_synthesizes_a_missing_packet(self) -> None:
        candidate = HARNESS.candidate()
        candidate["dispatch_specs"] = []
        self.assertEqual(candidate["canonical_dag"]["nodes"][0]["dependencies"], [])
        self.assert_invalid(candidate, "exactly one PR 1 packet")

    def test_unknown_schema_reports_explicit_version_error_and_empty_ready_set(self) -> None:
        view = plan_execution.analyze({"schema": "tracedecay.v2.execution-state/v999"}, HARNESS.live)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])
        self.assertIn("unsupported execution-state schema/version", view["errors"][0])

    def test_malformed_numeric_types_never_escape_analyze(self) -> None:
        fields = (
            ("canonical_dag", "graph_revision"),
            ("canonical_dag", "activation_receipt", "graph_revision"),
            ("canonical_dag", "activation_receipt", "slice_count"),
            ("canonical_dag", "activation_receipt", "edge_count"),
            ("canonical_dag", "activation_receipt", "authorized_count"),
            ("canonical_dag", "activation_receipt", "blocked_count"),
            ("dispatch_policy", "authority_revision"),
            ("dispatch_policy", "checked_manifest_revision"),
            ("dispatch_specs", 0, "source_blocks", 0, "start_line"),
            ("dispatch_specs", 0, "source_blocks", 0, "end_line"),
            ("dispatch_specs", 0, "claude_adversarial_review", "max_steps"),
            ("dispatch_blocks", 0, "authority_revision"),
            ("authority_transition", "checked_manifest_revision"),
            ("authority_transition", "prior_graph_revision"),
            ("authority_transition", "target_graph_revision"),
            ("authority_transition", "dispatch_blocks_count"),
            ("authority_transition", "activation_sequence"),
            ("authority_transition", "authority_review", "prior_graph_revision"),
        )
        malformed = (True, "1", 1.0, None, [])

        for index, path in enumerate(fields):
            with self.subTest(path=path):
                candidate = HARNESS.candidate()
                target: Any = candidate
                for part in path[:-1]:
                    target = target[part]
                target[path[-1]] = malformed[index % len(malformed)]
                view = plan_execution.analyze(candidate, HARNESS.live)
                self.assertFalse(view["valid"])
                self.assertEqual(view["next_ready"], [])


class PacketAndFenceTamperTests(unittest.TestCase):
    def assert_invalid(self, candidate: dict[str, Any], text: str) -> None:
        view = HARNESS.view(candidate)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])
        self.assertTrue(any(text in error for error in view["errors"]), view["errors"])

    def test_every_packet_contract_family_is_sealed(self) -> None:
        mutations = {
            "owner": lambda packet: packet.__setitem__("owner", "forged-owner"),
            "content": lambda packet: packet.__setitem__("content_digest", "sha256:" + "0" * 64),
            "acceptance_id": lambda packet: packet["acceptance"][0].__setitem__("criterion_id", "forged"),
            "acceptance_text": lambda packet: packet["acceptance"][0].__setitem__("text", "forged"),
            "acceptance_anchor": lambda packet: packet["acceptance"][0]["source_anchors"].append("forged"),
            "source_block": lambda packet: packet["source_blocks"][0].__setitem__("end_line", 1),
            "file": lambda packet: packet["exact_files"].__setitem__(0, "forged"),
            "command": lambda packet: packet["acceptance_commands"].__setitem__(0, "true"),
            "test": lambda packet: packet["required_tests"].__setitem__(0, "forged"),
            "workspace": lambda packet: packet["workspace"].__setitem__("branch", "forged"),
            "lane": lambda packet: packet["lane"].__setitem__("lifecycle_owner", "claude"),
            "claude": lambda packet: packet["claude_adversarial_review"].__setitem__("enabled", True),
            "gate": lambda packet: packet["gates"].__setitem__("integration", packet["gates"]["implementation"]),
            "prohibition": lambda packet: packet["prohibitions"].pop(),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                candidate = HARNESS.candidate()
                mutate(candidate["dispatch_specs"][0])
                self.assert_invalid(candidate, "sealed packet digest mismatch")

    def test_recomputed_local_hashes_cannot_replace_checked_packet_source(self) -> None:
        candidate = HARNESS.candidate()
        candidate["dispatch_specs"][0]["exact_files"][0] = "forged-local-file"
        candidate["dispatch_specs"][0]["exact_files"].sort()
        reseal_local_state(candidate)
        self.assert_invalid(candidate, "complete PR 1 packet bytes differ from checked source")

    def test_packet_source_is_pinned_to_exact_git_blob_and_raw_bytes(self) -> None:
        candidate = HARNESS.candidate()
        policy = candidate["dispatch_policy"]
        self.assertNotIn("packet_source_digest", HARNESS.source.document)
        self.assertEqual(policy["packet_source_blob_oid"], HARNESS.source.blob_oid)
        self.assertEqual(policy["packet_source_digest"], HARNESS.source.blob_sha256)
        self.assertEqual(
            v2.packet_contract_bytes(candidate["dispatch_specs"][0]),
            HARNESS.source.packet_bytes,
        )

        candidate["dispatch_policy"]["packet_source_blob_oid"] = "0" * 40
        reseal_local_state(candidate)
        self.assert_invalid(candidate, "differs from canonical Git blob")

    def test_candidate_digest_and_every_predecessor_fence_are_sealed(self) -> None:
        fields = {
            "expected_prior_generation": "forged",
            "prior_state_sha256": "sha256:" + "1" * 64,
            "prior_graph_revision": 4,
            "prior_graph_digest": "sha256:" + "2" * 64,
            "source_commit": "0" * 40,
            "source_set_digest": "sha256:" + "3" * 64,
            "manifest_digest": "sha256:" + "4" * 64,
            "checked_manifest_revision": 4,
            "target_graph_revision": 9,
            "packet_source_blob_oid": "6" * 40,
            "packet_source_digest": "sha256:" + "5" * 64,
        }
        for field, value in fields.items():
            with self.subTest(field=field):
                candidate = HARNESS.candidate()
                candidate["authority_transition"][field] = value
                self.assert_invalid(candidate, "authority_transition")

    def test_missing_unobserved_or_self_authored_review_suppresses_packet(self) -> None:
        missing = HARNESS.candidate()
        missing["authority_transition"]["authority_review"] = None
        self.assert_invalid(missing, "authority_review")

        unobserved = HARNESS.candidate()
        view = HARNESS.view(unobserved, trust_review=False)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])
        self.assertTrue(
            any("trusted authority-review observations" in error for error in view["errors"]),
            view["errors"],
        )

        self_review = HARNESS.candidate()
        review = self_review["authority_transition"]["authority_review"]
        review["reviewer_authority"] = review["implementation_authority"]
        review["receipt_digest"] = v2.authority_review_digest(review)
        self.assert_invalid(self_review, "reviewer authority must be distinct")

    def test_transition_timestamps_and_sequence_are_strict(self) -> None:
        cases = {
            "activated_at": "2026-07-13 12:00:00",
            "reviewed_at": "2026-07-13T12:00:00+00:00",
            "activation_sequence": 2,
        }
        for field, value in cases.items():
            with self.subTest(field=field):
                candidate = HARNESS.candidate()
                target = (
                    candidate["authority_transition"]["authority_review"]
                    if field == "reviewed_at"
                    else candidate["authority_transition"]
                )
                target[field] = value
                if field == "reviewed_at":
                    target["receipt_digest"] = v2.authority_review_digest(target)
                self.assert_invalid(candidate, field)


class V1CompatibilityTests(unittest.TestCase):
    def test_v1_verify_only_compiler_output_remains_v1_and_packet_free(self) -> None:
        state = HARNESS.compiled.state
        self.assertEqual(state["schema"], v1.EXPORT_SCHEMA)
        self.assertEqual(state["activation_mode"], "verify_only")
        self.assertEqual(state["dispatch_specs"], [])
        self.assertEqual(state["completion_ledger"]["entries"], [])
        view = plan_execution.analyze(state, HARNESS.live)
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual(view["schema"], v1.VIEW_SCHEMA)
        self.assertEqual(view["next_ready"], [])
        self.assertEqual(len(view["execution_order"]), 257)


if __name__ == "__main__":
    unittest.main()
