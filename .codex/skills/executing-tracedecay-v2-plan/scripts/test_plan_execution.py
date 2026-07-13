#!/usr/bin/env python3
"""Deterministic completion-ledger, live-evidence, and steering-fence contracts."""

from __future__ import annotations

import copy
import dataclasses
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

import execution_state as es
import bootstrap_execution
import git_observation as go
import live_evidence as le
import plan_execution
import slice_authority as sa


FIXTURES = Path(__file__).with_name("fixtures")
ROOT = Path(__file__).resolve().parents[4]


def load(name: str = "positive-ready.json") -> dict:
    document = json.loads((FIXTURES / name).read_text(encoding="utf-8"))
    if "HARNESS" in globals():
        HARNESS.seal(document)
    return document


def reseal(document: dict, *, replace_dispatch_authority: bool = False) -> None:
    for directive in (
        directive
        for entry in document["completion_ledger"]["entries"]
        for directive in entry["steering_directives"]
    ):
        directive.setdefault("remediation_task", None)
        directive.setdefault("successor_review_task", None)
    dispatch = {spec["slice_id"]: spec for spec in document["dispatch_specs"]}
    for spec in dispatch.values():
        spec.setdefault("required_tests", ["unit"])
    graph = document["canonical_dag"]
    if replace_dispatch_authority:
        for node in graph["nodes"]:
            node["dispatch_digest"] = es.dispatch_digest(dispatch[node["id"]])
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


class GitHarness:
    """Independent live-Git authority for every positive completion assertion."""

    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        base = Path(self.temporary.name)
        self.root = base / "canonical"
        self.root.mkdir()
        self._git("init", "-b", "main")
        self._git("config", "user.email", "test@example.invalid")
        self._git("config", "user.name", "TraceDecay Test")
        self._git("remote", "add", "origin", "https://example.invalid/tracedecay.git")
        plans = self.root / "docs/plans/tracedecay-v2"
        plans.mkdir(parents=True)
        (self.root / "docs/plans/2026-07-09-tracedecay-brain-rewrite.md").write_text(
            "# Synthetic V2 plan\n", encoding="utf-8"
        )
        (plans / "00-plan-set-index.md").write_text("# Synthetic index\n", encoding="utf-8")
        (self.root / "evidence.txt").write_text("candidate\n", encoding="utf-8")
        self._git("add", ".")
        self._git("commit", "-m", "candidate")
        self.candidate_commit = self._git("rev-parse", "HEAD").stdout.strip()
        self.worktrees: dict[str, Path] = {}
        for slice_id, branch in [("PR 1", "v2/pr-1"), ("PR 2", "v2/pr-2")]:
            path = base / branch.replace("/", "-")
            self._git("worktree", "add", "-b", branch, str(path), self.candidate_commit)
            self.worktrees[slice_id] = path.resolve()
        (self.root / "evidence.txt").write_text("canonical\n", encoding="utf-8")
        self._git("commit", "-am", "canonical")
        self.live = le.inspect(self.root, "refs/heads/main", [self.candidate_commit])
        if self.live.errors:
            raise AssertionError(self.live.errors)

    def _git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args], cwd=self.root, check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
        )

    def seal(self, document: dict) -> le.LiveEvidence:
        graph = document["canonical_dag"]
        graph["repository"] = self.live.repository
        graph["source_commit"] = self.live.canonical_commit
        graph["source_set_digest"] = self.live.source_set_digest
        activation = graph["activation_receipt"]
        activation["repository"] = self.live.repository
        activation["source_commit"] = self.live.canonical_commit
        activation["source_set_digest"] = self.live.source_set_digest
        ledger = document["completion_ledger"]
        ledger["repository"] = self.live.repository
        ledger["source_commit"] = self.live.canonical_commit
        ledger["source_set_digest"] = self.live.source_set_digest
        specs = {spec["slice_id"]: spec for spec in document["dispatch_specs"]}
        for slice_id, spec in specs.items():
            spec["required_tests"] = ["unit"]
            spec["workspace"] = {
                "branch": f"v2/pr-{slice_id.split()[-1]}",
                "worktree": str(self.worktrees[slice_id]),
            }
        for entry in ledger["entries"]:
            slice_id = entry["slice_id"]
            entry["source_commit"] = self.live.canonical_commit
            entry["source_set_digest"] = self.live.source_set_digest
            entry["required_tests"] = specs[slice_id]["required_tests"]
            candidate = entry["candidate"]
            candidate["commit"] = self.candidate_commit
            candidate["branch"] = specs[slice_id]["workspace"]["branch"]
            candidate["worktree"] = specs[slice_id]["workspace"]["worktree"]
            branch_ref = f"refs/heads/{candidate['branch']}"
            candidate["workspace_observation"] = copy.deepcopy(
                self.live.workspaces[
                    le.workspace_key(candidate["commit"], branch_ref, candidate["worktree"])
                ]
            )
            integration = entry.get("integration")
            if integration is not None:
                integration["candidate_commit"] = self.candidate_commit
                integration["canonical_commit"] = self.live.canonical_commit
                integration["canonical_branch"] = self.live.canonical_ref
                integration["source_set_digest"] = self.live.source_set_digest
                integration["ancestry_observation"] = copy.deepcopy(
                    self.live.ancestry[self.candidate_commit]
                )
        reseal(document, replace_dispatch_authority=True)
        return self.live

    def close(self) -> None:
        self.temporary.cleanup()


HARNESS: GitHarness


def setUpModule() -> None:
    global HARNESS
    HARNESS = GitHarness()


def tearDownModule() -> None:
    HARNESS.close()


def analyze(document: dict, live: le.LiveEvidence | None = None, *, trust_receipts: bool = False) -> dict:
    authority = live or HARNESS.live
    if trust_receipts:
        authority = dataclasses.replace(
            authority,
            review_receipts=frozenset(
                entry["review"]["receipt_digest"]
                for entry in document["completion_ledger"]["entries"]
                if isinstance(entry.get("review"), dict)
            ),
            test_receipts=frozenset(
                receipt["receipt_digest"]
                for entry in document["completion_ledger"]["entries"]
                for receipt in entry["test_receipts"]
            ),
        )
    return plan_execution.analyze(document, authority)


def analyze_trusted(document: dict, live: le.LiveEvidence | None = None) -> dict:
    """Simulate receipts independently observed by the test harness event recorder."""
    return analyze(document, live, trust_receipts=True)


class StateResolutionTests(unittest.TestCase):
    def test_legacy_state_and_active_pointer_are_ambiguous(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".tracedecay").mkdir()
            (root / plan_execution.DEFAULT_STATE).write_text("{}", encoding="utf-8")
            (root / ".tracedecay/v2-execution-active.json").write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "ambiguous execution state"):
                plan_execution.resolve_state(root, None)

    def test_explicit_and_env_state_win_over_ambiguous_repo_local_sources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".tracedecay").mkdir()
            (root / plan_execution.DEFAULT_STATE).write_text("{}", encoding="utf-8")
            (root / ".tracedecay/v2-execution-active.json").write_text("{}", encoding="utf-8")
            explicit = root / "explicit.json"
            configured = root / "configured.json"
            self.assertEqual(plan_execution.resolve_state(root, explicit), explicit)
            with mock.patch.dict(os.environ, {plan_execution.STATE_ENV: str(configured)}):
                self.assertEqual(plan_execution.resolve_state(root, None), configured)


class NextReadyTests(unittest.TestCase):
    def test_positive_fixture_selects_exact_bounded_packet(self) -> None:
        view = analyze_trusted(load())
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual([item["slice_id"] for item in view["next_ready"]], ["PR 2"])
        packet = view["next_ready"][0]
        self.assertEqual(packet["prerequisites"], ["PR 1"])
        self.assertEqual(packet["lane"]["reasoning_owner"], "gpt-5.6-sol")
        self.assertEqual(packet["optional_claude_review"]["max_steps"], 1)

    def test_candidate_only_is_valid_but_blocks_itself(self) -> None:
        document = load("negative-candidate-only.json")
        view = analyze_trusted(document)
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
            view = analyze_trusted(document)
            self.assertFalse(view["valid"])
            self.assertEqual(view["next_ready"], [])
            self.assertTrue(any(expected in error for error in view["errors"]), view["errors"])

    def test_one_owner_reused_by_distinct_nodes_fails_closed_after_reseal(self) -> None:
        document = load()
        document["canonical_dag"]["nodes"][1]["owner"] = "owner:plan-01"
        document["dispatch_specs"][1]["owner"] = "owner:plan-01"
        reseal(document)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])
        self.assertTrue(any("duplicate owner" in error for error in view["errors"]))

    def test_every_worker_packet_scalar_is_bounded(self) -> None:
        document = load()
        document["dispatch_specs"][1]["workspace"]["worktree"] = "x" * (es.MAX_TEXT + 1)
        reseal(document, replace_dispatch_authority=True)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("scalar exceeds 2048" in error for error in view["errors"]))

    def test_bootstrap_rejects_hand_authored_self_consistent_state(self) -> None:
        document = load()
        document["activation_mode"] = "verify_only"
        document["completion_ledger"]["entries"] = []
        document["dispatch_specs"] = []
        graph = document["canonical_dag"]
        manifest = {
            "schema": "tracedecay.v2.slice-dag/v1",
            "graph_revision": graph["graph_revision"],
            "source_set_digest": graph["source_set_digest"],
            "slices": {
                node["id"]: {
                    "content_digest": node["content_digest"],
                    "dependencies": [
                        {"parent": parent} for parent in node["dependencies"]
                    ],
                }
                for node in graph["nodes"]
            },
            "series": {},
        }
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            manifest_path = temp / "manifest.json"
            state_path = temp / "state.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            state_path.write_text(json.dumps(document), encoding="utf-8")
            process = subprocess.run(
                [
                    "python3", str(Path(bootstrap_execution.__file__)),
                    "--manifest", str(manifest_path),
                    "--state-export", str(state_path),
                    "--root", str(HARNESS.root),
                    "--canonical-ref", HARNESS.live.canonical_ref,
                ],
                check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            self.assertEqual(process.returncode, 2, process.stdout + process.stderr)
            self.assertIn("canonical commit lacks", process.stdout)
            self.assertFalse((HARNESS.root / bootstrap_execution.ACTIVE_POINTER).exists())


class LiveEvidenceAndReceiptTests(unittest.TestCase):
    def test_live_source_set_rejects_stale_but_self_consistent_export(self) -> None:
        document = load()
        authoritative = HARNESS.live
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
        view = analyze_trusted(document, authoritative)
        self.assertFalse(view["valid"])
        self.assertTrue(any("canonical_dag.source_set_digest" in e for e in view["errors"]))

    def test_forged_true_ancestry_is_rejected_against_live_git_observation(self) -> None:
        document = load()
        live = HARNESS.live
        integration = document["completion_ledger"]["entries"][0]["integration"]
        integration["ancestry_observation"]["status"] = "not_ancestor"
        integration["ancestry_observation"]["command_exit_code"] = 1
        integration["receipt_digest"] = es.receipt_digest(integration)
        view = analyze_trusted(document, live)
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
            view = analyze_trusted(document)
            self.assertFalse(view["valid"])
            self.assertTrue(any("canonical receipt payload bytes" in e for e in view["errors"]))

    def test_candidate_digest_and_unbound_test_command_are_rejected(self) -> None:
        document = load()
        document["completion_ledger"]["entries"][0]["candidate"]["branch"] = "forged"
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("candidate payload bytes" in e for e in view["errors"]))

        document = load()
        receipt = document["completion_ledger"]["entries"][0]["test_receipts"][0]
        receipt["command"] = "true"
        receipt["receipt_digest"] = es.receipt_digest(receipt)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("exact declared acceptance command" in e for e in view["errors"]))

        document = load()
        entry = document["completion_ledger"]["entries"][0]
        extra = copy.deepcopy(entry["test_receipts"][0])
        extra["name"] = "undeclared"
        extra["receipt_digest"] = es.receipt_digest(extra)
        entry["test_receipts"].append(extra)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("not declared in required_tests" in e for e in view["errors"]))

    def test_bare_independence_without_distinct_authority_is_rejected(self) -> None:
        document = load()
        review = document["completion_ledger"]["entries"][0]["review"]
        review["reviewer_authority"] = review["implementation_authority"]
        review["receipt_digest"] = es.receipt_digest(review)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("distinct principal/authority" in e for e in view["errors"]))

    def test_self_authored_review_and_test_receipts_do_not_unlock_work(self) -> None:
        document = load()
        view = analyze(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("trusted review observations" in e for e in view["errors"]))
        self.assertTrue(any("trusted test observations" in e for e in view["errors"]))

    def test_integrated_candidate_does_not_require_retained_worktree(self) -> None:
        document = load()
        live = dataclasses.replace(HARNESS.live, workspaces={})
        view = analyze_trusted(document, live)
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual([item["slice_id"] for item in view["next_ready"]], ["PR 2"])

    def test_external_live_failure_is_unknown_and_emits_no_packet(self) -> None:
        document = load()
        live = HARNESS.live
        failed = le.LiveEvidence(**{**live.__dict__, "errors": ("live.git.canonical_ref: failed",)})
        view = analyze_trusted(document, failed)
        self.assertFalse(view["valid"])
        self.assertEqual(view["next_ready"], [])

    def test_coordinated_test_contract_rewrite_cannot_replace_dag_authority(self) -> None:
        document = load()
        spec = document["dispatch_specs"][0]
        spec["acceptance_commands"] = ["true"]
        spec["required_tests"] = ["fabricated"]
        entry = document["completion_ledger"]["entries"][0]
        entry["required_tests"] = ["fabricated"]
        entry["test_receipts"][0]["name"] = "fabricated"
        entry["test_receipts"][0]["command"] = "true"
        reseal(document)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("canonical DAG dispatch_digest" in error for error in view["errors"]))

    def test_coordinated_workspace_rewrite_lacks_live_and_dag_authority(self) -> None:
        document = load()
        spec = document["dispatch_specs"][0]
        spec["workspace"] = {"branch": "forged", "worktree": "/forged"}
        candidate = document["completion_ledger"]["entries"][0]["candidate"]
        candidate["branch"] = "forged"
        candidate["worktree"] = "/forged"
        document["completion_ledger"]["entries"][0]["integration"] = None
        reseal(document)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("canonical DAG dispatch_digest" in error for error in view["errors"]))
        self.assertTrue(any("no fresh live Git association" in error for error in view["errors"]))

    def test_integration_branch_must_equal_resolved_live_ref(self) -> None:
        document = load()
        integration = document["completion_ledger"]["entries"][0]["integration"]
        integration["canonical_branch"] = "refs/heads/attacker"
        integration["receipt_digest"] = es.receipt_digest(integration)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("integration.canonical_branch" in error for error in view["errors"]))

    def test_dirty_worktree_observation_fails_closed(self) -> None:
        document = load()
        document["completion_ledger"]["entries"][0]["integration"] = None
        live = copy.deepcopy(HARNESS.live)
        candidate = document["completion_ledger"]["entries"][0]["candidate"]
        key = le.workspace_key(candidate["commit"], f"refs/heads/{candidate['branch']}",
                               candidate["worktree"])
        dirty = dict(live.workspaces[key])
        dirty["clean"] = False
        dirty["observation_digest"] = le.digest({
            name: value for name, value in dirty.items() if name != "observation_digest"
        })
        live.workspaces[key] = dirty
        candidate["workspace_observation"] = copy.deepcopy(dirty)
        reseal(document)
        view = analyze_trusted(document, live)
        self.assertFalse(view["valid"])
        self.assertTrue(any("worktree is not clean" in error for error in view["errors"]))

    def test_git_timeout_and_output_overflow_are_explicit_errors(self) -> None:
        timeout = subprocess.TimeoutExpired(["git"], go.GIT_TIMEOUT_SECONDS)
        with mock.patch.object(go.subprocess, "run", side_effect=timeout):
            result = le._git(HARNESS.root, "status")
        self.assertIn("timed out", result.error or "")

        def overflow(*args: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
            kwargs["stdout"].write(b"x" * (go.MAX_GIT_OUTPUT_BYTES + 1))
            return subprocess.CompletedProcess(["git", "status"], 0)

        with mock.patch.object(go.subprocess, "run", side_effect=overflow):
            result = le._git(HARNESS.root, "status")
        self.assertIn("exceeded", result.error or "")

    def test_non_ref_canonical_input_fails_closed(self) -> None:
        observed = le.inspect(HARNESS.root, HARNESS.candidate_commit, [])
        self.assertTrue(any("canonical_ref" in error for error in observed.errors))


class SteeringFenceTests(unittest.TestCase):
    def entry(self, document: dict) -> dict:
        return document["completion_ledger"]["entries"][0]

    def test_unobserved_required_steering_before_terminal_cas_fails_closed(self) -> None:
        document = load(); entry = self.entry(document)
        entry["steering_directives"].append({
            "directive_id": "steer:late-pre-cas", "classification": "required",
            "event_sequence": 5, "delivery_boundary": "event-log:5",
            "remediation_task": None, "successor_review_task": None,
        })
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("late required directive before terminal CAS" in e for e in view["errors"]))

    def test_stale_attempt_acknowledgement_fails_closed(self) -> None:
        document = load(); receipt = self.entry(document)["steering_receipts"][0]
        receipt["attempt_id"] = "attempt:stale"
        receipt["receipt_digest"] = es.receipt_digest(receipt)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("steering_receipts[0].attempt_id" in e for e in view["errors"]))

    def test_integration_proof_must_pin_attempt_steering_watermark(self) -> None:
        document = load(); integration = self.entry(document)["integration"]
        integration["steering_watermark"] = 3
        integration["receipt_digest"] = es.receipt_digest(integration)
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("integration.steering_watermark" in e for e in view["errors"]))

    def test_duplicate_delivery_fails_closed(self) -> None:
        document = load(); entry = self.entry(document)
        entry["steering_receipts"].append(copy.deepcopy(entry["steering_receipts"][0]))
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("duplicate delivery" in e for e in view["errors"]))

    def test_late_required_steering_after_terminal_cas_opens_remediation(self) -> None:
        document = load(); entry = self.entry(document)
        entry["attempt"]["current_event_sequence"] = 6
        entry["steering_directives"].append({
            "directive_id": "steer:post-cas", "classification": "required",
            "event_sequence": 6, "delivery_boundary": "event-log:6",
            "remediation_task": "task:pr-1:remediation",
            "successor_review_task": "task:pr-1:successor-review",
        })
        entry["task_lineage"]["remediation_tasks"] = ["task:pr-1:remediation"]
        entry["task_lineage"]["successor_review_tasks"] = ["task:pr-1:successor-review"]
        view = analyze_trusted(document)
        self.assertTrue(view["valid"], view["errors"])
        reasons = next(item["reasons"] for item in view["blocked"] if item["slice_id"] == "PR 1")
        self.assertIn("late_required_steering_remediation:steer:post-cas", reasons)

    def test_post_cas_required_steering_requires_bound_recovery_lineage(self) -> None:
        document = load(); entry = self.entry(document)
        entry["attempt"]["current_event_sequence"] = 6
        entry["steering_directives"].append({
            "directive_id": "steer:post-cas-unbound", "classification": "required",
            "event_sequence": 6, "delivery_boundary": "event-log:6",
            "remediation_task": None, "successor_review_task": None,
        })
        view = analyze_trusted(document)
        self.assertFalse(view["valid"])
        self.assertTrue(any("explicitly bound remediation" in error for error in view["errors"]))
        self.assertTrue(any("explicitly bound successor-review" in error for error in view["errors"]))

    def test_advisory_only_steering_never_fences_completion(self) -> None:
        document = load(); entry = self.entry(document)
        entry["attempt"]["current_event_sequence"] = 9
        entry["steering_directives"].append({
            "directive_id": "steer:advisory-late", "classification": "advisory",
            "event_sequence": 9, "delivery_boundary": "event-log:9",
            "remediation_task": None, "successor_review_task": None,
        })
        view = analyze_trusted(document)
        self.assertTrue(view["valid"], view["errors"])
        self.assertEqual([item["slice_id"] for item in view["next_ready"]], ["PR 2"])


class SurfaceTests(unittest.TestCase):
    def test_markdown_and_json_are_views_of_same_live_result(self) -> None:
        document = load()
        view = analyze_trusted(document)
        markdown = es.markdown(view)
        self.assertIn("# TraceDecay V2 next-ready", markdown)
        self.assertIn("### PR 2", markdown)
        self.assertNotIn("```json", markdown)
        for label in [
            "Schema", "Repository", "Source commit", "Source-set digest",
            "Graph revision", "Graph digest", "Owner", "Prerequisites",
            "Workspace branch", "Workspace worktree", "Exact files",
            "Acceptance commands", "Required tests", "Lane", "Reasoning owner",
            "Lifecycle owner", "Acting runtime", "Optional Claude review",
            "Acceptance criteria", "Untrusted until GPT verified", "Gates",
            "independent_review", "remediation", "successor_review", "integration",
            "Retrieval anchors", "Blocked",
        ]:
            self.assertIn(label, markdown)
        packet = view["next_ready"][0]
        for value in [
            packet["source_commit"], packet["source_set_digest"], packet["graph_digest"],
            packet["workspace"]["branch"], packet["workspace"]["worktree"],
            packet["lane"]["acting_runtime"], *packet["required_tests"],
            *packet["retrieval_anchors"], *packet["gates"].values(),
        ]:
            self.assertIn(json.dumps(value), markdown)
        self.assertEqual(json.loads(json.dumps(view, sort_keys=True)), view)

    def test_shared_view_diagnostics_are_bounded_with_digest_handles(self) -> None:
        diagnostics = [f"error-{index}:" + "x" * 3000 for index in range(300)]
        bounded = es._finalize_errors(diagnostics)
        self.assertEqual(len(bounded), es.MAX_DIAGNOSTICS)
        self.assertTrue(all(len(error) <= es.MAX_TEXT for error in bounded))
        self.assertIn("diagnostics: omitted", bounded[-1])
        self.assertTrue(any("truncated sha256:" in error for error in bounded[:-1]))

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
        document["completion_ledger"]["entries"] = []
        script = Path(__file__).with_name("plan_execution.py")
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / "state.json"
            state.write_text(json.dumps(document, sort_keys=True), encoding="utf-8")
            markdown = subprocess.run(
                ["python3", str(script), "--graph", str(state),
                 "--root", str(HARNESS.root), "--canonical-ref", "refs/heads/main"],
                check=True, text=True, stdout=subprocess.PIPE,
            ).stdout
            raw_json = subprocess.run(
                ["python3", str(script), "--graph", str(state),
                 "--root", str(HARNESS.root), "--canonical-ref", "refs/heads/main",
                 "--format", "json"],
                check=True, text=True, stdout=subprocess.PIPE,
            ).stdout
        view = json.loads(raw_json)
        self.assertTrue(view["valid"], view["errors"])
        self.assertIn("### PR 1", markdown)
        self.assertEqual(view["source_commit"], HARNESS.live.canonical_commit)

    def test_duplicate_json_key_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema":"one","schema":"two"}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
                plan_execution.strict_json(path)


if __name__ == "__main__":
    unittest.main()
