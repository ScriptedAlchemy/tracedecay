#!/usr/bin/env python3
"""Validate the explicit TraceDecay V2 staged-dispatch authority schema."""

from __future__ import annotations

import copy
import datetime
import hashlib
import html
import json
import re
from dataclasses import dataclass
from typing import Any, cast

import execution_state as v1
import live_evidence as le
import slice_authority as sa
import strict_json
from git_observation import run_git

EXPORT_SCHEMA = "tracedecay.v2.execution-state/v2"
DAG_SCHEMA = "tracedecay.v2.canonical-dag/v2"
VIEW_SCHEMA = "tracedecay.v2.next-ready-view/v2"
POLICY_SCHEMA = "tracedecay.v2.dispatch-policy/v1"
TRANSITION_SCHEMA = "tracedecay.v2.authority-transition/v1"
REVIEW_SCHEMA = "tracedecay.v2.authority-review-receipt/v1"
PACKET_SOURCE_PATH = ".codex/skills/executing-tracedecay-v2-plan/scripts/staged_dispatch_pr1.json"
MANIFEST_PATH = "docs/plans/tracedecay-v2/execution-authority.json"
BLOCK_REASON = "not_in_dispatch_scope"
VALIDATOR_VERSION = "execution_state_v2/v1"
RFC3339_UTC = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z$")


@dataclass(frozen=True)
class ValidationResult:
    errors: tuple[str, ...]
    nodes: dict[str, dict[str, Any]]
    entries: dict[str, dict[str, Any]]
    dispatch: dict[str, dict[str, Any]]
    blocks: dict[str, dict[str, Any]]
    graph: dict[str, Any]
    policy: dict[str, Any]
    transition: dict[str, Any]

    @property
    def valid(self) -> bool:
        return not self.errors


def _digest(domain: str, value: object) -> str:
    payload = {"domain": domain, "payload": value}
    return "sha256:" + hashlib.sha256(sa._canonical_json(payload).encode("utf-8")).hexdigest()


def packet_source_digest(source_bytes: bytes) -> str:
    """Hash the exact immutable Git blob bytes; the source cannot attest itself."""
    return "sha256:" + hashlib.sha256(source_bytes).hexdigest()


def packet_contract_bytes(packet: object) -> bytes:
    return sa._canonical_json(packet).encode("utf-8")


def policy_digest(policy: dict[str, Any]) -> str:
    return _digest(
        "tracedecay.v2.dispatch-policy/v1",
        {key: value for key, value in policy.items() if key != "policy_digest"},
    )


def authority_review_digest(review: dict[str, Any]) -> str:
    return _digest(
        "tracedecay.v2.authority-review-receipt/v1",
        {key: value for key, value in review.items() if key != "receipt_digest"},
    )


def block_set_digest(blocks: list[dict[str, Any]]) -> str:
    return _digest("tracedecay.v2.dispatch-block-set/v1", blocks)


def dispatch_entry_digest(
    *, kind: str, slice_id: str, payload: dict[str, Any], graph: dict[str, Any],
    policy: dict[str, Any],
) -> str:
    return _digest(
        "tracedecay.v2.dispatch-entry/v1",
        {
            "schema": "tracedecay.v2.dispatch-entry/v1",
            "kind": kind,
            "slice_id": slice_id,
            "repository": graph.get("repository"),
            "source_commit": graph.get("source_commit"),
            "source_set_digest": graph.get("source_set_digest"),
            "graph_revision": graph.get("graph_revision"),
            "dispatch_policy_digest": policy.get("policy_digest"),
            "packet_source_blob_oid": policy.get("packet_source_blob_oid"),
            "packet_source_digest": policy.get("packet_source_digest"),
            "payload": payload,
        },
    )


def dispatch_contract_set_digest(entries: dict[str, str]) -> str:
    return _digest(
        "tracedecay.v2.dispatch-contract-set/v1",
        [{"slice_id": slice_id, "entry_digest": entries[slice_id]} for slice_id in sorted(entries)],
    )


def graph_digest(graph: dict[str, Any]) -> str:
    return _digest(
        "tracedecay.v2.canonical-dag/v2",
        {
            "schema": graph.get("schema"),
            "repository": graph.get("repository"),
            "source_commit": graph.get("source_commit"),
            "source_set_digest": graph.get("source_set_digest"),
            "graph_revision": graph.get("graph_revision"),
            "dispatch_policy_digest": graph.get("dispatch_policy_digest"),
            "packet_source_blob_oid": graph.get("packet_source_blob_oid"),
            "packet_source_digest": graph.get("packet_source_digest"),
            "dispatch_contract_set_digest": graph.get("dispatch_contract_set_digest"),
            "nodes": graph.get("nodes"),
        },
    )


def candidate_state_digest(document: dict[str, Any]) -> str:
    """Seal immutable staged authority without sealing its append-only completion log."""
    payload = copy.deepcopy(document)
    ledger = payload.get("completion_ledger")
    if isinstance(ledger, dict):
        ledger["entries"] = []
    transition = payload.get("authority_transition")
    if isinstance(transition, dict):
        transition["candidate_state_digest"] = ""
        transition["authority_review"] = None
    return _digest("tracedecay.v2.execution-state-candidate/v2", payload)


def _keys(value: object, expected: set[str], label: str, errors: list[str]) -> bool:
    return v1._keys(value, expected, label, errors)


def _strings(value: object, label: str, errors: list[str], *, maximum: int | None = None) -> bool:
    return v1._strings(value, label, errors, maximum=maximum)


def _pin(label: str, actual: object, expected: object, errors: list[str]) -> None:
    v1._pin_equal(label, actual, expected, errors)


def _integer(value: object, label: str, errors: list[str], *, minimum: int = 0) -> bool:
    if isinstance(value, bool) or not isinstance(value, int):
        errors.append(f"{label}: must be an integer")
        return False
    if value < minimum:
        errors.append(f"{label}: must be at least {minimum}")
        return False
    return True


def _validate_rfc3339_utc(value: object, label: str, errors: list[str]) -> None:
    if not isinstance(value, str) or not RFC3339_UTC.fullmatch(value):
        errors.append(f"{label}: must be a canonical RFC3339 UTC timestamp ending in Z")
        return
    try:
        datetime.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError:
        errors.append(f"{label}: must be a valid RFC3339 UTC timestamp")


def validate(document: dict[str, Any], live: le.LiveEvidence | None = None) -> ValidationResult:
    errors: list[str] = []
    fields = {
        "schema", "activation_mode", "canonical_dag", "completion_ledger",
        "dispatch_policy", "dispatch_specs", "dispatch_blocks", "authority_transition",
        "retired_obligations",
    }
    if not _keys(document, fields, "export", errors):
        return ValidationResult(v1._finalize_errors(errors), {}, {}, {}, {}, {}, {}, {})
    if document.get("schema") != EXPORT_SCHEMA:
        errors.append(f"export.schema: expected {EXPORT_SCHEMA!r}")
    if document.get("activation_mode") != "staged_dispatch":
        errors.append("export.activation_mode: expected 'staged_dispatch'")
    if live is None:
        errors.append("live: authoritative checkout evidence is required")
    else:
        errors.extend(live.errors)

    retired = document.get("retired_obligations")
    retired_list = cast(list[str], retired) if isinstance(retired, list) else []
    if _strings(retired, "retired_obligations", errors):
        if retired_list != sorted(retired_list):
            errors.append("retired_obligations: values must be in canonical order")
        if "FM-168" not in retired_list:
            errors.append("retired_obligations: corrected tombstone FM-168 must remain retired")

    graph = _validate_graph(document.get("canonical_dag"), live, set(retired_list), errors)
    nodes = graph.pop("_nodes", {}) if graph else {}
    policy = _validate_policy(document.get("dispatch_policy"), graph, errors)
    dispatch = _validate_dispatch(document.get("dispatch_specs"), nodes, graph, policy, errors)
    _validate_reviewed_source(policy, dispatch, live, errors)
    blocks = _validate_blocks(document.get("dispatch_blocks"), nodes, graph, policy, errors)
    _validate_partition(nodes, dispatch, blocks, errors)
    _validate_contract_set(graph, policy, nodes, dispatch, blocks, errors)
    transition = _validate_transition(
        document.get("authority_transition"), document, graph, policy, blocks, live, errors
    )
    entries = v1._validate_ledger(
        document.get("completion_ledger"), graph, nodes, dispatch, set(retired_list), live, errors
    )
    return ValidationResult(
        v1._finalize_errors(errors), nodes, entries, dispatch, blocks, graph, policy, transition
    )


def _validate_graph(raw: object, live: le.LiveEvidence | None, retired: set[str],
                    errors: list[str]) -> dict[str, Any]:
    fields = {
        "schema", "repository", "source_commit", "source_set_digest", "graph_revision",
        "graph_digest", "dispatch_policy_digest", "packet_source_blob_oid",
        "packet_source_digest", "dispatch_contract_set_digest", "activation_receipt", "nodes",
    }
    if not _keys(raw, fields, "canonical_dag", errors):
        return {}
    graph = cast(dict[str, Any], raw)
    if graph["schema"] != DAG_SCHEMA:
        errors.append(f"canonical_dag.schema: expected {DAG_SCHEMA!r}")
    if not isinstance(graph["repository"], str) or not graph["repository"]:
        errors.append("canonical_dag.repository: must be non-empty")
    if not isinstance(graph["source_commit"], str) or not v1.COMMIT.fullmatch(graph["source_commit"]):
        errors.append("canonical_dag.source_commit: must be a full lowercase commit SHA")
    if not isinstance(graph["source_set_digest"], str) or not v1.SHA256.fullmatch(graph["source_set_digest"]):
        errors.append("canonical_dag.source_set_digest: must be sha256:<64 lowercase hex>")
    _integer(graph["graph_revision"], "canonical_dag.graph_revision", errors, minimum=1)
    if not isinstance(graph["packet_source_blob_oid"], str) or not v1.COMMIT.fullmatch(
        graph["packet_source_blob_oid"]
    ):
        errors.append("canonical_dag.packet_source_blob_oid: must be a full lowercase Git object ID")
    for field in ("dispatch_policy_digest", "packet_source_digest", "dispatch_contract_set_digest"):
        if not isinstance(graph[field], str) or not v1.SHA256.fullmatch(graph[field]):
            errors.append(f"canonical_dag.{field}: must be sha256:<64 lowercase hex>")
    if live is not None:
        _pin("canonical_dag.repository", graph["repository"], live.repository, errors)
        _pin("canonical_dag.source_commit", graph["source_commit"], live.canonical_commit, errors)
        _pin("canonical_dag.source_set_digest", graph["source_set_digest"], live.source_set_digest, errors)
    nodes = v1._validate_nodes(graph.get("nodes"), retired, errors)
    if graph.get("graph_digest") != graph_digest(graph):
        errors.append("canonical_dag.graph_digest: digest does not match canonical V2 DAG bytes")
    _validate_activation_receipt(graph, errors)
    return {**graph, "_nodes": nodes}


def _validate_activation_receipt(graph: dict[str, Any], errors: list[str]) -> None:
    fields = {
        "receipt_id", "transition_id", "stage_id", "repository", "source_commit",
        "source_set_digest", "graph_revision", "graph_digest", "dispatch_policy_digest",
        "packet_source_blob_oid", "packet_source_digest", "dispatch_contract_set_digest",
        "slice_count", "edge_count",
        "authorized_count", "blocked_count", "validator_version", "activated",
    }
    receipt = graph.get("activation_receipt")
    if not _keys(receipt, fields, "canonical_dag.activation_receipt", errors):
        return
    receipt = cast(dict[str, Any], receipt)
    _integer(
        receipt["graph_revision"], "canonical_dag.activation_receipt.graph_revision",
        errors, minimum=1,
    )
    for field in ("slice_count", "edge_count", "authorized_count", "blocked_count"):
        _integer(receipt[field], f"canonical_dag.activation_receipt.{field}", errors)
    for field in (
        "repository", "source_commit", "source_set_digest", "graph_revision", "graph_digest",
        "dispatch_policy_digest", "packet_source_blob_oid", "packet_source_digest",
        "dispatch_contract_set_digest",
    ):
        _pin(f"canonical_dag.activation_receipt.{field}", receipt[field], graph[field], errors)
    counts = {
        "slice_count": len(graph.get("nodes", [])) if isinstance(graph.get("nodes"), list) else -1,
        "edge_count": sum(
            len(node.get("dependencies", [])) for node in graph.get("nodes", [])
            if isinstance(node, dict) and isinstance(node.get("dependencies"), list)
        ),
    }
    for field, expected in counts.items():
        _pin(f"canonical_dag.activation_receipt.{field}", receipt[field], expected, errors)
    if receipt.get("validator_version") != VALIDATOR_VERSION:
        errors.append("canonical_dag.activation_receipt.validator_version: unsupported validator")
    if receipt.get("activated") is not True:
        errors.append("canonical_dag.activation_receipt.activated: must be true")


def _validate_policy(raw: object, graph: dict[str, Any], errors: list[str]) -> dict[str, Any]:
    fields = {
        "schema", "stage_id", "authority_revision", "authorized_slice_ids",
        "packet_source_path", "packet_source_blob_oid", "packet_source_digest",
        "checked_manifest_revision",
        "checked_manifest_digest", "checked_source_set_digest", "policy_digest",
    }
    if not _keys(raw, fields, "dispatch_policy", errors):
        return {}
    policy = cast(dict[str, Any], raw)
    if policy["schema"] != POLICY_SCHEMA:
        errors.append(f"dispatch_policy.schema: expected {POLICY_SCHEMA!r}")
    if not isinstance(policy["stage_id"], str) or not policy["stage_id"]:
        errors.append("dispatch_policy.stage_id: must be non-empty")
    _integer(policy["authority_revision"], "dispatch_policy.authority_revision", errors, minimum=1)
    _integer(
        policy["checked_manifest_revision"], "dispatch_policy.checked_manifest_revision",
        errors, minimum=1,
    )
    if policy["authority_revision"] != graph.get("graph_revision"):
        errors.append("dispatch_policy.authority_revision: must equal graph revision")
    if _strings(policy["authorized_slice_ids"], "dispatch_policy.authorized_slice_ids", errors):
        if policy["authorized_slice_ids"] != sorted(policy["authorized_slice_ids"]):
            errors.append("dispatch_policy.authorized_slice_ids: must be canonical order")
        if policy["authorized_slice_ids"] != ["PR 1"]:
            errors.append("dispatch_policy.authorized_slice_ids: initial stage authorizes exactly PR 1")
    if not isinstance(policy["packet_source_path"], str) or not policy["packet_source_path"]:
        errors.append("dispatch_policy.packet_source_path: must be non-empty")
    if not isinstance(policy["packet_source_blob_oid"], str) or not v1.COMMIT.fullmatch(
        policy["packet_source_blob_oid"]
    ):
        errors.append("dispatch_policy.packet_source_blob_oid: must be a full lowercase Git object ID")
    for field in ("packet_source_digest", "checked_manifest_digest", "checked_source_set_digest"):
        if not isinstance(policy[field], str) or not v1.SHA256.fullmatch(policy[field]):
            errors.append(f"dispatch_policy.{field}: must be sha256:<64 lowercase hex>")
    if policy["checked_source_set_digest"] != graph.get("source_set_digest"):
        errors.append("dispatch_policy.checked_source_set_digest: differs from graph source set")
    if policy["packet_source_blob_oid"] != graph.get("packet_source_blob_oid"):
        errors.append("dispatch_policy.packet_source_blob_oid: differs from graph")
    if policy["packet_source_digest"] != graph.get("packet_source_digest"):
        errors.append("dispatch_policy.packet_source_digest: differs from graph")
    if policy["policy_digest"] != policy_digest(policy):
        errors.append("dispatch_policy.policy_digest: digest mismatch")
    if policy["policy_digest"] != graph.get("dispatch_policy_digest"):
        errors.append("dispatch_policy.policy_digest: differs from graph")
    return policy


def _git_blob_observation(root: Any, commit: str, path: str,
                          *, maximum: int) -> tuple[str, bytes]:
    oid_result = run_git(root, "rev-parse", "--verify", f"{commit}:{path}", max_output_bytes=256)
    if oid_result.error is not None or oid_result.returncode != 0:
        raise ValueError(f"cannot resolve canonical Git blob {path}")
    try:
        oid = oid_result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise ValueError(f"canonical Git blob ID for {path} is not ASCII") from error
    if not v1.COMMIT.fullmatch(oid):
        raise ValueError(f"canonical Git blob ID for {path} is malformed")
    type_result = run_git(root, "cat-file", "-t", oid, max_output_bytes=32)
    if type_result.error is not None or type_result.returncode != 0 or type_result.stdout != b"blob\n":
        raise ValueError(f"canonical Git object for {path} is not a blob")
    source_result = run_git(root, "cat-file", "blob", oid, max_output_bytes=maximum)
    if source_result.error is not None or source_result.returncode != 0:
        raise ValueError(f"cannot read canonical Git blob {path}")
    return oid, source_result.stdout


def _validate_reviewed_source(policy: dict[str, Any], dispatch: dict[str, dict[str, Any]],
                              live: le.LiveEvidence | None, errors: list[str]) -> None:
    if not policy or live is None or live.canonical_commit is None:
        return
    if policy.get("packet_source_path") != PACKET_SOURCE_PATH:
        errors.append(f"dispatch_policy.packet_source_path: expected {PACKET_SOURCE_PATH!r}")
        return
    try:
        source_oid, source_bytes = _git_blob_observation(
            live.root, live.canonical_commit, PACKET_SOURCE_PATH, maximum=1024 * 1024
        )
        source = strict_json.loads_object(source_bytes, "packet source")
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError, TypeError):
        errors.append("dispatch_policy.packet_source: unavailable or invalid in canonical Git tree")
        return
    if source_oid != policy.get("packet_source_blob_oid"):
        errors.append("dispatch_policy.packet_source_blob_oid: differs from canonical Git blob")
    if packet_source_digest(source_bytes) != policy.get("packet_source_digest"):
        errors.append("dispatch_policy.packet_source_digest: differs from exact canonical Git blob bytes")
    if source.get("authorized_slice_ids") != policy.get("authorized_slice_ids"):
        errors.append("dispatch_policy.packet_source: authorized IDs differ from checked source")
    _integer(source.get("authority_revision"), "dispatch_policy.packet_source.authority_revision", errors, minimum=1)
    if source.get("authority_revision") != policy.get("authority_revision"):
        errors.append("dispatch_policy.packet_source: authority revision differs from checked source")
    if packet_contract_bytes(source.get("packet")) != packet_contract_bytes(dispatch.get("PR 1")):
        errors.append("dispatch_policy.packet_source: complete PR 1 packet bytes differ from checked source")

    try:
        _, manifest_bytes = _git_blob_observation(
            live.root, live.canonical_commit, MANIFEST_PATH, maximum=4 * 1024 * 1024
        )
        manifest = strict_json.loads_object(manifest_bytes, "checked manifest")
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError, TypeError):
        errors.append("dispatch_policy.checked_manifest: unavailable or invalid in canonical Git tree")
        return
    if _digest_raw(manifest_bytes) != policy.get("checked_manifest_digest"):
        errors.append("dispatch_policy.checked_manifest_digest: differs from canonical Git blob")
    _integer(manifest.get("graph_revision"), "dispatch_policy.checked_manifest.graph_revision", errors, minimum=1)
    if manifest.get("graph_revision") != policy.get("checked_manifest_revision"):
        errors.append("dispatch_policy.checked_manifest_revision: differs from canonical manifest")
    if manifest.get("source_set_digest") != policy.get("checked_source_set_digest"):
        errors.append("dispatch_policy.checked_source_set_digest: differs from canonical manifest")


def _digest_raw(payload: bytes) -> str:
    return "sha256:" + hashlib.sha256(payload).hexdigest()


def _validate_dispatch(raw: object, nodes: dict[str, dict[str, Any]], graph: dict[str, Any],
                       policy: dict[str, Any], errors: list[str]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    if not isinstance(raw, list):
        errors.append("dispatch_specs: must be an array")
        return result
    for index, packet in enumerate(raw):
        label = f"dispatch_specs[{index}]"
        if not isinstance(packet, dict):
            errors.append(f"{label}: must be an object")
            continue
        slice_id = packet.get("slice_id")
        if not isinstance(slice_id, str) or not slice_id:
            errors.append(f"{label}.slice_id: must be non-empty")
            continue
        if slice_id in result:
            errors.append(f"dispatch_specs: duplicate packet for {slice_id}")
            continue
        _validate_packet(packet, slice_id, nodes.get(slice_id), errors)
        result[slice_id] = packet
    ids = [packet.get("slice_id") for packet in raw if isinstance(packet, dict)]
    if all(isinstance(item, str) for item in ids) and ids != sorted(ids):
        errors.append("dispatch_specs: packets must be canonical order")
    if set(result) != set(policy.get("authorized_slice_ids", [])):
        errors.append("dispatch_specs: packets must exactly equal reviewed authorized slice IDs")
    for slice_id, packet in result.items():
        expected = dispatch_entry_digest(
            kind="authorized_packet", slice_id=slice_id, payload=packet, graph=graph, policy=policy
        )
        if nodes.get(slice_id, {}).get("dispatch_digest") != expected:
            errors.append(f"{slice_id}.dispatch: sealed packet digest mismatch")
    return result


def _validate_packet(packet: dict[str, Any], slice_id: str, node: dict[str, Any] | None,
                     errors: list[str]) -> None:
    fields = {
        "slice_id", "owner", "content_digest", "commit_subject", "acceptance",
        "source_blocks", "prerequisites", "exact_files", "acceptance_commands",
        "required_tests", "workspace", "lane", "claude_adversarial_review", "gates",
        "retrieval_anchors", "prohibitions",
    }
    if not _keys(packet, fields, f"{slice_id}.packet", errors):
        return
    if node is None:
        errors.append(f"dispatch_specs: unknown slice {slice_id}")
        return
    _pin(f"{slice_id}.packet.owner", packet["owner"], node.get("owner"), errors)
    _pin(f"{slice_id}.packet.content_digest", packet["content_digest"], node.get("content_digest"), errors)
    _pin(f"{slice_id}.packet.prerequisites", packet["prerequisites"], node.get("dependencies"), errors)
    if slice_id != "PR 1":
        errors.append("dispatch_specs: initial stage packet must be PR 1")
    if not isinstance(packet["commit_subject"], str) or not packet["commit_subject"]:
        errors.append(f"{slice_id}.packet.commit_subject: must be non-empty")
    for field, maximum in (
        ("exact_files", v1.MAX_FILES), ("acceptance_commands", v1.MAX_COMMANDS),
        ("required_tests", v1.MAX_COMMANDS), ("retrieval_anchors", v1.MAX_ANCHORS),
        ("prohibitions", v1.MAX_COMMANDS),
    ):
        _strings(packet[field], f"{slice_id}.packet.{field}", errors, maximum=maximum)
        if isinstance(packet[field], list) and not packet[field]:
            errors.append(f"{slice_id}.packet.{field}: must be non-empty")
    if isinstance(packet["exact_files"], list) and packet["exact_files"] != sorted(packet["exact_files"]):
        errors.append(f"{slice_id}.packet.exact_files: must be canonical order")
    if isinstance(packet["required_tests"], list) and packet["required_tests"] != sorted(packet["required_tests"]):
        errors.append(f"{slice_id}.packet.required_tests: must be canonical order")
    _validate_acceptance(packet["acceptance"], packet["source_blocks"], slice_id, errors)
    _validate_workspace(packet["workspace"], slice_id, errors)
    v1._validate_lane(packet["lane"], slice_id, errors)
    _validate_claude(packet["claude_adversarial_review"], slice_id, errors)
    _validate_gates(packet["gates"], slice_id, errors)
    v1._bounded_scalars(packet, f"{slice_id}.packet", errors)


def _validate_acceptance(acceptance: object, source_blocks: object, slice_id: str,
                         errors: list[str]) -> None:
    block_fields = {"path", "start_line", "end_line", "block_sha256"}
    refs: set[str] = set()
    if not isinstance(source_blocks, list) or len(source_blocks) != 2:
        errors.append(f"{slice_id}.packet.source_blocks: must contain the two reviewed source blocks")
    else:
        for index, block in enumerate(source_blocks):
            if not _keys(block, block_fields, f"{slice_id}.packet.source_blocks[{index}]", errors):
                continue
            block = cast(dict[str, Any], block)
            start_valid = _integer(
                block["start_line"], f"{slice_id}.packet.source_blocks[{index}].start_line",
                errors, minimum=1,
            )
            end_valid = _integer(
                block["end_line"], f"{slice_id}.packet.source_blocks[{index}].end_line",
                errors, minimum=1,
            )
            if start_valid and end_valid and block["start_line"] > block["end_line"]:
                errors.append(f"{slice_id}.packet.source_blocks[{index}]: line range is reversed")
            refs.add(
                f"{block['path']}:{block['start_line']}-{block['end_line']}#sha256:{block['block_sha256']}"
            )
    criterion_fields = {"criterion_id", "text", "source_anchors"}
    if not isinstance(acceptance, list) or len(acceptance) != 8:
        errors.append(f"{slice_id}.packet.acceptance: must contain all eight reviewed criteria")
        return
    ids: list[str] = []
    for index, criterion in enumerate(acceptance):
        if not _keys(criterion, criterion_fields, f"{slice_id}.packet.acceptance[{index}]", errors):
            continue
        criterion = cast(dict[str, Any], criterion)
        if not isinstance(criterion["criterion_id"], str) or not criterion["criterion_id"]:
            errors.append(f"{slice_id}.packet.acceptance[{index}].criterion_id: invalid")
        else:
            ids.append(criterion["criterion_id"])
        if not isinstance(criterion["text"], str) or not criterion["text"]:
            errors.append(f"{slice_id}.packet.acceptance[{index}].text: invalid")
        if _strings(
            criterion["source_anchors"],
            f"{slice_id}.packet.acceptance[{index}].source_anchors", errors,
            maximum=v1.MAX_ANCHORS,
        ) and not set(criterion["source_anchors"]) <= refs:
            errors.append(f"{slice_id}.packet.acceptance[{index}].source_anchors: unreviewed anchor")
    if ids != sorted(set(ids)):
        errors.append(f"{slice_id}.packet.acceptance: criteria must be sorted by unique ID")


def _validate_workspace(value: object, slice_id: str, errors: list[str]) -> None:
    fields = {"branch", "worktree", "created_externally", "clean_required"}
    if not _keys(value, fields, f"{slice_id}.packet.workspace", errors):
        return
    value = cast(dict[str, Any], value)
    if value["branch"] != "codex/v2-pr-1-architecture-decision-records":
        errors.append(f"{slice_id}.packet.workspace.branch: unexpected branch")
    if value["worktree"] != "/fast/projects/tracedecay/.worktrees/v2-pr-1-architecture-decision-records":
        errors.append(f"{slice_id}.packet.workspace.worktree: unexpected path")
    if value["created_externally"] is not True or value["clean_required"] is not True:
        errors.append(f"{slice_id}.packet.workspace: must be externally created and clean")


def _validate_claude(value: object, slice_id: str, errors: list[str]) -> None:
    fields = {
        "enabled", "runtime", "mode", "max_steps", "acceptance_criteria",
        "forbidden_authorities", "untrusted_until_gpt_verified",
    }
    if not _keys(value, fields, f"{slice_id}.packet.claude_adversarial_review", errors):
        return
    value = cast(dict[str, Any], value)
    _integer(
        value["max_steps"], f"{slice_id}.packet.claude_adversarial_review.max_steps", errors
    )
    if value["enabled"] is not False or value["runtime"] != "none":
        errors.append(f"{slice_id}.packet.claude_adversarial_review: must be disabled")
    if value["mode"] != "disabled" or value["max_steps"] != 0:
        errors.append(f"{slice_id}.packet.claude_adversarial_review: disabled mode must have zero steps")
    if value["untrusted_until_gpt_verified"] is not True:
        errors.append(f"{slice_id}.packet.claude_adversarial_review: must remain untrusted")
    if value["acceptance_criteria"] != []:
        errors.append(f"{slice_id}.packet.claude.acceptance_criteria: disabled review has no criteria")
    _strings(value["forbidden_authorities"], f"{slice_id}.packet.claude.forbidden_authorities", errors, maximum=8)
    required = {"activation", "authority_review", "integration", "packet_source", "self_approval"}
    if isinstance(value["forbidden_authorities"], list) and set(value["forbidden_authorities"]) != required:
        errors.append(f"{slice_id}.packet.claude.forbidden_authorities: incomplete authority denial")


def _validate_gates(value: object, slice_id: str, errors: list[str]) -> None:
    fields = {"implementation", "independent_review", "remediation", "successor_review", "integration"}
    if not _keys(value, fields, f"{slice_id}.packet.gates", errors):
        return
    value = cast(dict[str, Any], value)
    for field in fields:
        if not isinstance(value[field], str) or not value[field]:
            errors.append(f"{slice_id}.packet.gates.{field}: must be non-empty")
    if len(set(value.values())) != len(fields):
        errors.append(f"{slice_id}.packet.gates: lifecycle gates must remain distinct")


def _validate_blocks(raw: object, nodes: dict[str, dict[str, Any]], graph: dict[str, Any],
                     policy: dict[str, Any], errors: list[str]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    fields = {"slice_id", "stage_id", "reason_code", "authority_revision"}
    if not isinstance(raw, list):
        errors.append("dispatch_blocks: must be an array")
        return result
    for index, block in enumerate(raw):
        label = f"dispatch_blocks[{index}]"
        if not _keys(block, fields, label, errors):
            continue
        block = cast(dict[str, Any], block)
        slice_id = block["slice_id"]
        if not isinstance(slice_id, str) or not slice_id:
            errors.append(f"{label}.slice_id: must be non-empty")
            continue
        if slice_id in result:
            errors.append(f"dispatch_blocks: duplicate block for {slice_id}")
            continue
        if slice_id not in nodes:
            errors.append(f"dispatch_blocks: unknown slice {slice_id}")
        if block["stage_id"] != policy.get("stage_id"):
            errors.append(f"{slice_id}.dispatch_block.stage_id: policy mismatch")
        if block["reason_code"] != BLOCK_REASON:
            errors.append(f"{slice_id}.dispatch_block.reason_code: expected {BLOCK_REASON}")
        _integer(
            block["authority_revision"], f"{slice_id}.dispatch_block.authority_revision",
            errors, minimum=1,
        )
        if block["authority_revision"] != graph.get("graph_revision"):
            errors.append(f"{slice_id}.dispatch_block.authority_revision: graph mismatch")
        expected = dispatch_entry_digest(
            kind="blocked_node", slice_id=slice_id, payload=block, graph=graph, policy=policy
        )
        if nodes.get(slice_id, {}).get("dispatch_digest") != expected:
            errors.append(f"{slice_id}.dispatch_block: sealed block digest mismatch")
        result[slice_id] = block
    ids = [block.get("slice_id") for block in raw if isinstance(block, dict)]
    if all(isinstance(item, str) for item in ids) and ids != sorted(ids):
        errors.append("dispatch_blocks: records must be canonical order")
    return result


def _validate_partition(nodes: dict[str, dict[str, Any]], dispatch: dict[str, dict[str, Any]],
                        blocks: dict[str, dict[str, Any]], errors: list[str]) -> None:
    overlap = set(dispatch) & set(blocks)
    missing = set(nodes) - set(dispatch) - set(blocks)
    extra = (set(dispatch) | set(blocks)) - set(nodes)
    if overlap:
        errors.append(f"dispatch partition: overlapping packet/block IDs {sorted(overlap)!r}")
    if missing:
        errors.append(f"dispatch partition: missing explicit packet/block IDs {sorted(missing)!r}")
    if extra:
        errors.append(f"dispatch partition: unknown packet/block IDs {sorted(extra)!r}")
    if len(dispatch) != 1 or set(dispatch) != {"PR 1"}:
        errors.append("dispatch partition: initial stage requires exactly one PR 1 packet")
    if nodes and len(blocks) != len(nodes) - 1:
        errors.append(f"dispatch partition: expected {len(nodes) - 1} explicit blockers")


def _validate_contract_set(graph: dict[str, Any], policy: dict[str, Any],
                           nodes: dict[str, dict[str, Any]], dispatch: dict[str, dict[str, Any]],
                           blocks: dict[str, dict[str, Any]], errors: list[str]) -> None:
    entries: dict[str, str] = {}
    for slice_id, packet in dispatch.items():
        entries[slice_id] = dispatch_entry_digest(
            kind="authorized_packet", slice_id=slice_id, payload=packet, graph=graph, policy=policy
        )
    for slice_id, block in blocks.items():
        entries[slice_id] = dispatch_entry_digest(
            kind="blocked_node", slice_id=slice_id, payload=block, graph=graph, policy=policy
        )
    if set(entries) == set(nodes):
        expected = dispatch_contract_set_digest(entries)
        if graph.get("dispatch_contract_set_digest") != expected:
            errors.append("canonical_dag.dispatch_contract_set_digest: complete set digest mismatch")


def _validate_transition(raw: object, document: dict[str, Any], graph: dict[str, Any],
                         policy: dict[str, Any], blocks: dict[str, dict[str, Any]],
                         live: le.LiveEvidence | None, errors: list[str]) -> dict[str, Any]:
    fields = {
        "schema", "transition_id", "stage_id", "repository", "source_commit",
        "source_set_digest", "manifest_digest", "checked_manifest_revision",
        "expected_prior_generation", "prior_state_sha256", "prior_graph_revision",
        "prior_graph_digest", "target_graph_revision", "target_graph_digest",
        "enabled_slice_ids", "dispatch_blocks_digest", "dispatch_blocks_count",
        "dispatch_contract_set_digest", "packet_source_path", "packet_source_blob_oid",
        "packet_source_digest", "candidate_state_digest", "authority_review", "validator_version",
        "activation_sequence", "activated_at",
    }
    if not _keys(raw, fields, "authority_transition", errors):
        return {}
    transition = cast(dict[str, Any], raw)
    if transition["schema"] != TRANSITION_SCHEMA:
        errors.append(f"authority_transition.schema: expected {TRANSITION_SCHEMA!r}")
    numeric_fields = {
        "checked_manifest_revision": 1,
        "prior_graph_revision": 0,
        "target_graph_revision": 1,
        "dispatch_blocks_count": 0,
        "activation_sequence": 1,
    }
    numeric_valid = {
        field: _integer(
            transition[field], f"authority_transition.{field}", errors, minimum=minimum
        )
        for field, minimum in numeric_fields.items()
    }
    mappings = {
        "stage_id": policy.get("stage_id"),
        "repository": graph.get("repository"),
        "source_commit": graph.get("source_commit"),
        "source_set_digest": graph.get("source_set_digest"),
        "manifest_digest": policy.get("checked_manifest_digest"),
        "checked_manifest_revision": policy.get("checked_manifest_revision"),
        "target_graph_revision": graph.get("graph_revision"),
        "target_graph_digest": graph.get("graph_digest"),
        "enabled_slice_ids": policy.get("authorized_slice_ids"),
        "dispatch_contract_set_digest": graph.get("dispatch_contract_set_digest"),
        "packet_source_path": policy.get("packet_source_path"),
        "packet_source_blob_oid": policy.get("packet_source_blob_oid"),
        "packet_source_digest": policy.get("packet_source_digest"),
    }
    for field, expected in mappings.items():
        _pin(f"authority_transition.{field}", transition[field], expected, errors)
    if (
        numeric_valid["prior_graph_revision"]
        and numeric_valid["target_graph_revision"]
        and transition["prior_graph_revision"] + 1 != transition["target_graph_revision"]
    ):
        errors.append("authority_transition: target graph revision must equal prior plus one")
    if not isinstance(transition["expected_prior_generation"], str) or not transition["expected_prior_generation"]:
        errors.append("authority_transition.expected_prior_generation: must be non-empty")
    for field in ("prior_state_sha256", "prior_graph_digest", "dispatch_blocks_digest"):
        if not isinstance(transition[field], str) or not v1.SHA256.fullmatch(transition[field]):
            errors.append(f"authority_transition.{field}: must be sha256:<64 lowercase hex>")
    ordered_blocks = [blocks[key] for key in sorted(blocks)]
    if blocks and transition["dispatch_blocks_digest"] != block_set_digest(ordered_blocks):
        errors.append("authority_transition.dispatch_blocks_digest: mismatch")
    if transition["dispatch_blocks_count"] != len(blocks):
        errors.append("authority_transition.dispatch_blocks_count: mismatch")
    if transition["candidate_state_digest"] != candidate_state_digest(document):
        errors.append("authority_transition.candidate_state_digest: mismatch")
    if transition["validator_version"] != VALIDATOR_VERSION:
        errors.append("authority_transition.validator_version: unsupported validator")
    if transition["activation_sequence"] != 1:
        errors.append("authority_transition.activation_sequence: initial staged dispatch must be 1")
    _validate_rfc3339_utc(transition["activated_at"], "authority_transition.activated_at", errors)
    _validate_authority_review(transition["authority_review"], transition, live, errors)
    receipt = graph.get("activation_receipt", {})
    if isinstance(receipt, dict):
        for field in ("transition_id", "stage_id"):
            _pin(f"canonical_dag.activation_receipt.{field}", receipt.get(field), transition[field], errors)
        _pin("canonical_dag.activation_receipt.authorized_count", receipt.get("authorized_count"), 1, errors)
        _pin("canonical_dag.activation_receipt.blocked_count", receipt.get("blocked_count"), len(blocks), errors)
    return transition


def _validate_authority_review(raw: object, transition: dict[str, Any],
                               live: le.LiveEvidence | None, errors: list[str]) -> None:
    fields = {
        "schema", "receipt_id", "candidate_state_digest", "packet_source_blob_oid",
        "packet_source_digest",
        "prior_generation", "prior_state_sha256", "prior_graph_revision", "prior_graph_digest",
        "reviewer", "reviewer_principal", "reviewer_authority", "implementation_authority",
        "independent", "verdict", "reviewed_at", "receipt_digest",
    }
    if not _keys(raw, fields, "authority_transition.authority_review", errors):
        return
    review = cast(dict[str, Any], raw)
    if review["schema"] != REVIEW_SCHEMA:
        errors.append(f"authority_review.schema: expected {REVIEW_SCHEMA!r}")
    _integer(review["prior_graph_revision"], "authority_review.prior_graph_revision", errors)
    for review_field, transition_field in (
        ("candidate_state_digest", "candidate_state_digest"),
        ("packet_source_blob_oid", "packet_source_blob_oid"),
        ("packet_source_digest", "packet_source_digest"),
        ("prior_generation", "expected_prior_generation"),
        ("prior_state_sha256", "prior_state_sha256"),
        ("prior_graph_revision", "prior_graph_revision"),
        ("prior_graph_digest", "prior_graph_digest"),
    ):
        _pin(f"authority_review.{review_field}", review[review_field], transition[transition_field], errors)
    if review["verdict"] != "approved" or review["independent"] is not True:
        errors.append("authority_review: an independent approved verdict is required")
    for field in ("receipt_id", "reviewer", "reviewer_principal", "reviewer_authority", "implementation_authority", "reviewed_at"):
        if not isinstance(review[field], str) or not review[field]:
            errors.append(f"authority_review.{field}: must be non-empty")
    _validate_rfc3339_utc(review["reviewed_at"], "authority_review.reviewed_at", errors)
    if review["reviewer_principal"] == review["implementation_authority"]:
        errors.append("authority_review: reviewer principal cannot be implementation authority")
    if review["reviewer_authority"] == review["implementation_authority"]:
        errors.append("authority_review: reviewer authority must be distinct")
    if review["receipt_digest"] != authority_review_digest(review):
        errors.append("authority_review.receipt_digest: digest mismatch")
    if live is None or review["receipt_digest"] not in live.authority_review_receipts:
        errors.append("authority_review.receipt_digest: absent from trusted authority-review observations")


def execution_order(nodes: dict[str, dict[str, Any]]) -> list[str]:
    return v1.execution_order(nodes)


def next_ready(result: ValidationResult) -> dict[str, Any]:
    graph = result.graph
    transition = result.transition
    base = {
        "schema": VIEW_SCHEMA,
        "valid": result.valid,
        "activation_mode": "staged_dispatch" if result.valid else "invalid",
        "repository": graph.get("repository"),
        "source_commit": graph.get("source_commit"),
        "source_set_digest": graph.get("source_set_digest"),
        "graph_revision": graph.get("graph_revision"),
        "graph_digest": graph.get("graph_digest"),
        "dispatch_policy_digest": graph.get("dispatch_policy_digest"),
        "packet_source_blob_oid": graph.get("packet_source_blob_oid"),
        "packet_source_digest": graph.get("packet_source_digest"),
        "dispatch_contract_set_digest": graph.get("dispatch_contract_set_digest"),
        "prior_generation": transition.get("expected_prior_generation"),
        "prior_state_sha256": transition.get("prior_state_sha256"),
        "prior_graph_revision": transition.get("prior_graph_revision"),
        "prior_graph_digest": transition.get("prior_graph_digest"),
        "errors": list(result.errors),
        "next_ready": [],
        "blocked": [],
        "execution_order": execution_order(result.nodes) if result.valid else [],
    }
    if not result.valid:
        return base
    complete = {
        node_id: not v1.completion_reasons(result.entries.get(node_id))
        for node_id in result.nodes
    }
    for node_id in base["execution_order"]:
        if complete[node_id]:
            continue
        if node_id in result.blocks:
            base["blocked"].append({"slice_id": node_id, "reasons": [BLOCK_REASON]})
            continue
        own = result.entries.get(node_id)
        own_reasons = v1.completion_reasons(own) if own is not None else []
        prerequisite_reasons: list[str] = []
        for parent in sorted(result.nodes[node_id]["dependencies"]):
            if not complete[parent]:
                for reason in v1.completion_reasons(result.entries.get(parent)):
                    prerequisite_reasons.append(f"prerequisite:{parent}:{reason}")
        reasons = sorted(set(own_reasons + prerequisite_reasons))
        if reasons:
            base["blocked"].append({"slice_id": node_id, "reasons": reasons})
            continue
        packet = copy.deepcopy(result.dispatch[node_id])
        packet.update({
            "source_commit": graph["source_commit"],
            "source_set_digest": graph["source_set_digest"],
            "graph_revision": graph["graph_revision"],
            "graph_digest": graph["graph_digest"],
            "dispatch_policy_digest": graph["dispatch_policy_digest"],
            "packet_source_blob_oid": graph["packet_source_blob_oid"],
            "packet_source_digest": graph["packet_source_digest"],
            "dispatch_contract_set_digest": graph["dispatch_contract_set_digest"],
        })
        base["next_ready"].append(packet)
    return base


def markdown(view: dict[str, Any]) -> str:
    def scalar(value: object) -> str:
        return f"<code>{html.escape(json.dumps(value, ensure_ascii=False), quote=False)}</code>"

    def values(items: list[object]) -> str:
        return ", ".join(scalar(item) for item in items) or "none"

    lines = [
        "# TraceDecay V2 staged next-ready",
        "",
        f"- Valid: {'yes' if view['valid'] else 'no'}",
        f"- Activation mode: {scalar(view.get('activation_mode'))}",
        f"- Schema: {scalar(view.get('schema'))}",
        f"- Repository: {scalar(view.get('repository'))}",
        f"- Source commit: {scalar(view.get('source_commit'))}",
        f"- Source-set digest: {scalar(view.get('source_set_digest'))}",
        f"- Graph revision: {scalar(view.get('graph_revision'))}",
        f"- Graph digest: {scalar(view.get('graph_digest'))}",
        f"- Dispatch-policy digest: {scalar(view.get('dispatch_policy_digest'))}",
        f"- Packet-source blob OID: {scalar(view.get('packet_source_blob_oid'))}",
        f"- Packet-source digest: {scalar(view.get('packet_source_digest'))}",
        f"- Contract-set digest: {scalar(view.get('dispatch_contract_set_digest'))}",
        f"- Prior generation: {scalar(view.get('prior_generation'))}",
        f"- Prior state SHA-256: {scalar(view.get('prior_state_sha256'))}",
        f"- Execution-order nodes: {scalar(len(view.get('execution_order', [])))}",
    ]
    if view["errors"]:
        lines.extend(["", "## Errors", *[f"- {scalar(item)}" for item in view["errors"]]])
    lines.extend(["", "## Next ready"])
    if not view["next_ready"]:
        lines.append("- None.")
    for packet in view["next_ready"]:
        lines.extend([
            f"### {packet['slice_id']}",
            f"- Owner: {scalar(packet['owner'])}",
            f"- Content digest: {scalar(packet['content_digest'])}",
            f"- Commit subject: {scalar(packet['commit_subject'])}",
            f"- Prerequisites: {values(packet['prerequisites'])}",
            f"- Workspace: {scalar(packet['workspace'])}",
            "- Exact files: " + values(packet["exact_files"]),
            "- Acceptance: " + values(packet["acceptance"]),
            "- Source blocks: " + values(packet["source_blocks"]),
            "- Acceptance commands: " + values(packet["acceptance_commands"]),
            "- Required tests: " + values(packet["required_tests"]),
            "- Prohibitions: " + values(packet["prohibitions"]),
            f"- Lane: {scalar(packet['lane'])}",
            f"- Claude adversarial review: {scalar(packet['claude_adversarial_review'])}",
            f"- Gates: {scalar(packet['gates'])}",
            "- Retrieval anchors: " + values(packet["retrieval_anchors"]),
        ])
    lines.extend(["", "## Blocked"])
    if not view["blocked"]:
        lines.append("- None.")
    else:
        lines.extend(
            f"- {scalar(item['slice_id'])}: {values(item['reasons'])}" for item in view["blocked"]
        )
    return "\n".join(lines) + "\n"
