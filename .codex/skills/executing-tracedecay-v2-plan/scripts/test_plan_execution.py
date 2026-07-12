#!/usr/bin/env python3
"""Deterministic completion-ledger, live-evidence, and steering-fence contracts."""

from __future__ import annotations

import copy
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import execution_state as es
import live_evidence as le
import plan_execution


FIXTURES = Path(__file__).with_name("fixtures")
ROOT = Path(__file__).resolve().parents[4]


def load(name: str = "positive-ready.json") -> dict:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


def fake_live(document: dict, *, source_digest: str | None = None) -> le.LiveEvidence:
    graph = document["canonical_dag"]
    ancestry = {}
    for entry in document["completion_ledger"]["entries"]:
        integration = entry.get("integration")
        if integration is not None:
            ancestry[entry["candidate"]["commit"]] = copy.deepcopy(
                integration["ancestry_observation"]
            )
    return le.LiveEvidence(
        root=ROOT,
        repository=graph["repository"],
        canonical_ref="refs/heads/main",
        canonical_commit=graph["source_commit"],
        source_set_digest=source_digest or graph["source_set_digest"],
        ancestry=ancestry,
        errors=(),
    )


def analyze(document: dict, live: le.LiveEvidence | None = None) -> dict:
    return plan_execution.analyze(document, live or fake_live(document))


def reseal(document: dict) -> None:
    graph = document["canonical_dag"]
    graph["graph_digest"] = es.graph_digest(graph)
    graph["activation_receipt"]["graph_digest"] = graph["graph_digest"]
    ledger = document["completion_ledger"]
    ledger["graph_digest"] = graph["graph_digest"]
    for entry in ledger["entries"]:
        entry["graph_digest"] = graph["graph_digest"]
        candidate = entry["candidate"]
        candidate["digest"] = es.candidate_digest(candidate)
        review = entry.get("review")
        if review is not None:
            review["candidate_commit"] = candidate["commit"]
            review["candidate_digest"] = candidate["digest"]
            review["receipt_digest"] = es.receipt_digest(review)
        for receipt in entry["test_receipts"]:
            receipt["candidate_commit"] = candidate["commit"]
            receipt["candidate_digest"] = candidate["digest"]
            receipt["receipt_digest"] = es.receipt_digest(receipt)
        for receipt in entry["steering_receipts"]:
            receipt["receipt_digest"] = es.receipt_digest(receipt)
        integration = entry.get("integration")
        if integration is not None:
            integration["graph_digest"] = graph["graph_digest"]
            integration["receipt_digest"] = es.receipt_digest(integration)


class NextReadyTests(unittest.TestCase):
    def test_positive_fixture_selects_exact_bounded_packet(self) -> None:
        view = analyze(load())
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual([item["slice_id"] for item in view["next_ready"]], ["PR 2"])
        packet = view["next_ready"][0]
        self.assertEqual(packet["prerequisites"], ["PR 1"])
        self.assertEqual(packet["lane"]["reasoning_owner"], "gpt-5.6-sol")
        self.assertEqual(packet["optional_claude_review"]["max_steps"], 1)

    def test_candidate_only_is_valid_but_blocks_itself(self) -> None:
        document = load("negative-candidate-only.json")
        view = analyze(document)
        self.assertTrue(view["valid"], view["errors"])
        blocked = {item["slice_id"]: item["reasons"] for item in view["blocked"]}
        self.assertIn("candidate_only_unintegrated", blocked["PR 2"])
        self.assertIn("missing_independent_review", blocked["PR 2"])

    def test_duplicate_owner_cycle_unresolved_and_retired_edges_fail_closed(self) -> None:
        cases = []
        cases.append((load("negative-duplicate-owner.json"), "duplicate owner"))
        cycle = load(); cycle["canonical_dag"]["nodes"][0]["dependencies"] = ["PR 2"]
        reseal(cycle); cases.append((cycle, "dependency cycle"))
        missing = load(); missing["canonical_dag"]["nodes"][1]["dependencies"] = ["PR 99"]
        reseal(missing); cases.append((missing, "unresolved prerequisite"))
        retired = load(); retired["canonical_dag"]["nodes"][1]["dependencies"] = ["FM-168"]
        reseal(retired); cases.append((retired, "retired obligation FM-168"))
        for document, expected in cases:
            view = analyze(document)
            self.assertFalse(view["valid"])
            self.assertEqual(view["next_ready"], [])
            self.assertTrue(any(expected in error for error in view["errors"]), view["errors"])


class LiveEvidenceAndReceiptTests(unittest.TestCase):
    def test_live_source_set_rejects_stale_but_self_consistent_export(self) -> None:
        document = load()
        authoritative = fake_live(document)
        stale = "sha256:" + "0" * 64
        graph = document["canonical_dag"]
        graph["source_set_digest"] = stale
        graph["activation_receipt"]["source_set_digest"] = stale
        ledger = document["completion_ledger"]
        ledger["source_set_digest"] = stale
        for entry in ledger["entries"]:
            entry["source_set_digest"] = stale
            if entry["integration"] is not None:
                entry["integration"]["source_set_digest"] = stale
        reseal(document)
        view = analyze(document, authoritative)
        self.assertFalse(view["valid"])
        self.assertTrue(any("canonical_dag.source_set_digest" in e for e in view["errors"]))

    def test_forged_true_ancestry_is_rejected_against_live_git_observation(self) -> None:
        document = load()
        live = fake_live(document)
        integration = document["completion_ledger"]["entries"][0]["integration"]
        integration["ancestry_observation"]["status"] = "not_ancestor"
        integration["ancestry_observation"]["command_exit_code"] = 1
        integration["receipt_digest"] = es.receipt_digest(integration)
        view = analyze(document, live)
        self.assertFalse(view["valid"])
        self.assertTrue(any("sealed live Git observation" in e for e in view["errors"]))

    def test_tampered_receipt_with_unchanged_digest_is_rejected(self) -> None:
        for key in ["review", "test_receipts", "integration"]:
            document = load()
            entry = document["completion_ledger"]["entries"][0]
            if key == "review":
                entry[key]["verdict"] = "rejected"
            elif key == "test_receipts":
                entry[key][0]["exit_code"] = 9
            else:
                entry[key]["state"] = "pending"
            view = analyze(document)
            self.assertFalse(view["valid"])
            self.assertTrue(any("canonical receipt payload bytes" in e for e in view["errors"]))

    def test_candidate_digest_and_unbound_test_command_are_rejected(self) -> None:
        document = load()
        document["completion_ledger"]["entries"][0]["candidate"]["branch"] = "forged"
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("candidate payload bytes" in e for e in view["errors"]))

        document = load()
        receipt = document["completion_ledger"]["entries"][0]["test_receipts"][0]
        receipt["command"] = "true"
        receipt["receipt_digest"] = es.receipt_digest(receipt)
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("exact declared acceptance command" in e for e in view["errors"]))

        document = load()
        entry = document["completion_ledger"]["entries"][0]
        extra = copy.deepcopy(entry["test_receipts"][0])
        extra["name"] = "undeclared"
        extra["receipt_digest"] = es.receipt_digest(extra)
        entry["test_receipts"].append(extra)
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("not declared in required_tests" in e for e in view["errors"]))

    def test_bare_independence_without_distinct_authority_is_rejected(self) -> None:
        document = load()
        review = document["completion_ledger"]["entries"][0]["review"]
        review["reviewer_authority"] = review["implementation_authority"]
        review["receipt_digest"] = es.receipt_digest(review)
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("distinct principal/authority" in e for e in view["errors"]))

    def test_external_live_failure_is_unknown_and_emits_no_packet(self) -> None:
        document = load()
        live = fake_live(document)
        failed = le.LiveEvidence(**{**live.__dict__, "errors": ("live.git.canonical_ref: failed",)})
        view = analyze(document, failed)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])


class SteeringFenceTests(unittest.TestCase):
    def entry(self, document: dict) -> dict:
        return document["completion_ledger"]["entries"][0]

    def test_unobserved_required_steering_before_terminal_cas_fails_closed(self) -> None:
        document = load(); entry = self.entry(document)
        entry["steering_directives"].append({
            "directive_id": "steer:late-pre-cas", "classification": "required",
            "event_sequence": 5, "delivery_boundary": "event-log:5",
        })
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("late required directive before terminal CAS" in e for e in view["errors"]))

    def test_stale_attempt_acknowledgement_fails_closed(self) -> None:
        document = load(); receipt = self.entry(document)["steering_receipts"][0]
        receipt["attempt_id"] = "attempt:stale"
        receipt["receipt_digest"] = es.receipt_digest(receipt)
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("steering_receipts[0].attempt_id" in e for e in view["errors"]))

    def test_integration_proof_must_pin_attempt_steering_watermark(self) -> None:
        document = load(); integration = self.entry(document)["integration"]
        integration["steering_watermark"] = 3
        integration["receipt_digest"] = es.receipt_digest(integration)
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("integration.steering_watermark" in e for e in view["errors"]))

    def test_duplicate_delivery_fails_closed(self) -> None:
        document = load(); entry = self.entry(document)
        entry["steering_receipts"].append(copy.deepcopy(entry["steering_receipts"][0]))
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("duplicate delivery" in e for e in view["errors"]))

    def test_late_required_steering_after_terminal_cas_opens_remediation(self) -> None:
        document = load(); entry = self.entry(document)
        entry["attempt"]["current_event_sequence"] = 6
        entry["steering_directives"].append({
            "directive_id": "steer:post-cas", "classification": "required",
            "event_sequence": 6, "delivery_boundary": "event-log:6",
        })
        view = analyze(document)
        self.assertTrue(view["valid"], view["errors"])
        reasons = next(item["reasons"] for item in view["blocked"] if item["slice_id"] == "PR 1")
        self.assertIn("late_required_steering_remediation:steer:post-cas", reasons)

    def test_advisory_only_steering_never_fences_completion(self) -> None:
        document = load(); entry = self.entry(document)
        entry["attempt"]["current_event_sequence"] = 9
        entry["steering_directives"].append({
            "directive_id": "steer:advisory-late", "classification": "advisory",
            "event_sequence": 9, "delivery_boundary": "event-log:9",
        })
        view = analyze(document)
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual([item["slice_id"] for item in view["next_ready"]], ["PR 2"])


class SurfaceTests(unittest.TestCase):
    def test_markdown_and_json_are_views_of_same_live_result(self) -> None:
        document = load()
        view = analyze(document)
        markdown = es.markdown(view)
        self.assertIn("# TraceDecay V2 next-ready", markdown)
        self.assertIn("### PR 2", markdown)
        self.assertEqual(json.loads(json.dumps(view, sort_keys=True)), view)

    def test_cli_requires_live_root(self) -> None:
        script = Path(__file__).with_name("plan_execution.py")
        result = subprocess.run(
            ["python3", str(script), "--graph", str(FIXTURES / "positive-ready.json")],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("--root", result.stderr)

    def test_cli_markdown_and_json_share_one_actual_checkout_boundary(self) -> None:
        document = load()
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, check=True, text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()
        observed = le.inspect(ROOT, "HEAD", [head])
        self.assertEqual(observed.errors, ())
        graph = document["canonical_dag"]
        graph["repository"] = observed.repository
        graph["source_commit"] = observed.canonical_commit
        graph["source_set_digest"] = observed.source_set_digest
        activation = graph["activation_receipt"]
        activation["repository"] = observed.repository
        activation["source_commit"] = observed.canonical_commit
        activation["source_set_digest"] = observed.source_set_digest
        ledger = document["completion_ledger"]
        ledger["repository"] = observed.repository
        ledger["source_commit"] = observed.canonical_commit
        ledger["source_set_digest"] = observed.source_set_digest
        entry = ledger["entries"][0]
        entry["source_commit"] = observed.canonical_commit
        entry["source_set_digest"] = observed.source_set_digest
        entry["candidate"]["commit"] = head
        integration = entry["integration"]
        integration["candidate_commit"] = head
        integration["canonical_commit"] = observed.canonical_commit
        integration["source_set_digest"] = observed.source_set_digest
        integration["ancestry_observation"] = observed.ancestry[head]
        reseal(document)
        script = Path(__file__).with_name("plan_execution.py")
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / "state.json"
            state.write_text(json.dumps(document, sort_keys=True), encoding="utf-8")
            markdown = subprocess.run(
                ["python3", str(script), "--graph", str(state), "--root", str(ROOT),
                 "--canonical-ref", "HEAD"],
                check=True, text=True, stdout=subprocess.PIPE,
            ).stdout
            raw_json = subprocess.run(
                ["python3", str(script), "--graph", str(state), "--root", str(ROOT),
                 "--canonical-ref", "HEAD", "--format", "json"],
                check=True, text=True, stdout=subprocess.PIPE,
            ).stdout
        view = json.loads(raw_json)
        self.assertTrue(view["valid"], view["errors"])
        self.assertIn("### PR 2", markdown)
        self.assertEqual(view["source_commit"], head)

    def test_duplicate_json_key_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema":"one","schema":"two"}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
                plan_execution.strict_json(path)


if __name__ == "__main__":
    unittest.main()