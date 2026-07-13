#!/usr/bin/env python3
"""Prepare or activate one reviewed, predecessor-fenced staged-dispatch revision."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import shutil
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

PACKET_SOURCE_PATH = Path(
    ".codex/skills/executing-tracedecay-v2-plan/scripts/staged_dispatch_pr1.json"
)
SOURCE_SCHEMA = "tracedecay.v2.staged-dispatch-source/v1"
GENERATION = re.compile(r"^r[1-9][0-9]*-[0-9a-f]{16}-[0-9a-f]{16}$")


@dataclass(frozen=True)
class Predecessor:
    generation: str
    pointer_bytes: bytes
    manifest: dict[str, Any]
    state: dict[str, Any]
    state_sha256: str
    live: live_evidence.LiveEvidence


@dataclass(frozen=True)
class ReviewedSource:
    document: dict[str, Any]
    blob_oid: str
    blob_sha256: str
    raw_bytes: bytes
    packet_bytes: bytes


def _bytes(document: dict[str, Any]) -> bytes:
    return compile_plan_authority._canonical_json_bytes(document)


def _sha256(payload: bytes) -> str:
    return "sha256:" + hashlib.sha256(payload).hexdigest()


def _git_blob(root: Path, commit: str, path: Path) -> tuple[str, bytes]:
    spec = f"{commit}:{path.as_posix()}"
    resolved = run_git(root, "rev-parse", "--verify", spec, max_output_bytes=256)
    if resolved.error is not None or resolved.returncode != 0:
        detail = resolved.error or resolved.stderr.decode("utf-8", "replace")
        raise ValueError(f"cannot resolve reviewed packet source {path}: {detail}")
    try:
        oid = resolved.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise ValueError(f"reviewed packet source blob ID is not ASCII: {path}") from error
    if not execution_state.COMMIT.fullmatch(oid):
        raise ValueError(f"reviewed packet source blob ID is malformed: {path}")
    kind = run_git(root, "cat-file", "-t", oid, max_output_bytes=32)
    if kind.error is not None or kind.returncode != 0 or kind.stdout != b"blob\n":
        raise ValueError(f"reviewed packet source object is not a blob: {path}")
    result = run_git(root, "cat-file", "blob", oid, max_output_bytes=1024 * 1024)
    if result.error is not None or result.returncode != 0:
        detail = result.error or result.stderr.decode("utf-8", "replace")
        raise ValueError(f"cannot read reviewed packet source {path}: {detail}")
    return oid, result.stdout


def load_reviewed_source(root: Path, commit: str,
                         path: Path = PACKET_SOURCE_PATH) -> ReviewedSource:
    blob_oid, raw_bytes = _git_blob(root, commit, path)
    source = strict_json.loads_object(raw_bytes, "packet source")
    fields = {
        "schema", "stage_id", "authority_revision", "checked_manifest_revision",
        "checked_manifest_digest", "checked_source_set_digest", "authorized_slice_ids",
        "packet",
    }
    if set(source) != fields:
        raise ValueError(f"packet source fields differ: {sorted(source)!r}")
    if source["schema"] != SOURCE_SCHEMA:
        raise ValueError(f"packet source schema must be {SOURCE_SCHEMA!r}")
    if source["authorized_slice_ids"] != ["PR 1"]:
        raise ValueError("packet source must authorize exactly PR 1")
    if source["authority_revision"] != source["checked_manifest_revision"] + 1:
        raise ValueError("packet source target revision must equal checked manifest revision plus one")
    return ReviewedSource(
        document=source,
        blob_oid=blob_oid,
        blob_sha256=v2.packet_source_digest(raw_bytes),
        raw_bytes=raw_bytes,
        packet_bytes=v2.packet_contract_bytes(source["packet"]),
    )


def _validate_packet_against_manifest(source: dict[str, Any], manifest: dict[str, Any]) -> None:
    manifest_bytes = _bytes(manifest)
    if source["checked_manifest_digest"] != _sha256(manifest_bytes):
        raise ValueError("packet source checked manifest digest mismatch")
    if source["checked_manifest_revision"] != manifest.get("graph_revision"):
        raise ValueError("packet source checked manifest revision mismatch")
    if source["checked_source_set_digest"] != manifest.get("source_set_digest"):
        raise ValueError("packet source checked source-set digest mismatch")
    slices = manifest.get("slices")
    authority = slices.get("PR 1") if isinstance(slices, dict) else None
    if not isinstance(authority, dict):
        raise ValueError("checked manifest lacks PR 1 authority")
    packet = source["packet"]
    if not isinstance(packet, dict):
        raise ValueError("packet source packet must be an object")
    expected_owner = authority.get("owner", {})
    anchor = expected_owner.get("anchor", {}) if isinstance(expected_owner, dict) else {}
    owner_string = (
        f"PR 1@{expected_owner.get('path')}:{anchor.get('start_line')}-{anchor.get('end_line')}"
        f"#sha256:{anchor.get('block_sha256')}"
    )
    checks = {
        "slice_id": "PR 1",
        "owner": owner_string,
        "content_digest": authority.get("content_digest"),
        "commit_subject": authority.get("commit_subject"),
        "prerequisites": sorted(
            item.get("parent") for item in authority.get("dependencies", [])
            if isinstance(item, dict) and isinstance(item.get("parent"), str)
        ),
    }
    for field, expected in checks.items():
        if packet.get(field) != expected:
            raise ValueError(f"packet source {field} differs from checked PR 1 authority")
    expected_acceptance = [
        (item.get("criterion_id"), item.get("text"))
        for item in authority.get("acceptance", []) if isinstance(item, dict)
    ]
    actual_acceptance = [
        (item.get("criterion_id"), item.get("text"))
        for item in packet.get("acceptance", []) if isinstance(item, dict)
    ]
    if actual_acceptance != expected_acceptance:
        raise ValueError("packet source acceptance differs from checked PR 1 authority")
    expected_blocks = authority.get("source_anchors")
    if packet.get("source_blocks") != expected_blocks:
        raise ValueError("packet source blocks differ from checked PR 1 authority")


def load_predecessor(root: Path, canonical_ref: str, expected_generation: str) -> Predecessor:
    pointer_path = root / bootstrap_execution.ACTIVE_POINTER
    active_pointer = strict_json.loads_object(pointer_path.read_bytes(), "active pointer")
    if active_pointer.get("schema") != bootstrap_execution.POINTER_SCHEMA:
        raise ValueError("active pointer schema mismatch")
    if not GENERATION.fullmatch(expected_generation):
        raise ValueError("expected predecessor generation is malformed")
    generation_path = root / bootstrap_execution.GENERATIONS / expected_generation
    manifest_bytes = (generation_path / "manifest.json").read_bytes()
    state_bytes = (generation_path / "state.json").read_bytes()
    manifest = strict_json.loads_object(manifest_bytes, "predecessor manifest")
    state = strict_json.loads_object(state_bytes, "predecessor state")
    manifest_hex = hashlib.sha256(manifest_bytes).hexdigest()
    state_hex = hashlib.sha256(state_bytes).hexdigest()
    actual_generation = f"r{manifest.get('graph_revision')}-{manifest_hex[:16]}-{state_hex[:16]}"
    if actual_generation != expected_generation:
        raise ValueError("expected predecessor generation does not match stored bytes")
    pointer = {
        "schema": bootstrap_execution.POINTER_SCHEMA,
        "generation": expected_generation,
        "manifest": f"v2-execution-generations/{expected_generation}/manifest.json",
        "state": f"v2-execution-generations/{expected_generation}/state.json",
        "manifest_sha256": manifest_hex,
        "state_sha256": state_hex,
    }
    pointer_bytes = _bytes(pointer)
    live = live_evidence.inspect(root, canonical_ref, [])
    validation = execution_state.validate(state, live)
    if validation.errors:
        raise ValueError("active predecessor is invalid: " + "; ".join(validation.errors))
    if state.get("activation_mode") != "verify_only":
        raise ValueError("active predecessor must be compiler-produced verify_only authority")
    if state.get("dispatch_specs") != [] or state.get("completion_ledger", {}).get("entries") != []:
        raise ValueError("active predecessor must contain no packets or ledger entries")
    revision = manifest.get("graph_revision")
    compiled, compiled_live = compile_plan_authority.compile_from_ref(root, canonical_ref, revision)
    if compiled_live.errors:
        raise ValueError("cannot verify compiler predecessor: " + "; ".join(compiled_live.errors))
    if _bytes(compiled.manifest) != _bytes(manifest) or _bytes(compiled.state) != _bytes(state):
        raise ValueError("active predecessor bytes differ from canonical compiler output")
    return Predecessor(
        generation=expected_generation,
        pointer_bytes=pointer_bytes,
        manifest=manifest,
        state=state,
        state_sha256=_sha256(state_bytes),
        live=live,
    )


def build_candidate(predecessor: Predecessor, reviewed_source: ReviewedSource, *, activated_at: str) -> dict[str, Any]:
    source = reviewed_source.document
    if v2.packet_source_digest(reviewed_source.raw_bytes) != reviewed_source.blob_sha256:
        raise ValueError("reviewed packet source raw-byte digest mismatch")
    if v2.packet_contract_bytes(source.get("packet")) != reviewed_source.packet_bytes:
        raise ValueError("reviewed packet source complete packet bytes mismatch")
    _validate_packet_against_manifest(source, predecessor.manifest)
    prior_graph = predecessor.state["canonical_dag"]
    target_revision = source["authority_revision"]
    if target_revision != prior_graph["graph_revision"] + 1:
        raise ValueError("target authority revision is not the exact predecessor successor")

    policy: dict[str, Any] = {
        "schema": v2.POLICY_SCHEMA,
        "stage_id": source["stage_id"],
        "authority_revision": target_revision,
        "authorized_slice_ids": list(source["authorized_slice_ids"]),
        "packet_source_path": PACKET_SOURCE_PATH.as_posix(),
        "packet_source_blob_oid": reviewed_source.blob_oid,
        "packet_source_digest": reviewed_source.blob_sha256,
        "checked_manifest_revision": source["checked_manifest_revision"],
        "checked_manifest_digest": source["checked_manifest_digest"],
        "checked_source_set_digest": source["checked_source_set_digest"],
        "policy_digest": "",
    }
    policy["policy_digest"] = v2.policy_digest(policy)
    packet = copy.deepcopy(source["packet"])
    prior_nodes = prior_graph["nodes"]
    blocks = [
        {
            "slice_id": node["id"],
            "stage_id": source["stage_id"],
            "reason_code": v2.BLOCK_REASON,
            "authority_revision": target_revision,
        }
        for node in prior_nodes if node["id"] != "PR 1"
    ]
    blocks.sort(key=lambda item: item["slice_id"])

    graph: dict[str, Any] = {
        "schema": v2.DAG_SCHEMA,
        "repository": prior_graph["repository"],
        "source_commit": prior_graph["source_commit"],
        "source_set_digest": prior_graph["source_set_digest"],
        "graph_revision": target_revision,
        "graph_digest": "",
        "dispatch_policy_digest": policy["policy_digest"],
        "packet_source_blob_oid": policy["packet_source_blob_oid"],
        "packet_source_digest": policy["packet_source_digest"],
        "dispatch_contract_set_digest": "",
        "activation_receipt": {},
        "nodes": [],
    }
    packet_map = {"PR 1": packet}
    block_map = {item["slice_id"]: item for item in blocks}
    entry_digests: dict[str, str] = {}
    for prior in prior_nodes:
        node = {
            "id": prior["id"],
            "owner": prior["owner"],
            "content_digest": prior["content_digest"],
            "dispatch_digest": "",
            "dependencies": list(prior["dependencies"]),
        }
        if node["id"] == "PR 1":
            digest = v2.dispatch_entry_digest(
                kind="authorized_packet", slice_id=node["id"], payload=packet_map[node["id"]],
                graph=graph, policy=policy,
            )
        else:
            digest = v2.dispatch_entry_digest(
                kind="blocked_node", slice_id=node["id"], payload=block_map[node["id"]],
                graph=graph, policy=policy,
            )
        node["dispatch_digest"] = digest
        entry_digests[node["id"]] = digest
        graph["nodes"].append(node)
    graph["dispatch_contract_set_digest"] = v2.dispatch_contract_set_digest(entry_digests)
    graph["graph_digest"] = v2.graph_digest(graph)
    transition_id = (
        f"transition:r{prior_graph['graph_revision']}-r{target_revision}:"
        f"{reviewed_source.blob_sha256[7:23]}"
    )
    graph["activation_receipt"] = {
        "receipt_id": f"activation:{transition_id}",
        "transition_id": transition_id,
        "stage_id": source["stage_id"],
        "repository": graph["repository"],
        "source_commit": graph["source_commit"],
        "source_set_digest": graph["source_set_digest"],
        "graph_revision": graph["graph_revision"],
        "graph_digest": graph["graph_digest"],
        "dispatch_policy_digest": graph["dispatch_policy_digest"],
        "packet_source_blob_oid": graph["packet_source_blob_oid"],
        "packet_source_digest": graph["packet_source_digest"],
        "dispatch_contract_set_digest": graph["dispatch_contract_set_digest"],
        "slice_count": len(graph["nodes"]),
        "edge_count": sum(len(node["dependencies"]) for node in graph["nodes"]),
        "authorized_count": 1,
        "blocked_count": len(blocks),
        "validator_version": v2.VALIDATOR_VERSION,
        "activated": True,
    }
    ledger = copy.deepcopy(predecessor.state["completion_ledger"])
    ledger["graph_revision"] = graph["graph_revision"]
    ledger["graph_digest"] = graph["graph_digest"]
    transition = {
        "schema": v2.TRANSITION_SCHEMA,
        "transition_id": transition_id,
        "stage_id": source["stage_id"],
        "repository": graph["repository"],
        "source_commit": graph["source_commit"],
        "source_set_digest": graph["source_set_digest"],
        "manifest_digest": source["checked_manifest_digest"],
        "checked_manifest_revision": source["checked_manifest_revision"],
        "expected_prior_generation": predecessor.generation,
        "prior_state_sha256": predecessor.state_sha256,
        "prior_graph_revision": prior_graph["graph_revision"],
        "prior_graph_digest": prior_graph["graph_digest"],
        "target_graph_revision": graph["graph_revision"],
        "target_graph_digest": graph["graph_digest"],
        "enabled_slice_ids": ["PR 1"],
        "dispatch_blocks_digest": v2.block_set_digest(blocks),
        "dispatch_blocks_count": len(blocks),
        "dispatch_contract_set_digest": graph["dispatch_contract_set_digest"],
        "packet_source_path": PACKET_SOURCE_PATH.as_posix(),
        "packet_source_blob_oid": reviewed_source.blob_oid,
        "packet_source_digest": reviewed_source.blob_sha256,
        "candidate_state_digest": "",
        "authority_review": None,
        "validator_version": v2.VALIDATOR_VERSION,
        "activation_sequence": 1,
        "activated_at": activated_at,
    }
    state = {
        "schema": v2.EXPORT_SCHEMA,
        "activation_mode": "staged_dispatch",
        "canonical_dag": graph,
        "completion_ledger": ledger,
        "dispatch_policy": policy,
        "dispatch_specs": [packet],
        "dispatch_blocks": blocks,
        "authority_transition": transition,
        "retired_obligations": copy.deepcopy(predecessor.state["retired_obligations"]),
    }
    transition["candidate_state_digest"] = v2.candidate_state_digest(state)
    return state


def load_authority_review(path: Path, candidate: dict[str, Any],
                          observed_receipt_digests: frozenset[str]) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise ValueError("authority review receipt must be a regular non-symlink file")
    review = plan_execution.strict_json(path)
    transition = candidate["authority_transition"]
    expected = {
        "candidate_state_digest": transition["candidate_state_digest"],
        "packet_source_blob_oid": transition["packet_source_blob_oid"],
        "packet_source_digest": transition["packet_source_digest"],
        "prior_generation": transition["expected_prior_generation"],
        "prior_state_sha256": transition["prior_state_sha256"],
        "prior_graph_revision": transition["prior_graph_revision"],
        "prior_graph_digest": transition["prior_graph_digest"],
    }
    for field, value in expected.items():
        if review.get(field) != value:
            raise ValueError(f"authority review receipt {field} mismatch")
    receipt_digest = review.get("receipt_digest")
    if receipt_digest != v2.authority_review_digest(review):
        raise ValueError("authority review receipt digest mismatch")
    if receipt_digest not in observed_receipt_digests:
        raise ValueError("authority review receipt digest absent from trusted observation set")
    if review.get("schema") != v2.REVIEW_SCHEMA:
        raise ValueError("authority review receipt schema mismatch")
    if review.get("verdict") != "approved" or review.get("independent") is not True:
        raise ValueError("authority review receipt must be independently approved")
    if review.get("reviewer_authority") == review.get("implementation_authority"):
        raise ValueError("authority review authority must differ from implementation authority")
    return review


def _candidate_without_review(candidate: dict[str, Any]) -> dict[str, Any]:
    value = copy.deepcopy(candidate)
    value["authority_transition"]["authority_review"] = None
    return value


def _install(root: Path, predecessor: Predecessor, candidate: dict[str, Any]) -> tuple[Path, Path]:
    state_bytes = _bytes(candidate)
    manifest_bytes = _bytes(predecessor.manifest)
    state_hex = hashlib.sha256(state_bytes).hexdigest()
    manifest_hex = hashlib.sha256(manifest_bytes).hexdigest()
    revision = candidate["canonical_dag"]["graph_revision"]
    generation = f"r{revision}-{manifest_hex[:16]}-{state_hex[:16]}"
    generations = root / bootstrap_execution.GENERATIONS
    final = generations / generation
    active = root / bootstrap_execution.ACTIVE_POINTER
    with bootstrap_execution._activation_lock(root):
        if active.read_bytes() != predecessor.pointer_bytes:
            current = strict_json.loads_object(active.read_bytes(), "active pointer")
            if current.get("generation") == generation:
                current_state = root / ".tracedecay" / current["state"]
                if current_state.read_bytes() == state_bytes:
                    return final / "state.json", active
            raise ValueError("active predecessor changed before compare-and-swap")
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
        elif (final / "manifest.json").read_bytes() != manifest_bytes or (final / "state.json").read_bytes() != state_bytes:
            raise ValueError(f"existing execution generation {generation} has different bytes")
        pointer = {
            "schema": bootstrap_execution.POINTER_SCHEMA,
            "generation": generation,
            "manifest": f"v2-execution-generations/{generation}/manifest.json",
            "state": f"v2-execution-generations/{generation}/state.json",
            "manifest_sha256": manifest_hex,
            "state_sha256": state_hex,
        }
        bootstrap_execution._atomic_install(active, pointer)
    return final / "state.json", active


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--canonical-ref", required=True)
    parser.add_argument("--expected-active-generation", required=True)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--prepare-candidate", type=Path)
    modes.add_argument("--candidate", type=Path)
    parser.add_argument("--trusted-review-receipt", type=Path)
    parser.add_argument("--activated-at", required=True)
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        predecessor = load_predecessor(root, args.canonical_ref, args.expected_active_generation)
        source = load_reviewed_source(root, predecessor.live.canonical_commit or "")
        candidate = build_candidate(
            predecessor, source, activated_at=args.activated_at,
        )
        if args.prepare_candidate is not None:
            args.prepare_candidate.parent.mkdir(parents=True, exist_ok=True)
            args.prepare_candidate.write_bytes(_bytes(candidate))
            print(json.dumps({
                "valid": True,
                "mode": "prepared",
                "candidate": str(args.prepare_candidate),
                "candidate_state_digest": candidate["authority_transition"]["candidate_state_digest"],
                "packet_source_blob_oid": source.blob_oid,
                "packet_source_digest": source.blob_sha256,
                "expected_active_generation": predecessor.generation,
            }, sort_keys=True))
            return 0
        if args.trusted_review_receipt is None:
            raise ValueError("--trusted-review-receipt is required for activation")
        supplied = plan_execution.strict_json(args.candidate)
        if _bytes(_candidate_without_review(supplied)) != _bytes(candidate):
            raise ValueError("supplied candidate bytes differ from deterministic reviewed-source candidate")
        observed = live_evidence.load_authority_review_observations(root, required=True)
        review = load_authority_review(args.trusted_review_receipt, candidate, observed)
        candidate["authority_transition"]["authority_review"] = review
        validation_live = replace(predecessor.live, authority_review_receipts=observed)
        validation = v2.validate(candidate, validation_live)
        if validation.errors:
            raise ValueError("candidate V2 authority is invalid: " + "; ".join(validation.errors))
        state_path, active = _install(root, predecessor, candidate)
        print(json.dumps({
            "valid": True,
            "mode": "activated",
            "state": str(state_path),
            "active_pointer": str(active),
            "candidate_state_digest": candidate["authority_transition"]["candidate_state_digest"],
            "next_ready": [item["slice_id"] for item in v2.next_ready(validation)["next_ready"]],
        }, sort_keys=True))
        return 0
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError, TypeError, OverflowError) as error:
        print(json.dumps({"valid": False, "errors": [f"transition: {type(error).__name__}: {error}"]}, sort_keys=True))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
