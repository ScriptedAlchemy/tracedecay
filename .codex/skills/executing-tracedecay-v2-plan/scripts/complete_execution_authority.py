#!/usr/bin/env python3
"""Append one event-observed PR 1 completion through a fenced V2-to-V2 CAS."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import resource
import shutil
import subprocess
import tempfile
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

import bootstrap_execution
import compile_plan_authority
import execution_state
import execution_state_v2 as v2
import live_evidence
import plan_execution
import strict_json
from git_observation import run_git


GENERATION = re.compile(r"^r[1-9][0-9]*-[0-9a-f]{16}-[0-9a-f]{16}$")
REVIEW_RECEIPT = Path(".tracedecay/pr1-independent-review.json")
REVIEW_EVENT = Path(".tracedecay/pr1-independent-review.event.json")
REVIEW_EVENT_SCHEMA = "tracedecay.v2.completion-review-event/v1"
REVIEWER_TASK = "/root/completion_receipt_schema"
PR1_TEST_COMMAND = "cargo test --test architecture_boundaries"
COMMAND_TIMEOUT_SECONDS = 3600
MAX_COMMAND_OUTPUT_BYTES = 8 * 1024 * 1024


def _limit_command_output() -> None:
    resource.setrlimit(
        resource.RLIMIT_FSIZE,
        (MAX_COMMAND_OUTPUT_BYTES, MAX_COMMAND_OUTPUT_BYTES),
    )


@dataclass(frozen=True)
class Predecessor:
    generation: str
    pointer_bytes: bytes
    manifest_bytes: bytes
    state: dict[str, Any]


def _bytes(document: dict[str, Any]) -> bytes:
    return compile_plan_authority._canonical_json_bytes(document)


def _generation_pointer(generation: str, manifest_bytes: bytes,
                        state_bytes: bytes) -> dict[str, Any]:
    return {
        "schema": bootstrap_execution.POINTER_SCHEMA,
        "generation": generation,
        "manifest": f"v2-execution-generations/{generation}/manifest.json",
        "state": f"v2-execution-generations/{generation}/state.json",
        "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
        "state_sha256": hashlib.sha256(state_bytes).hexdigest(),
    }


def _observation_sets(root: Path, *, required: bool = False) -> tuple[frozenset[str], frozenset[str], frozenset[str]]:
    authority = live_evidence.load_authority_review_observations(root, required=True)
    review, tests = live_evidence.load_completion_observations(root, required=required)
    return authority, review, tests


def _record_observations(root: Path, review_digest: str,
                         test_digests: list[str]) -> None:
    """Sole bootstrap writer for event-observed completion receipt digests."""
    path = root / live_evidence.COMPLETION_OBSERVATIONS
    existing_reviews: frozenset[str] = frozenset()
    existing_tests: frozenset[str] = frozenset()
    if path.exists():
        existing_reviews, existing_tests = live_evidence.load_completion_observations(
            root, required=True
        )
    document = {
        "schema": live_evidence.COMPLETION_OBSERVATIONS_SCHEMA,
        "review_receipt_digests": sorted(existing_reviews | {review_digest}),
        "test_receipt_digests": sorted(existing_tests | set(test_digests)),
    }
    bootstrap_execution._atomic_install(path, document)


def _candidate_worktree(packet: dict[str, Any], entry: dict[str, Any]) -> Path:
    candidate = entry.get("candidate")
    workspace = packet.get("workspace")
    if not isinstance(candidate, dict) or not isinstance(workspace, dict):
        raise ValueError("candidate or packet workspace is malformed")
    for field in ("branch", "worktree"):
        if candidate.get(field) != workspace.get(field):
            raise ValueError(f"candidate {field} differs from reviewed packet workspace")
    worktree = Path(str(candidate["worktree"]))
    if worktree.is_symlink() or not worktree.is_dir():
        raise ValueError("candidate worktree must be a real non-symlink directory")
    checks = [
        (("rev-parse", "HEAD"), str(candidate.get("commit"))),
        (("symbolic-ref", "-q", "HEAD"), f"refs/heads/{candidate.get('branch')}"),
        (("status", "--porcelain"), ""),
    ]
    for arguments, expected in checks:
        result = run_git(worktree, *arguments, max_output_bytes=64 * 1024)
        if result.error is not None or result.returncode != 0:
            detail = result.error or result.stderr.decode("utf-8", "replace")
            raise ValueError(f"candidate worktree Git observation failed: {detail}")
        try:
            actual = result.stdout.decode("utf-8").strip()
        except UnicodeDecodeError as error:
            raise ValueError("candidate worktree Git output is not UTF-8") from error
        if actual != expected:
            raise ValueError("candidate worktree HEAD, branch, or cleanliness mismatch")
    return worktree


def _run_acceptance(packet: dict[str, Any],
                    entry: dict[str, Any]) -> list[dict[str, Any]]:
    commands = packet.get("acceptance_commands")
    required_tests = packet.get("required_tests")
    if not isinstance(commands, list) or not all(isinstance(item, str) for item in commands):
        raise ValueError("PR 1 packet acceptance commands are malformed")
    if not isinstance(required_tests, list) or not all(
        isinstance(item, str) for item in required_tests
    ):
        raise ValueError("PR 1 packet required tests are malformed")
    if len(required_tests) != len(set(required_tests)):
        raise ValueError("PR 1 packet required tests must be unique")
    if commands.count(PR1_TEST_COMMAND) != 1:
        raise ValueError("PR 1 packet must contain the exact architecture test command once")
    candidate = entry.get("candidate")
    if not isinstance(candidate, dict):
        raise ValueError("completion entry candidate is malformed")
    worktree = _candidate_worktree(packet, entry)
    with tempfile.TemporaryFile() as list_output:
        listed = subprocess.run(
            f"{PR1_TEST_COMMAND} -- --list",
            cwd=worktree,
            shell=True,
            executable="/bin/bash",
            stdin=subprocess.DEVNULL,
            stdout=list_output,
            stderr=subprocess.STDOUT,
            timeout=COMMAND_TIMEOUT_SECONDS,
            check=False,
            preexec_fn=_limit_command_output,
        )
        if listed.returncode != 0 or list_output.tell() > MAX_COMMAND_OUTPUT_BYTES:
            raise ValueError("cannot enumerate the bounded PR 1 architecture test set")
        list_output.seek(0)
        listed_bytes = list_output.read()
    listed_names = sorted(
        line.removesuffix(": test")
        for line in listed_bytes.decode("utf-8", "replace").splitlines()
        if line.endswith(": test")
    )
    if len(listed_names) != len(set(listed_names)) or not set(required_tests).issubset(listed_names):
        raise ValueError("enumerated architecture tests do not contain unique reviewed required_tests")
    receipts: list[dict[str, Any]] = []
    for command in commands:
        with tempfile.TemporaryFile() as output:
            try:
                completed = subprocess.run(
                    command,
                    cwd=worktree,
                    shell=True,
                    executable="/bin/bash",
                    stdin=subprocess.DEVNULL,
                    stdout=output,
                    stderr=subprocess.STDOUT,
                    timeout=COMMAND_TIMEOUT_SECONDS,
                    check=False,
                    preexec_fn=_limit_command_output,
                )
            except subprocess.TimeoutExpired as error:
                raise ValueError(f"acceptance command timed out: {command}") from error
            if completed.returncode != 0:
                output.seek(0)
                tail = output.read()[-4096:].decode("utf-8", "replace")
                raise ValueError(
                    f"acceptance command failed with exit {completed.returncode}: {command}: {tail}"
                )
            if output.tell() > MAX_COMMAND_OUTPUT_BYTES:
                raise ValueError(f"acceptance command output exceeded bound: {command}")
            if command == PR1_TEST_COMMAND:
                output.seek(0)
                executed = output.read().decode("utf-8", "replace")
                executed_names = sorted(
                    match.group(1)
                    for match in re.finditer(r"^test ([^ ]+) \.\.\. ok$", executed, re.MULTILINE)
                )
                if (
                    len(executed_names) != len(set(executed_names))
                    or not set(required_tests).issubset(executed_names)
                ):
                    raise ValueError(
                        "executed architecture tests do not contain unique reviewed required_tests"
                    )
        receipt: dict[str, Any] = {
            "tests": list(required_tests) if command == PR1_TEST_COMMAND else [],
            "command": command,
            "exit_code": completed.returncode,
            "candidate_commit": candidate.get("commit"),
            "candidate_digest": candidate.get("digest"),
            "receipt_digest": "",
        }
        receipt["receipt_digest"] = execution_state.receipt_digest(receipt)
        receipts.append(receipt)
    return receipts


def _load_review_event(root: Path, entry: dict[str, Any]) -> dict[str, Any]:
    event_path = root / REVIEW_EVENT
    receipt_path = root / REVIEW_RECEIPT
    for path in (event_path, receipt_path):
        if path.is_symlink() or not path.is_file() or (path.stat().st_mode & 0o077):
            raise ValueError("fixed first-party review artifacts must be regular owner-only files")
    event = plan_execution.strict_json(event_path)
    fields = {
        "schema", "reviewer_task", "receipt_path", "receipt_digest",
        "candidate_commit", "candidate_digest", "verdict", "observed_at", "event_digest",
    }
    if set(event) != fields or event.get("schema") != REVIEW_EVENT_SCHEMA:
        raise ValueError("fixed first-party review event schema or fields differ")
    if event.get("event_digest") != execution_state.receipt_digest(event, "event_digest"):
        raise ValueError("fixed first-party review event digest mismatch")
    if not isinstance(event.get("observed_at"), str) or not re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
        event["observed_at"],
    ):
        raise ValueError("fixed first-party review event observed_at must be RFC3339 UTC")
    review = plan_execution.strict_json(receipt_path)
    candidate = entry.get("candidate")
    if not isinstance(candidate, dict):
        raise ValueError("review event candidate is malformed")
    pins = {
        "reviewer_task": REVIEWER_TASK,
        "receipt_path": REVIEW_RECEIPT.as_posix(),
        "receipt_digest": review.get("receipt_digest"),
        "candidate_commit": candidate.get("commit"),
        "candidate_digest": candidate.get("digest"),
        "verdict": review.get("verdict"),
    }
    for field, expected in pins.items():
        if event.get(field) != expected:
            raise ValueError(f"fixed first-party review event {field} mismatch")
    if review.get("review_task") != REVIEWER_TASK:
        raise ValueError("fixed independent review receipt task mismatch")
    return review


def _observe_completion(root: Path, packet: dict[str, Any], entry: dict[str, Any]) -> dict[str, Any]:
    if entry.get("test_receipts") not in ([], None):
        raise ValueError("completion entry template must not contain caller-authored test receipts")
    review = _load_review_event(root, entry)
    if entry.get("review") != review:
        raise ValueError("completion entry review differs from independent review receipt bytes")
    if review.get("verdict") != "approved" or review.get("independent") is not True:
        raise ValueError("completion review must be independently approved")
    if review.get("receipt_digest") != execution_state.receipt_digest(review):
        raise ValueError("completion review receipt digest mismatch")
    lineage = entry.get("task_lineage")
    if not isinstance(lineage, dict):
        raise ValueError("completion task lineage is malformed")
    if review.get("reviewer_principal") == lineage.get("implementation_actor"):
        raise ValueError("completion reviewer principal is the implementation actor")
    if review.get("reviewer_authority") == review.get("implementation_authority"):
        raise ValueError("completion reviewer authority is not independent")
    observed = copy.deepcopy(entry)
    observed["test_receipts"] = _run_acceptance(packet, observed)
    _candidate_worktree(packet, observed)
    _record_observations(
        root,
        review["receipt_digest"],
        [receipt["receipt_digest"] for receipt in observed["test_receipts"]],
    )
    return observed


def _bootstrap_reconciliation(predecessor: Predecessor, entry: dict[str, Any],
                              live: live_evidence.LiveEvidence) -> dict[str, Any]:
    """Create new bootstrap evidence; never invent a historical worker attempt."""
    if entry.get("attempt") not in (None, {}):
        raise ValueError("completion template must not claim a historical attempt")
    if entry.get("steering_directives") != [] or entry.get("steering_receipts") != []:
        raise ValueError("completion template must not claim historical steering evidence")
    if entry.get("integration") not in (None, {}):
        raise ValueError("completion template must not claim historical integration evidence")
    lineage = entry.get("task_lineage")
    candidate = entry.get("candidate")
    graph = predecessor.state.get("canonical_dag")
    if not isinstance(lineage, dict) or not isinstance(candidate, dict) or not isinstance(graph, dict):
        raise ValueError("completion lineage, candidate, or graph is malformed")
    if lineage.get("integration_task") not in ("", "bootstrap-reconciliation"):
        raise ValueError("completion template must leave integration task to the recorder")
    commit = candidate.get("commit")
    digest = candidate.get("digest")
    if not isinstance(commit, str) or not isinstance(digest, str):
        raise ValueError("completion candidate pins are malformed")
    ancestry = live.ancestry.get(commit)
    if not isinstance(ancestry, dict) or ancestry.get("status") != "ancestor":
        raise ValueError("completion candidate lacks live canonical ancestry")
    identity = hashlib.sha256(
        f"PR 1\0{commit}\0{digest}\0{predecessor.generation}".encode("utf-8")
    ).hexdigest()[:24]
    attempt_id = f"bootstrap-reconciliation:PR1:{identity}"
    fence = f"v2-generation:{predecessor.generation}"
    integration_task = f"{attempt_id}:integration"
    reconciled = copy.deepcopy(entry)
    reconciled["task_lineage"]["integration_task"] = integration_task
    reconciled["attempt"] = {
        "attempt_id": attempt_id,
        "lease_fence_epoch": fence,
        "observed_steering_sequence": 0,
        "current_event_sequence": 0,
        "terminal_cas_sequence": 0,
        "terminal_cas_committed": True,
    }
    reconciled["steering_directives"] = []
    reconciled["steering_receipts"] = []
    integration: dict[str, Any] = {
        "integration_task": integration_task,
        "state": "integrated",
        "candidate_commit": commit,
        "canonical_commit": graph.get("source_commit"),
        "canonical_branch": live.canonical_ref,
        "source_set_digest": graph.get("source_set_digest"),
        "graph_revision": graph.get("graph_revision"),
        "graph_digest": graph.get("graph_digest"),
        "attempt_id": attempt_id,
        "lease_fence_epoch": fence,
        "steering_watermark": 0,
        "terminal_cas_sequence": 0,
        "ancestry_observation": ancestry,
        "receipt_digest": "",
    }
    integration["receipt_digest"] = execution_state.receipt_digest(integration)
    reconciled["integration"] = integration
    return reconciled


def load_predecessor(root: Path, canonical_ref: str,
                     expected_generation: str) -> Predecessor:
    if not GENERATION.fullmatch(expected_generation):
        raise ValueError("expected active generation is malformed")
    generation_path = root / bootstrap_execution.GENERATIONS / expected_generation
    manifest_path = generation_path / "manifest.json"
    state_path = generation_path / "state.json"
    for path in (manifest_path, state_path):
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"stored predecessor must be a regular non-symlink file: {path}")
    manifest_bytes = manifest_path.read_bytes()
    state_bytes = state_path.read_bytes()
    manifest = strict_json.loads_object(manifest_bytes, "predecessor manifest")
    state = strict_json.loads_object(state_bytes, "predecessor state")
    manifest_hex = hashlib.sha256(manifest_bytes).hexdigest()
    state_hex = hashlib.sha256(state_bytes).hexdigest()
    state_revision = state.get("canonical_dag", {}).get("graph_revision")
    actual_generation = f"r{state_revision}-{manifest_hex[:16]}-{state_hex[:16]}"
    if actual_generation != expected_generation:
        raise ValueError("expected predecessor generation does not match stored bytes")
    pointer = _generation_pointer(expected_generation, manifest_bytes, state_bytes)
    authority, reviews, tests = _observation_sets(root)
    live = live_evidence.inspect(
        root,
        canonical_ref,
        plan_execution.candidate_commits(state),
        review_receipts=reviews,
        test_receipts=tests,
        authority_review_receipts=authority,
    )
    validation = v2.validate(state, live)
    if validation.errors:
        raise ValueError("active V2 predecessor is invalid: " + "; ".join(validation.errors))
    if state.get("activation_mode") != "staged_dispatch":
        raise ValueError("active predecessor must be staged-dispatch V2 authority")
    if state.get("completion_ledger", {}).get("entries") != []:
        raise ValueError("PR 1 completion predecessor must have an empty ledger")
    dispatch = state.get("dispatch_specs")
    blocks = state.get("dispatch_blocks")
    if (
        not isinstance(dispatch, list)
        or [item.get("slice_id") for item in dispatch if isinstance(item, dict)] != ["PR 1"]
        or not isinstance(blocks, list)
        or len(blocks) != 256
    ):
        raise ValueError("completion predecessor must preserve the reviewed PR 1/256 partition")
    return Predecessor(
        generation=expected_generation,
        pointer_bytes=_bytes(pointer),
        manifest_bytes=manifest_bytes,
        state=state,
    )


def build_candidate(predecessor: Predecessor,
                    completion_entry: dict[str, Any]) -> dict[str, Any]:
    if completion_entry.get("slice_id") != "PR 1":
        raise ValueError("completion entry must be for PR 1")
    candidate = predecessor.state.copy()
    ledger = copy.deepcopy(predecessor.state["completion_ledger"])
    ledger["entries"] = [copy.deepcopy(completion_entry)]
    candidate["completion_ledger"] = ledger
    return candidate


def _install(
    root: Path,
    predecessor: Predecessor,
    candidate: dict[str, Any],
    state_bytes: bytes,
) -> tuple[Path, Path, bool]:
    manifest_bytes = predecessor.manifest_bytes
    state_hex = hashlib.sha256(state_bytes).hexdigest()
    manifest_hex = hashlib.sha256(manifest_bytes).hexdigest()
    revision = candidate["canonical_dag"]["graph_revision"]
    generation = f"r{revision}-{manifest_hex[:16]}-{state_hex[:16]}"
    pointer = _generation_pointer(generation, manifest_bytes, state_bytes)
    pointer_bytes = _bytes(pointer)
    generations = root / bootstrap_execution.GENERATIONS
    final = generations / generation
    active = root / bootstrap_execution.ACTIVE_POINTER
    with bootstrap_execution._activation_lock(root):
        current_bytes = active.read_bytes()
        if current_bytes == pointer_bytes:
            if (
                (final / "manifest.json").read_bytes() != manifest_bytes
                or (final / "state.json").read_bytes() != state_bytes
            ):
                raise ValueError("active replay generation bytes differ from candidate")
            return final / "state.json", active, True
        if current_bytes != predecessor.pointer_bytes:
            raise ValueError("active predecessor changed before completion compare-and-swap")
        generations.mkdir(parents=True, exist_ok=True)
        if not final.exists():
            staging = Path(tempfile.mkdtemp(prefix=f".{generation}.", dir=generations))
            try:
                bootstrap_execution._write_staged(staging / "manifest.json", manifest_bytes)
                bootstrap_execution._write_staged(staging / "state.json", state_bytes)
                bootstrap_execution._fsync_directory(staging)
                os.replace(staging, final)
                bootstrap_execution._fsync_directory(generations)
            finally:
                if staging.exists():
                    shutil.rmtree(staging)
        elif (
            (final / "manifest.json").read_bytes() != manifest_bytes
            or (final / "state.json").read_bytes() != state_bytes
        ):
            raise ValueError(f"existing execution generation {generation} has different bytes")
        bootstrap_execution._atomic_install(active, pointer)
    return final / "state.json", active, False


def _assert_active_fence(root: Path, predecessor: Predecessor,
                         supplied_candidate: dict[str, Any] | None) -> None:
    active = root / bootstrap_execution.ACTIVE_POINTER
    current = active.read_bytes()
    allowed = {predecessor.pointer_bytes}
    if supplied_candidate is not None:
        state_bytes = _bytes(supplied_candidate)
        revision = supplied_candidate.get("canonical_dag", {}).get("graph_revision")
        manifest_hex = hashlib.sha256(predecessor.manifest_bytes).hexdigest()
        state_hex = hashlib.sha256(state_bytes).hexdigest()
        generation = f"r{revision}-{manifest_hex[:16]}-{state_hex[:16]}"
        allowed.add(_bytes(_generation_pointer(generation, predecessor.manifest_bytes, state_bytes)))
    if current not in allowed:
        raise ValueError("active pointer is neither expected predecessor nor exact replay target")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--canonical-ref", required=True)
    parser.add_argument("--expected-active-generation", required=True)
    parser.add_argument("--completion-entry", type=Path, required=True)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--prepare-candidate", type=Path)
    modes.add_argument("--candidate", type=Path)
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        predecessor = load_predecessor(root, args.canonical_ref, args.expected_active_generation)
        supplied = plan_execution.strict_json(args.candidate) if args.candidate is not None else None
        _assert_active_fence(root, predecessor, supplied)
        entry_template = plan_execution.strict_json(args.completion_entry)
        packet = predecessor.state["dispatch_specs"][0]
        observed_entry = _observe_completion(root, packet, entry_template)
        authority, reviews, tests = _observation_sets(root, required=True)
        live = live_evidence.inspect(
            root,
            args.canonical_ref,
            [observed_entry["candidate"]["commit"]],
            review_receipts=reviews,
            test_receipts=tests,
            authority_review_receipts=authority,
        )
        entry = _bootstrap_reconciliation(predecessor, observed_entry, live)
        candidate = build_candidate(predecessor, entry)
        candidate_bytes = _bytes(candidate)
        validation = v2.validate(candidate, live)
        if validation.errors:
            raise ValueError("completion candidate is invalid: " + "; ".join(validation.errors))
        view = v2.next_ready(validation)
        if args.prepare_candidate is not None:
            bootstrap_execution._atomic_install(args.prepare_candidate, candidate)
            print(json.dumps({
                "valid": True,
                "mode": "prepared",
                "candidate": str(args.prepare_candidate),
                "expected_active_generation": predecessor.generation,
                "next_ready": [item["slice_id"] for item in view["next_ready"]],
                "blocked_count": len(view["blocked"]),
            }, sort_keys=True))
            return 0
        assert supplied is not None
        if _bytes(supplied) != candidate_bytes:
            raise ValueError("supplied candidate differs from deterministic completion candidate")
        state_path, active, replay = _install(
            root, predecessor, candidate, candidate_bytes
        )
        print(json.dumps({
            "valid": True,
            "mode": "replayed" if replay else "activated",
            "state": str(state_path),
            "active_pointer": str(active),
            "next_ready": [item["slice_id"] for item in view["next_ready"]],
            "blocked_count": len(view["blocked"]),
        }, sort_keys=True))
        return 0
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError, TypeError, OverflowError) as error:
        print(json.dumps({
            "valid": False,
            "errors": [f"completion: {type(error).__name__}: {error}"],
        }, sort_keys=True))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
