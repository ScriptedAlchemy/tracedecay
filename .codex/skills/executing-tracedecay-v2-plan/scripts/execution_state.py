#!/usr/bin/env python3
"""Validate V2 execution state and project fail-closed next-ready packets."""

from __future__ import annotations

import hashlib
import html
import json
import re
from dataclasses import dataclass
from typing import Any, cast

import live_evidence as le
import slice_authority as sa

EXPORT_SCHEMA = "tracedecay.v2.execution-state/v1"
DAG_SCHEMA = "tracedecay.v2.canonical-dag/v1"
LEDGER_SCHEMA = "tracedecay.v2.completion-ledger/v1"
VIEW_SCHEMA = "tracedecay.v2.next-ready-view/v1"
VALIDATOR_VERSION = "execution_state/v1"
COMPILER_VERSION = "compile_plan_authority/v1"
SHA256 = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
FM_ID = re.compile(r"^FM-[0-9]{3}$")
MAX_FILES = 32
MAX_COMMANDS = 32
MAX_ANCHORS = 64
MAX_TEXT = 2048
MAX_DIAGNOSTICS = 256


@dataclass(frozen=True)
class ValidationResult:
    errors: tuple[str, ...]
    nodes: dict[str, dict[str, Any]]
    entries: dict[str, dict[str, Any]]
    dispatch: dict[str, dict[str, Any]]
    graph: dict[str, Any]
    activation_mode: str

    @property
    def valid(self) -> bool:
        return not self.errors


def _digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(sa._canonical_json(value).encode("utf-8")).hexdigest()


def receipt_digest(value: dict[str, Any], field: str = "receipt_digest") -> str:
    """Digest the exact canonical receipt payload, excluding only its digest field."""
    return _digest({key: item for key, item in value.items() if key != field})


def candidate_digest(value: dict[str, Any]) -> str:
    return receipt_digest(value, "digest")


def graph_digest(graph: dict[str, Any]) -> str:
    """Digest canonical DAG authority, excluding its digest and activation receipt."""
    return _digest({
        "schema": graph.get("schema"),
        "repository": graph.get("repository"),
        "source_commit": graph.get("source_commit"),
        "source_set_digest": graph.get("source_set_digest"),
        "graph_revision": graph.get("graph_revision"),
        "nodes": graph.get("nodes"),
    })


def dispatch_digest(value: dict[str, Any]) -> str:
    """Seal one complete bounded dispatch/test/workspace contract."""
    return _digest(value)


def _keys(value: object, expected: set[str], label: str, errors: list[str]) -> bool:
    if not isinstance(value, dict):
        errors.append(f"{label}: must be an object")
        return False
    actual = set(value)
    if actual != expected:
        errors.append(
            f"{label}: fields must be exactly {sorted(expected)!r}; got {sorted(actual)!r}"
        )
        return False
    return True


def _strings(value: object, label: str, errors: list[str], *, maximum: int | None = None) -> bool:
    if not isinstance(value, list) or not all(isinstance(item, str) and item for item in value):
        errors.append(f"{label}: must be an array of non-empty strings")
        return False
    if len(value) != len(set(value)):
        errors.append(f"{label}: values must be unique")
        return False
    if maximum is not None and len(value) > maximum:
        errors.append(f"{label}: exceeds bound {maximum}")
        return False
    if any(len(item) > MAX_TEXT for item in value):
        errors.append(f"{label}: item exceeds {MAX_TEXT} characters")
        return False
    return True


def _bounded_scalars(value: object, label: str, errors: list[str]) -> None:
    """Reject every oversized string reachable from a worker packet contract."""
    if isinstance(value, str):
        if len(value) > MAX_TEXT:
            errors.append(f"{label}: scalar exceeds {MAX_TEXT} characters")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _bounded_scalars(item, f"{label}[{index}]", errors)
    elif isinstance(value, dict):
        for key, item in value.items():
            _bounded_scalars(item, f"{label}.{key}", errors)


def _pin_equal(label: str, actual: object, expected: object, errors: list[str]) -> None:
    if actual != expected:
        errors.append(f"{label}: stale pin {actual!r}; expected {expected!r}")


def _finalize_errors(errors: list[str]) -> tuple[str, ...]:
    """Bound diagnostics in the shared view while retaining deterministic handles."""
    bounded: list[str] = []
    for error in errors:
        if len(error) <= MAX_TEXT:
            bounded.append(error)
            continue
        handle = hashlib.sha256(error.encode("utf-8")).hexdigest()
        suffix = f" [truncated sha256:{handle}]"
        bounded.append(error[:MAX_TEXT - len(suffix)] + suffix)
    canonical = sorted(set(bounded))
    if len(canonical) <= MAX_DIAGNOSTICS:
        return tuple(canonical)
    omitted = canonical[MAX_DIAGNOSTICS - 1:]
    handle = hashlib.sha256("\n".join(omitted).encode("utf-8")).hexdigest()
    summary = f"diagnostics: omitted {len(omitted)} entries; sha256:{handle}"
    return tuple([*canonical[:MAX_DIAGNOSTICS - 1], summary])


def validate(document: dict[str, Any], live: le.LiveEvidence | None = None) -> ValidationResult:
    errors: list[str] = []
    nodes: dict[str, dict[str, Any]] = {}
    entries: dict[str, dict[str, Any]] = {}
    dispatch: dict[str, dict[str, Any]] = {}
    graph: dict[str, Any] = {}
    activation_mode = document.get("activation_mode", "dispatch")

    expected_export_fields = {
        "schema", "canonical_dag", "completion_ledger", "dispatch_specs",
        "retired_obligations",
    }
    if frozenset(document) not in {frozenset(expected_export_fields),
                                   frozenset(expected_export_fields | {"activation_mode"})}:
        errors.append(
            f"export: fields must be {sorted(expected_export_fields)!r} with optional "
            f"'activation_mode'; got {sorted(document)!r}"
        )
        return ValidationResult(
            _finalize_errors(errors), nodes, entries, dispatch, graph, "invalid"
        )
    if document.get("schema") != EXPORT_SCHEMA:
        errors.append(f"export.schema: expected {EXPORT_SCHEMA!r}")
    if activation_mode not in {"dispatch", "verify_only"}:
        errors.append("export.activation_mode: expected 'dispatch' or 'verify_only'")
    if live is None:
        errors.append("live: authoritative checkout evidence is required")
    else:
        errors.extend(live.errors)

    retired = document.get("retired_obligations")
    retired_list = cast(list[str], retired) if isinstance(retired, list) else []
    if _strings(retired, "retired_obligations", errors):
        if retired_list != sorted(retired_list):
            errors.append("retired_obligations: values must be in canonical order")
        if any(not FM_ID.fullmatch(item) for item in retired_list):
            errors.append("retired_obligations: every value must be FM-###")
        if "FM-168" not in retired_list:
            errors.append("retired_obligations: corrected tombstone FM-168 must remain retired")
    retired_set = set(retired_list)

    raw_graph = document.get("canonical_dag")
    graph_fields = {
        "schema", "repository", "source_commit", "source_set_digest", "graph_revision",
        "graph_digest", "activation_receipt", "nodes",
    }
    if not _keys(raw_graph, graph_fields, "canonical_dag", errors):
        graph = {}
    else:
        graph = cast(dict[str, Any], raw_graph)
        if graph["schema"] != DAG_SCHEMA:
            errors.append(f"canonical_dag.schema: expected {DAG_SCHEMA!r}")
        if not isinstance(graph["repository"], str) or not graph["repository"]:
            errors.append("canonical_dag.repository: must be a non-empty stable identity")
        if not isinstance(graph["source_commit"], str) or not COMMIT.fullmatch(graph["source_commit"]):
            errors.append("canonical_dag.source_commit: must be a full lowercase commit SHA")
        if not isinstance(graph["source_set_digest"], str) or not SHA256.fullmatch(graph["source_set_digest"]):
            errors.append("canonical_dag.source_set_digest: must be sha256:<64 lowercase hex>")
        if live is not None:
            _pin_equal("canonical_dag.repository", graph["repository"], live.repository, errors)
            _pin_equal("canonical_dag.source_commit", graph["source_commit"], live.canonical_commit, errors)
            _pin_equal(
                "canonical_dag.source_set_digest", graph["source_set_digest"],
                live.source_set_digest, errors,
            )
        if isinstance(graph["graph_revision"], bool) or not isinstance(graph["graph_revision"], int) or graph["graph_revision"] < 1:
            errors.append("canonical_dag.graph_revision: must be a positive integer")
        try:
            calculated = graph_digest(graph)
        except (TypeError, ValueError, OverflowError):
            errors.append("canonical_dag.graph_digest: DAG is not finite canonical I-JSON")
        else:
            if graph["graph_digest"] != calculated:
                errors.append("canonical_dag.graph_digest: digest does not match canonical DAG bytes")
        _validate_activation(graph, errors)
        nodes = _validate_nodes(graph.get("nodes"), retired_set, errors)

    if activation_mode == "verify_only":
        if document.get("dispatch_specs") != []:
            errors.append("dispatch_specs: verify_only activation requires an empty array")
    else:
        dispatch = _validate_dispatch(document.get("dispatch_specs"), nodes, errors)
    ledger = document.get("completion_ledger")
    entries = _validate_ledger(ledger, graph, nodes, dispatch, retired_set, live, errors)
    if activation_mode == "verify_only" and entries:
        errors.append("completion_ledger.entries: verify_only activation requires no entries")

    return ValidationResult(
        _finalize_errors(errors), nodes, entries, dispatch, graph, activation_mode
    )


def _validate_activation(graph: dict[str, Any], errors: list[str]) -> None:
    receipt = graph.get("activation_receipt")
    fields = {
        "receipt_id", "repository", "source_commit", "source_set_digest",
        "graph_revision", "graph_digest", "manifest_digest",
        "candidate_graph_revision", "activated_graph_revision", "slice_count",
        "edge_count", "series_count", "compiler_version", "validator_version",
        "activated",
    }
    if not _keys(receipt, fields, "canonical_dag.activation_receipt", errors):
        return
    receipt = cast(dict[str, Any], receipt)
    if not isinstance(receipt["receipt_id"], str) or not receipt["receipt_id"]:
        errors.append("canonical_dag.activation_receipt.receipt_id: must be non-empty")
    for field in ["repository", "source_commit", "source_set_digest", "graph_revision", "graph_digest"]:
        _pin_equal(f"canonical_dag.activation_receipt.{field}", receipt[field], graph[field], errors)
    if not isinstance(receipt["manifest_digest"], str) or not SHA256.fullmatch(receipt["manifest_digest"]):
        errors.append("canonical_dag.activation_receipt.manifest_digest: must be sha256:<64 lowercase hex>")
    for field in ("candidate_graph_revision", "activated_graph_revision"):
        _pin_equal(f"canonical_dag.activation_receipt.{field}", receipt[field], graph["graph_revision"], errors)
    expected_counts = {
        "slice_count": len(graph.get("nodes", [])) if isinstance(graph.get("nodes"), list) else -1,
        "edge_count": sum(
            len(node.get("dependencies", []))
            for node in graph.get("nodes", [])
            if isinstance(node, dict) and isinstance(node.get("dependencies"), list)
        ),
    }
    for field, expected in expected_counts.items():
        _pin_equal(f"canonical_dag.activation_receipt.{field}", receipt[field], expected, errors)
    if (
        isinstance(receipt["series_count"], bool)
        or not isinstance(receipt["series_count"], int)
        or receipt["series_count"] < 0
    ):
        errors.append("canonical_dag.activation_receipt.series_count: must be a non-negative integer")
    if receipt["compiler_version"] != COMPILER_VERSION:
        errors.append(
            "canonical_dag.activation_receipt.compiler_version: "
            f"expected {COMPILER_VERSION!r}"
        )
    if receipt["validator_version"] != VALIDATOR_VERSION:
        errors.append(
            "canonical_dag.activation_receipt.validator_version: "
            f"expected {VALIDATOR_VERSION!r}"
        )
    if receipt["activated"] is not True:
        errors.append("canonical_dag.activation_receipt.activated: must be true")


def _validate_nodes(raw: object, retired: set[str], errors: list[str]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    if not isinstance(raw, list):
        errors.append("canonical_dag.nodes: must be an array")
        return result
    fields = {"id", "owner", "content_digest", "dispatch_digest", "dependencies"}
    owners: dict[str, str] = {}
    for index, node in enumerate(raw):
        label = f"canonical_dag.nodes[{index}]"
        if not _keys(node, fields, label, errors):
            continue
        node = cast(dict[str, Any], node)
        node_id = node["id"]
        if not isinstance(node_id, str) or not node_id:
            errors.append(f"{label}.id: must be non-empty")
            continue
        if node_id in result:
            prior_owner = result[node_id].get("owner")
            if node.get("owner") != prior_owner:
                errors.append(
                    f"canonical_dag.nodes: ambiguous duplicate owner records for {node_id}"
                )
            else:
                errors.append(f"canonical_dag.nodes: ambiguous duplicate node {node_id}")
            continue
        classification = sa.classify_token(node_id)
        if classification.kind != "declaration" or classification.ids != (node_id,):
            errors.append(f"{label}.id: must be one canonical scalar slice ID")
        if node_id in retired:
            errors.append(f"{node_id}: retired obligation cannot be a DAG node")
        owner = node["owner"]
        if not isinstance(owner, str) or not owner:
            errors.append(f"{node_id}.owner: must be non-empty")
        elif owner in owners and owners[owner] != node_id:
            errors.append(
                f"canonical_dag.nodes: duplicate owner {owner!r} reused by "
                f"{owners[owner]} and {node_id}"
            )
        else:
            owners[owner] = node_id
        if not isinstance(node["content_digest"], str) or not SHA256.fullmatch(node["content_digest"]):
            errors.append(f"{node_id}.content_digest: must be sha256:<64 lowercase hex>")
        if not isinstance(node["dispatch_digest"], str) or not SHA256.fullmatch(node["dispatch_digest"]):
            errors.append(f"{node_id}.dispatch_digest: must be sha256:<64 lowercase hex>")
        if _strings(node["dependencies"], f"{node_id}.dependencies", errors):
            if node["dependencies"] != sorted(node["dependencies"]):
                errors.append(f"{node_id}.dependencies: values must be in canonical order")
        result[node_id] = node
        _bounded_scalars(node, label, errors)

    declared_ids = [node.get("id") for node in raw if isinstance(node, dict)]
    if all(isinstance(node_id, str) for node_id in declared_ids):
        canonical_ids = cast(list[str], declared_ids)
        if canonical_ids != sorted(canonical_ids):
            errors.append("canonical_dag.nodes: nodes must be in canonical ID order")

    for node_id, node in sorted(result.items()):
        for parent in node.get("dependencies", []):
            if parent == node_id:
                errors.append(f"{node_id}: self dependency")
            elif parent in retired:
                errors.append(f"{node_id}: dependency references retired obligation {parent}")
            elif parent not in result:
                errors.append(f"{node_id}: unresolved prerequisite {parent}")
    _detect_cycles(result, errors)
    return result


def _detect_cycles(nodes: dict[str, dict[str, Any]], errors: list[str]) -> None:
    visiting: set[str] = set()
    visited: set[str] = set()
    witnesses: set[tuple[str, ...]] = set()

    def visit(node_id: str, trail: list[str]) -> None:
        if node_id in visiting:
            start = trail.index(node_id)
            cycle = tuple(trail[start:] + [node_id])
            if cycle not in witnesses:
                witnesses.add(cycle)
                errors.append("canonical_dag: dependency cycle " + " -> ".join(cycle))
            return
        if node_id in visited or node_id not in nodes:
            return
        visiting.add(node_id)
        for parent in sorted(nodes[node_id].get("dependencies", [])):
            visit(parent, trail + [node_id])
        visiting.remove(node_id)
        visited.add(node_id)

    for node_id in sorted(nodes):
        visit(node_id, [])


def _validate_dispatch(raw: object, nodes: dict[str, dict[str, Any]], errors: list[str]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    if not isinstance(raw, list):
        errors.append("dispatch_specs: must be an array")
        return result
    fields = {
        "slice_id", "owner", "exact_files", "acceptance_commands", "workspace",
        "required_tests", "lane", "optional_claude_review", "gates", "retrieval_anchors",
    }
    for index, spec in enumerate(raw):
        label = f"dispatch_specs[{index}]"
        if not _keys(spec, fields, label, errors):
            continue
        spec = cast(dict[str, Any], spec)
        slice_id = spec["slice_id"]
        if not isinstance(slice_id, str) or not slice_id:
            errors.append(f"{label}.slice_id: must be non-empty")
            continue
        if slice_id in result:
            errors.append(f"dispatch_specs: ambiguous duplicate packet for {slice_id}")
            continue
        if slice_id not in nodes:
            errors.append(f"dispatch_specs: unknown slice {slice_id}")
        if spec["owner"] != nodes.get(slice_id, {}).get("owner"):
            errors.append(f"{slice_id}.dispatch.owner: does not match canonical owner")
        if nodes.get(slice_id, {}).get("dispatch_digest") != dispatch_digest(spec):
            errors.append(
                f"{slice_id}.dispatch: complete packet/test/workspace contract does not match "
                "canonical DAG dispatch_digest"
            )
        _strings(spec["exact_files"], f"{slice_id}.exact_files", errors, maximum=MAX_FILES)
        _strings(spec["acceptance_commands"], f"{slice_id}.acceptance_commands", errors,
                 maximum=MAX_COMMANDS)
        _strings(spec["required_tests"], f"{slice_id}.required_tests", errors,
                 maximum=MAX_COMMANDS)
        _strings(spec["retrieval_anchors"], f"{slice_id}.retrieval_anchors", errors,
                 maximum=MAX_ANCHORS)
        if isinstance(spec["exact_files"], list) and not spec["exact_files"]:
            errors.append(f"{slice_id}.exact_files: bounded packet requires at least one file")
        if isinstance(spec["acceptance_commands"], list) and not spec["acceptance_commands"]:
            errors.append(f"{slice_id}.acceptance_commands: requires at least one command")
        if isinstance(spec["required_tests"], list) and not spec["required_tests"]:
            errors.append(f"{slice_id}.required_tests: requires at least one named test")
        if isinstance(spec["retrieval_anchors"], list) and not spec["retrieval_anchors"]:
            errors.append(f"{slice_id}.retrieval_anchors: requires at least one anchor")
        _validate_workspace(spec["workspace"], slice_id, errors)
        _validate_lane(spec["lane"], slice_id, errors)
        _validate_claude(spec["optional_claude_review"], slice_id, errors)
        _validate_gates(spec["gates"], slice_id, errors)
        _bounded_scalars(spec, label, errors)
        result[slice_id] = spec
    declared_ids = [spec.get("slice_id") for spec in raw if isinstance(spec, dict)]
    if all(isinstance(slice_id, str) for slice_id in declared_ids):
        canonical_ids = cast(list[str], declared_ids)
        if canonical_ids != sorted(canonical_ids):
            errors.append("dispatch_specs: packets must be in canonical slice order")
    for node_id in sorted(set(nodes) - set(result)):
        errors.append(f"dispatch_specs: missing bounded worker packet for {node_id}")
    return result


def _validate_workspace(value: object, slice_id: str, errors: list[str]) -> None:
    if not _keys(value, {"branch", "worktree"}, f"{slice_id}.workspace", errors):
        return
    value = cast(dict[str, Any], value)
    if not all(isinstance(value[field], str) and value[field] for field in ["branch", "worktree"]):
        errors.append(f"{slice_id}.workspace: branch and worktree must be non-empty")


def _validate_lane(value: object, slice_id: str, errors: list[str]) -> None:
    if not _keys(value, {"reasoning_owner", "lifecycle_owner", "acting_runtime"},
                 f"{slice_id}.lane", errors):
        return
    value = cast(dict[str, Any], value)
    if value["reasoning_owner"] != "gpt-5.6-sol" or value["lifecycle_owner"] != "gpt-5.6-sol":
        errors.append(f"{slice_id}.lane: multi-step reasoning and lifecycle must be GPT-5.6-Sol-owned")
    if not isinstance(value["acting_runtime"], str) or not value["acting_runtime"]:
        errors.append(f"{slice_id}.lane.acting_runtime: must be non-empty")


def _validate_claude(value: object, slice_id: str, errors: list[str]) -> None:
    fields = {"enabled", "mode", "max_steps", "acceptance_criteria", "untrusted_until_gpt_verified"}
    if not _keys(value, fields, f"{slice_id}.optional_claude_review", errors):
        return
    value = cast(dict[str, Any], value)
    if not isinstance(value["enabled"], bool):
        errors.append(f"{slice_id}.optional_claude_review.enabled: must be boolean")
    if value["mode"] != "read_only" or value["max_steps"] != 1:
        errors.append(f"{slice_id}.optional_claude_review: must be one read-only step")
    if value["untrusted_until_gpt_verified"] is not True:
        errors.append(f"{slice_id}.optional_claude_review: result must remain untrusted until GPT verification")
    _strings(value["acceptance_criteria"], f"{slice_id}.optional_claude_review.acceptance_criteria",
             errors, maximum=8)
    if value["enabled"] and isinstance(value["acceptance_criteria"], list) and not value["acceptance_criteria"]:
        errors.append(f"{slice_id}.optional_claude_review: enabled review requires acceptance criteria")


def _validate_gates(value: object, slice_id: str, errors: list[str]) -> None:
    fields = {"independent_review", "remediation", "successor_review", "integration"}
    if not _keys(value, fields, f"{slice_id}.gates", errors):
        return
    value = cast(dict[str, Any], value)
    for field in sorted(fields):
        if not isinstance(value[field], str) or not value[field]:
            errors.append(f"{slice_id}.gates.{field}: must be a non-empty gate description")


def _validate_ledger(raw: object, graph: dict[str, Any], nodes: dict[str, dict[str, Any]],
                     dispatch: dict[str, dict[str, Any]], retired: set[str],
                     live: le.LiveEvidence | None,
                     errors: list[str]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    fields = {
        "schema", "repository", "source_commit", "source_set_digest",
        "graph_revision", "graph_digest", "entries",
    }
    if not _keys(raw, fields, "completion_ledger", errors):
        return result
    raw = cast(dict[str, Any], raw)
    if raw["schema"] != LEDGER_SCHEMA:
        errors.append(f"completion_ledger.schema: expected {LEDGER_SCHEMA!r}")
    for field in ["repository", "source_commit", "source_set_digest", "graph_revision", "graph_digest"]:
        _pin_equal(f"completion_ledger.{field}", raw[field], graph.get(field), errors)
    if not isinstance(raw["entries"], list):
        errors.append("completion_ledger.entries: must be an array")
        return result
    entry_fields = {
        "slice_id", "candidate", "task_lineage", "review", "required_tests",
        "test_receipts", "integration", "source_commit", "source_set_digest",
        "graph_revision", "graph_digest", "attempt", "steering_directives",
        "steering_receipts",
    }
    for index, entry in enumerate(raw["entries"]):
        label = f"completion_ledger.entries[{index}]"
        if not _keys(entry, entry_fields, label, errors):
            continue
        entry = cast(dict[str, Any], entry)
        slice_id = entry["slice_id"]
        if not isinstance(slice_id, str) or not slice_id:
            errors.append(f"{label}.slice_id: must be non-empty")
            continue
        if slice_id in result:
            errors.append(f"completion_ledger.entries: ambiguous duplicate entry for {slice_id}")
            continue
        if slice_id not in nodes:
            errors.append(f"completion_ledger.entries: unknown slice {slice_id}")
        if slice_id in retired:
            errors.append(f"completion_ledger.entries: retired obligation {slice_id} cannot complete")
        for field in ["source_commit", "source_set_digest", "graph_revision", "graph_digest"]:
            _pin_equal(f"{slice_id}.{field}", entry[field], graph.get(field), errors)
        integrated = (
            isinstance(entry["integration"], dict)
            and entry["integration"].get("state") == "integrated"
        )
        candidate = _validate_candidate(
            entry["candidate"], slice_id, dispatch.get(slice_id), live, integrated, errors
        )
        _validate_lineage(entry["task_lineage"], slice_id, errors)
        _strings(entry["required_tests"], f"{slice_id}.required_tests", errors)
        _pin_equal(
            f"{slice_id}.required_tests", entry["required_tests"],
            dispatch.get(slice_id, {}).get("required_tests"), errors,
        )
        tests = _validate_tests(
            entry["test_receipts"], entry["required_tests"], slice_id, candidate,
            dispatch.get(slice_id), live, errors
        )

        review = _validate_review(
            entry["review"], slice_id, candidate, entry["task_lineage"], live, errors
        )
        integration = _validate_integration(
            entry["integration"], slice_id, candidate, entry["task_lineage"], graph,
            entry["attempt"], live, errors
        )
        steering = _validate_steering(entry, slice_id, errors)
        validated_entry = dict(entry)
        validated_entry["_validated_tests"] = tests
        validated_entry["_validated_review"] = review
        validated_entry["_validated_integration"] = integration
        validated_entry["_steering_reasons"] = steering
        validated_entry["_acceptance_commands"] = (
            dispatch.get(slice_id, {}).get("acceptance_commands", [])
        )
        result[slice_id] = validated_entry
    declared_ids = [entry.get("slice_id") for entry in raw["entries"] if isinstance(entry, dict)]
    if all(isinstance(slice_id, str) for slice_id in declared_ids):
        canonical_ids = cast(list[str], declared_ids)
        if canonical_ids != sorted(canonical_ids):
            errors.append("completion_ledger.entries: entries must be in canonical slice order")
    return result


def _validate_candidate(value: object, slice_id: str, spec: dict[str, Any] | None,
                        live: le.LiveEvidence | None, integrated: bool,
                        errors: list[str]) -> dict[str, Any]:
    fields = {"commit", "digest", "branch", "worktree", "workspace_observation"}
    if not _keys(value, fields, f"{slice_id}.candidate", errors):
        return {}
    value = cast(dict[str, Any], value)
    if not isinstance(value["commit"], str) or not COMMIT.fullmatch(value["commit"]):
        errors.append(f"{slice_id}.candidate.commit: must be a full lowercase commit SHA")
    if value["digest"] != candidate_digest(value):
        errors.append(
            f"{slice_id}.candidate.digest: digest does not match canonical candidate payload bytes"
        )
    workspace = spec.get("workspace", {}) if isinstance(spec, dict) else {}
    _pin_equal(f"{slice_id}.candidate.branch", value["branch"], workspace.get("branch"), errors)
    _pin_equal(f"{slice_id}.candidate.worktree", value["worktree"], workspace.get("worktree"), errors)
    stored = value["workspace_observation"]
    workspace_fields = {
        "repository", "candidate_commit", "branch_ref", "worktree", "method",
        "status_method", "clean", "observation_digest",
    }
    if not _keys(stored, workspace_fields, f"{slice_id}.candidate.workspace_observation", errors):
        stored = {}
    elif stored["observation_digest"] != receipt_digest(stored, "observation_digest"):
        errors.append(
            f"{slice_id}.candidate.workspace_observation: digest does not match observation bytes"
        )
    branch_ref = (
        value["branch"] if str(value["branch"]).startswith("refs/")
        else f"refs/heads/{value['branch']}"
    )
    expected_observation = None
    if live is not None:
        expected_observation = live.workspaces.get(
            le.workspace_key(str(value["commit"]), branch_ref, str(value["worktree"]))
        )
    if integrated:
        if stored.get("candidate_commit") != value["commit"]:
            errors.append(
                f"{slice_id}.candidate.workspace_observation: candidate commit mismatch"
            )
    elif expected_observation is None:
        errors.append(f"{slice_id}.candidate.workspace_observation: no fresh live Git association")
    elif stored != expected_observation:
        errors.append(
            f"{slice_id}.candidate.workspace_observation: does not match fresh live Git association"
        )
    elif expected_observation.get("clean") is not True:
        errors.append(f"{slice_id}.candidate.workspace_observation: worktree is not clean")
    return value


def _validate_lineage(value: object, slice_id: str, errors: list[str]) -> None:
    fields = {
        "implementation_task", "implementation_actor", "parent_tasks", "review_tasks", "remediation_tasks",
        "successor_review_tasks", "integration_task",
    }
    if not _keys(value, fields, f"{slice_id}.task_lineage", errors):
        return
    value = cast(dict[str, Any], value)
    for field in ["implementation_task", "implementation_actor", "integration_task"]:
        if not isinstance(value[field], str) or not value[field]:
            errors.append(f"{slice_id}.task_lineage.{field}: must be non-empty")
    for field in ["parent_tasks", "review_tasks", "remediation_tasks", "successor_review_tasks"]:
        _strings(value[field], f"{slice_id}.task_lineage.{field}", errors)


def _validate_review(value: object, slice_id: str, candidate: dict[str, Any], lineage: object,
                     live: le.LiveEvidence | None,
                     errors: list[str]) -> dict[str, Any] | None:
    if value is None:
        return None
    fields = {
        "review_task", "reviewer", "reviewer_principal", "reviewer_authority",
        "implementation_authority", "independent", "verdict", "candidate_commit",
        "candidate_digest", "receipt_digest", "anchors",
    }
    if not _keys(value, fields, f"{slice_id}.review", errors):
        return None
    value = cast(dict[str, Any], value)
    if value["verdict"] not in {"approved", "changes_requested", "rejected", "inconclusive"}:
        errors.append(f"{slice_id}.review.verdict: invalid verdict")
    if not isinstance(value["independent"], bool):
        errors.append(f"{slice_id}.review.independent: must be boolean")
    for field in ["review_task", "reviewer", "reviewer_principal", "reviewer_authority",
                  "implementation_authority"]:
        if not isinstance(value[field], str) or not value[field]:
            errors.append(f"{slice_id}.review.{field}: must be non-empty")
    _pin_equal(f"{slice_id}.review.candidate_commit", value["candidate_commit"],
               candidate.get("commit"), errors)
    _pin_equal(f"{slice_id}.review.candidate_digest", value["candidate_digest"],
               candidate.get("digest"), errors)
    if value["receipt_digest"] != receipt_digest(value):
        errors.append(
            f"{slice_id}.review.receipt_digest: digest does not match canonical receipt payload bytes"
        )
    if live is None or value["receipt_digest"] not in live.review_receipts:
        errors.append(f"{slice_id}.review.receipt_digest: absent from trusted review observations")
    _strings(value["anchors"], f"{slice_id}.review.anchors", errors, maximum=MAX_ANCHORS)
    if isinstance(value["anchors"], list) and not value["anchors"]:
        errors.append(f"{slice_id}.review.anchors: independent verdict requires evidence")
    lineage_object = cast(dict[str, Any], lineage) if isinstance(lineage, dict) else {}
    if value["review_task"] not in lineage_object.get("review_tasks", []):
        errors.append(f"{slice_id}.review.review_task: absent from task lineage")
    if value["reviewer"] == lineage_object.get("implementation_actor"):
        errors.append(f"{slice_id}.review.reviewer: self-review is not independent")
    if value["reviewer_principal"] == lineage_object.get("implementation_actor"):
        errors.append(f"{slice_id}.review.reviewer_principal: implementation principal cannot review")
    if value["reviewer_authority"] == value["implementation_authority"]:
        errors.append(
            f"{slice_id}.review.reviewer_authority: must be distinct from implementation authority"
        )
    if value["independent"] is True and (
        value["reviewer_principal"] == lineage_object.get("implementation_actor")
        or value["reviewer_authority"] == value["implementation_authority"]
    ):
        errors.append(
            f"{slice_id}.review.independent: bare assertion lacks distinct principal/authority evidence"
        )
    return value


def _validate_tests(value: object, required_names: object, slice_id: str,
                    candidate: dict[str, Any],
                    spec: dict[str, Any] | None,
                    live: le.LiveEvidence | None,
                    errors: list[str]) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        errors.append(f"{slice_id}.test_receipts: must be an array")
        return []
    if len(value) > MAX_COMMANDS:
        errors.append(f"{slice_id}.test_receipts: exceeds bound {MAX_COMMANDS}")
    legacy_fields = {
        "name", "command", "exit_code", "candidate_commit", "candidate_digest",
        "receipt_digest",
    }
    grouped_fields = (legacy_fields - {"name"}) | {"tests"}
    result: list[dict[str, Any]] = []
    names: set[str] = set()
    commands: set[str] = set()
    for index, receipt in enumerate(value):
        label = f"{slice_id}.test_receipts[{index}]"
        if not isinstance(receipt, dict) or (
            set(receipt) != legacy_fields and set(receipt) != grouped_fields
        ):
            errors.append(
                f"{label}: requires exact legacy name fields or grouped tests fields"
            )
            continue
        receipt = cast(dict[str, Any], receipt)
        receipt_names = [receipt["name"]] if "name" in receipt else receipt["tests"]
        if not isinstance(receipt_names, list):
            errors.append(f"{label}.tests: must be an array")
            receipt_names = []
        elif len(receipt_names) > MAX_COMMANDS:
            errors.append(f"{label}.tests: exceeds bound {MAX_COMMANDS}")
        for name in receipt_names:
            name_label = f"{label}.{'name' if 'name' in receipt else 'tests'}"
            if not isinstance(name, str) or not name:
                errors.append(f"{name_label}: entries must be non-empty strings")
            elif name in names:
                errors.append(f"{slice_id}.test_receipts: ambiguous duplicate receipt {name}")
            elif isinstance(required_names, list) and name not in required_names:
                errors.append(f"{name_label}: {name!r} is not declared in required_tests")
            else:
                names.add(name)
        if not isinstance(receipt["command"], str) or not receipt["command"]:
            errors.append(f"{label}.command: must be non-empty")
        elif receipt["command"] not in (spec or {}).get("acceptance_commands", []):
            errors.append(f"{label}.command: is not an exact declared acceptance command")
        elif receipt["command"] in commands:
            errors.append(f"{slice_id}.test_receipts: ambiguous duplicate command receipt")
        else:
            commands.add(receipt["command"])
        if isinstance(receipt["exit_code"], bool) or not isinstance(receipt["exit_code"], int):
            errors.append(f"{label}.exit_code: must be an integer")
        _pin_equal(f"{label}.candidate_commit", receipt["candidate_commit"],
                   candidate.get("commit"), errors)
        _pin_equal(f"{label}.candidate_digest", receipt["candidate_digest"],
                   candidate.get("digest"), errors)
        if receipt["receipt_digest"] != receipt_digest(receipt):
            errors.append(
                f"{label}.receipt_digest: digest does not match canonical receipt payload bytes"
            )
        if live is None or receipt["receipt_digest"] not in live.test_receipts:
            errors.append(f"{label}.receipt_digest: absent from trusted test observations")
        result.append(receipt)
    return result


def _validate_integration(value: object, slice_id: str, candidate: dict[str, Any], lineage: object,
                          graph: dict[str, Any], attempt: object,
                          live: le.LiveEvidence | None,
                          errors: list[str]) -> dict[str, Any] | None:
    if value is None:
        return None
    fields = {
        "integration_task", "state", "candidate_commit", "canonical_commit",
        "canonical_branch", "source_set_digest", "graph_revision", "graph_digest",
        "attempt_id", "lease_fence_epoch", "steering_watermark", "terminal_cas_sequence",
        "ancestry_observation", "receipt_digest",
    }
    if not _keys(value, fields, f"{slice_id}.integration", errors):
        return None
    value = cast(dict[str, Any], value)
    if value["state"] not in {"pending", "integrated", "rejected", "unknown"}:
        errors.append(f"{slice_id}.integration.state: invalid state")
    _pin_equal(f"{slice_id}.integration.candidate_commit", value["candidate_commit"],
               candidate.get("commit"), errors)
    for field in ["canonical_commit", "source_set_digest", "graph_revision", "graph_digest"]:
        graph_field = "source_commit" if field == "canonical_commit" else field
        _pin_equal(f"{slice_id}.integration.{field}", value[field], graph.get(graph_field), errors)
    if not isinstance(value["canonical_branch"], str) or not value["canonical_branch"]:
        errors.append(f"{slice_id}.integration.canonical_branch: must be non-empty")
    elif live is not None:
        _pin_equal(
            f"{slice_id}.integration.canonical_branch", value["canonical_branch"],
            live.canonical_ref, errors,
        )
    attempt_object = cast(dict[str, Any], attempt) if isinstance(attempt, dict) else {}
    for integration_field, attempt_field in [
        ("attempt_id", "attempt_id"),
        ("lease_fence_epoch", "lease_fence_epoch"),
        ("steering_watermark", "observed_steering_sequence"),
        ("terminal_cas_sequence", "terminal_cas_sequence"),
    ]:
        _pin_equal(
            f"{slice_id}.integration.{integration_field}", value[integration_field],
            attempt_object.get(attempt_field), errors,
        )
    observed = value["ancestry_observation"]
    expected = live.ancestry.get(candidate.get("commit", "")) if live is not None else None
    if expected is None:
        errors.append(f"{slice_id}.integration.ancestry_observation: no sealed live Git observation")
    elif observed != expected:
        errors.append(
            f"{slice_id}.integration.ancestry_observation: does not match sealed live Git observation"
        )
    if value["receipt_digest"] != receipt_digest(value):
        errors.append(
            f"{slice_id}.integration.receipt_digest: digest does not match canonical receipt payload bytes"
        )
    if isinstance(lineage, dict) and value["integration_task"] != lineage.get("integration_task"):
        errors.append(f"{slice_id}.integration.integration_task: absent from task lineage")
    return value


def _validate_steering(entry: dict[str, Any], slice_id: str,
                       errors: list[str]) -> list[str]:
    """Validate required steering through an immutable terminal-CAS fence."""
    attempt = entry["attempt"]
    attempt_fields = {
        "attempt_id", "lease_fence_epoch", "observed_steering_sequence",
        "current_event_sequence", "terminal_cas_sequence", "terminal_cas_committed",
    }
    if not _keys(attempt, attempt_fields, f"{slice_id}.attempt", errors):
        return ["steering_attempt_invalid"]
    attempt = cast(dict[str, Any], attempt)
    for field in ["attempt_id", "lease_fence_epoch"]:
        if not isinstance(attempt[field], str) or not attempt[field]:
            errors.append(f"{slice_id}.attempt.{field}: must be non-empty")
    for field in ["observed_steering_sequence", "current_event_sequence", "terminal_cas_sequence"]:
        if isinstance(attempt[field], bool) or not isinstance(attempt[field], int) or attempt[field] < 0:
            errors.append(f"{slice_id}.attempt.{field}: must be a non-negative integer")
    if attempt["terminal_cas_committed"] is not True:
        errors.append(f"{slice_id}.attempt.terminal_cas_committed: must be true")
    observed = attempt["observed_steering_sequence"]
    terminal = attempt["terminal_cas_sequence"]
    current = attempt["current_event_sequence"]
    if isinstance(observed, int) and isinstance(terminal, int) and observed > terminal:
        errors.append(f"{slice_id}.attempt: observed steering cannot exceed terminal CAS sequence")
    if isinstance(terminal, int) and isinstance(current, int) and terminal > current:
        errors.append(f"{slice_id}.attempt: terminal CAS cannot exceed current event sequence")

    directives = entry["steering_directives"]
    receipts = entry["steering_receipts"]
    directive_fields = {
        "directive_id", "classification", "event_sequence", "delivery_boundary",
        "remediation_task", "successor_review_task",
    }
    receipt_fields = {
        "directive_id", "attempt_id", "lease_fence_epoch", "event_sequence",
        "delivery_boundary", "delivered", "acknowledged", "disposition",
        "actor", "authority", "receipt_digest",
    }
    if not isinstance(directives, list):
        errors.append(f"{slice_id}.steering_directives: must be an array")
        directives = []
    if not isinstance(receipts, list):
        errors.append(f"{slice_id}.steering_receipts: must be an array")
        receipts = []
    directive_map: dict[str, dict[str, Any]] = {}
    sequence_ids: set[int] = set()
    for index, directive in enumerate(directives):
        label = f"{slice_id}.steering_directives[{index}]"
        if not _keys(directive, directive_fields, label, errors):
            continue
        directive = cast(dict[str, Any], directive)
        directive_id = directive["directive_id"]
        sequence = directive["event_sequence"]
        if not isinstance(directive_id, str) or not directive_id or directive_id in directive_map:
            errors.append(f"{label}.directive_id: must be non-empty and unique")
            continue
        if directive["classification"] not in {"required", "advisory"}:
            errors.append(f"{label}.classification: must be required or advisory")
        if isinstance(sequence, bool) or not isinstance(sequence, int) or sequence < 0:
            errors.append(f"{label}.event_sequence: must be a non-negative integer")
        elif sequence in sequence_ids:
            errors.append(f"{slice_id}.steering_directives: duplicate event sequence {sequence}")
        else:
            sequence_ids.add(sequence)
        if not isinstance(directive["delivery_boundary"], str) or not directive["delivery_boundary"]:
            errors.append(f"{label}.delivery_boundary: must be non-empty")
        for field in ["remediation_task", "successor_review_task"]:
            if directive[field] is not None and (
                not isinstance(directive[field], str) or not directive[field]
            ):
                errors.append(f"{label}.{field}: must be null or a non-empty task ID")
        directive_map[directive_id] = directive

    receipt_map: dict[str, dict[str, Any]] = {}
    for index, receipt in enumerate(receipts):
        label = f"{slice_id}.steering_receipts[{index}]"
        if not _keys(receipt, receipt_fields, label, errors):
            continue
        receipt = cast(dict[str, Any], receipt)
        directive_id = receipt["directive_id"]
        if directive_id in receipt_map:
            errors.append(f"{slice_id}.steering_receipts: duplicate delivery for {directive_id}")
            continue
        receipt_map[directive_id] = receipt
        directive = directive_map.get(directive_id)
        if directive is None:
            errors.append(f"{label}.directive_id: unknown steering directive")
            continue
        for field in ["event_sequence", "delivery_boundary"]:
            _pin_equal(f"{label}.{field}", receipt[field], directive[field], errors)
        for field in ["attempt_id", "lease_fence_epoch"]:
            _pin_equal(f"{label}.{field}", receipt[field], attempt[field], errors)
        if receipt["delivered"] is not True or receipt["acknowledged"] is not True:
            errors.append(f"{label}: disposition evidence must be delivered and acknowledged")
        if receipt["disposition"] not in {"applied", "rejected", "superseded"}:
            errors.append(f"{label}.disposition: unresolved disposition")
        for field in ["actor", "authority"]:
            if not isinstance(receipt[field], str) or not receipt[field]:
                errors.append(f"{label}.{field}: must be non-empty")
        if receipt["receipt_digest"] != receipt_digest(receipt):
            errors.append(
                f"{label}.receipt_digest: digest does not match canonical receipt payload bytes"
            )

    if not isinstance(observed, int) or not isinstance(terminal, int):
        return ["steering_attempt_invalid"]
    reasons: list[str] = []
    for directive_id, directive in sorted(directive_map.items()):
        sequence = directive["event_sequence"]
        if directive["classification"] != "required" or not isinstance(sequence, int):
            continue
        if sequence <= observed and directive_id not in receipt_map:
            errors.append(f"{slice_id}.steering: unacknowledged required directive {directive_id}")
        elif observed < sequence <= terminal:
            errors.append(f"{slice_id}.steering: late required directive before terminal CAS {directive_id}")
        elif sequence > terminal:
            reasons.append(f"late_required_steering_remediation:{directive_id}")
            lineage = entry.get("task_lineage", {})
            remediation_task = directive.get("remediation_task")
            successor_task = directive.get("successor_review_task")
            if (
                not isinstance(lineage, dict)
                or not isinstance(remediation_task, str)
                or remediation_task not in lineage.get("remediation_tasks", [])
            ):
                errors.append(
                    f"{slice_id}.steering: post-CAS required directive {directive_id} "
                    "requires an explicitly bound remediation task in lineage"
                )
            if (
                not isinstance(lineage, dict)
                or not isinstance(successor_task, str)
                or successor_task not in lineage.get("successor_review_tasks", [])
            ):
                errors.append(
                    f"{slice_id}.steering: post-CAS required directive {directive_id} "
                    "requires an explicitly bound successor-review task in lineage"
                )
    return sorted(set(reasons))


def completion_reasons(entry: dict[str, Any] | None) -> list[str]:
    """Return deterministic reasons why an entry is not verified integrated completion."""
    if entry is None:
        return ["missing_completion_ledger"]
    reasons: list[str] = []
    review = entry.get("_validated_review")
    tests = entry.get("_validated_tests", [])
    integration = entry.get("_validated_integration")
    reasons.extend(entry.get("_steering_reasons", []))
    if review is None:
        reasons.append("missing_independent_review")
    else:
        if review.get("verdict") != "approved":
            reasons.append(f"review_{review.get('verdict')}")
            lineage = entry.get("task_lineage", {})
            if not lineage.get("remediation_tasks") or not lineage.get("successor_review_tasks"):
                reasons.append("negative_review_without_recovery_lineage")
        if review.get("independent") is not True:
            reasons.append("review_not_independent")
    required = entry.get("required_tests", [])
    receipts = {
        name: receipt
        for receipt in tests
        for name in ([receipt["name"]] if "name" in receipt else receipt.get("tests", []))
    }
    for name in sorted(required):
        receipt = receipts.get(name)
        if receipt is None:
            reasons.append(f"missing_test_receipt:{name}")
        elif receipt.get("exit_code") != 0:
            reasons.append(f"failed_test_receipt:{name}")
    receipts_by_command = {receipt.get("command"): receipt for receipt in tests}
    for command in entry.get("_acceptance_commands", []):
        receipt = receipts_by_command.get(command)
        if receipt is None:
            reasons.append(f"missing_acceptance_command_receipt:{command}")
        elif receipt.get("exit_code") != 0:
            reasons.append(f"failed_acceptance_command_receipt:{command}")
    if integration is None:
        reasons.append("candidate_only_unintegrated")
    else:
        if integration.get("state") != "integrated":
            reasons.append(f"integration_{integration.get('state')}")
        observation = integration.get("ancestry_observation", {})
        if observation.get("status") != "ancestor" or observation.get("command_exit_code") != 0:
            reasons.append("candidate_not_in_canonical_history")
    return sorted(set(reasons))


def execution_order(nodes: dict[str, dict[str, Any]]) -> list[str]:
    """Return deterministic parent-before-child order for an already validated DAG."""
    remaining = {node_id: set(node["dependencies"]) for node_id, node in nodes.items()}
    ordered: list[str] = []
    while remaining:
        ready = sorted(node_id for node_id, parents in remaining.items() if not parents)
        if not ready:
            return []
        ordered.extend(ready)
        for node_id in ready:
            del remaining[node_id]
        for parents in remaining.values():
            parents.difference_update(ready)
    return ordered


def next_ready(result: ValidationResult) -> dict[str, Any]:
    """Produce one deterministic view. Invalid authority always suppresses all packets."""
    graph = result.graph
    base = {
        "schema": VIEW_SCHEMA,
        "valid": result.valid,
        "activation_mode": result.activation_mode,
        "repository": graph.get("repository"),
        "source_commit": graph.get("source_commit"),
        "source_set_digest": graph.get("source_set_digest"),
        "graph_revision": graph.get("graph_revision"),
        "graph_digest": graph.get("graph_digest"),
        "errors": list(result.errors),
        "next_ready": [],
        "blocked": [],
        "execution_order": execution_order(result.nodes) if result.valid else [],
    }
    if not result.valid:
        return base
    if result.activation_mode == "verify_only":
        base["blocked"] = [
            {"slice_id": node_id, "reasons": ["verification_only_not_dispatchable"]}
            for node_id in base["execution_order"]
        ]
        return base

    complete = {
        node_id: not completion_reasons(result.entries.get(node_id))
        for node_id in result.nodes
    }
    for node_id, node in sorted(result.nodes.items()):
        if complete[node_id]:
            continue
        own_entry = result.entries.get(node_id)
        own_reasons = completion_reasons(own_entry) if own_entry is not None else []
        prerequisite_reasons: list[str] = []
        for parent in sorted(node["dependencies"]):
            if not complete[parent]:
                for reason in completion_reasons(result.entries.get(parent)):
                    prerequisite_reasons.append(f"prerequisite:{parent}:{reason}")
        reasons = sorted(set(own_reasons + prerequisite_reasons))
        if reasons:
            base["blocked"].append({"slice_id": node_id, "reasons": reasons})
            continue
        spec = result.dispatch[node_id]
        base["next_ready"].append({
            "slice_id": node_id,
            "owner": spec["owner"],
            "prerequisites": sorted(node["dependencies"]),
            "exact_files": spec["exact_files"],
            "acceptance_commands": spec["acceptance_commands"],
            "required_tests": spec["required_tests"],
            "workspace": spec["workspace"],
            "lane": spec["lane"],
            "optional_claude_review": spec["optional_claude_review"],
            "gates": spec["gates"],
            "retrieval_anchors": spec["retrieval_anchors"],
            "source_commit": graph["source_commit"],
            "source_set_digest": graph["source_set_digest"],
            "graph_revision": graph["graph_revision"],
            "graph_digest": graph["graph_digest"],
        })
    return base


def markdown(view: dict[str, Any]) -> str:
    """Render the same sealed view as bounded Markdown for humans and MCP defaults."""
    def scalar(value: object) -> str:
        encoded = html.escape(json.dumps(value, ensure_ascii=False), quote=False)
        return f"<code>{encoded}</code>"

    def values(items: list[object]) -> str:
        return ", ".join(scalar(item) for item in items) or "none"

    lines = [
        "# TraceDecay V2 next-ready",
        "",
        f"- Valid: {'yes' if view['valid'] else 'no'}",
        f"- Activation mode: {scalar(view.get('activation_mode'))}",
        f"- Schema: {scalar(view['schema'])}",
        f"- Repository: {scalar(view.get('repository'))}",
        f"- Source commit: {scalar(view.get('source_commit'))}",
        f"- Source-set digest: {scalar(view.get('source_set_digest'))}",
        f"- Graph revision: {scalar(view.get('graph_revision'))}",
        f"- Graph digest: {scalar(view.get('graph_digest'))}",
        f"- Execution-order nodes: {scalar(len(view.get('execution_order', [])))}",
    ]
    if view["errors"]:
        lines.extend(["", "## Errors"])
        lines.extend(f"- {scalar(error)}" for error in view["errors"])
    lines.extend(["", "## Next ready"])
    if not view["next_ready"]:
        lines.append("- None.")
    for packet in view["next_ready"]:
        lines.extend([
            f"### {packet['slice_id']}",
            f"- Slice ID: {scalar(packet['slice_id'])}",
            f"- Owner: {scalar(packet['owner'])}",
            f"- Prerequisites: {values(packet['prerequisites'])}",
            f"- Source commit: {scalar(packet['source_commit'])}",
            f"- Source-set digest: {scalar(packet['source_set_digest'])}",
            f"- Graph revision: {scalar(packet['graph_revision'])}",
            f"- Graph digest: {scalar(packet['graph_digest'])}",
            f"- Workspace branch: {scalar(packet['workspace']['branch'])}",
            f"- Workspace worktree: {scalar(packet['workspace']['worktree'])}",
            "- Exact files: " + values(packet["exact_files"]),
            "- Acceptance commands:",
        ])
        lines.extend(f"  - {scalar(command)}" for command in packet["acceptance_commands"])
        lines.append("- Required tests: " + values(packet["required_tests"]))
        lines.extend([
            "- Lane:",
            f"  - Reasoning owner: {scalar(packet['lane']['reasoning_owner'])}",
            f"  - Lifecycle owner: {scalar(packet['lane']['lifecycle_owner'])}",
            f"  - Acting runtime: {scalar(packet['lane']['acting_runtime'])}",
            "- Optional Claude review:",
            f"  - Enabled: {scalar(packet['optional_claude_review']['enabled'])}",
            f"  - Mode: {scalar(packet['optional_claude_review']['mode'])}",
            f"  - Max steps: {scalar(packet['optional_claude_review']['max_steps'])}",
            "  - Acceptance criteria: "
            + values(packet["optional_claude_review"]["acceptance_criteria"]),
            "  - Untrusted until GPT verified: "
            + scalar(packet["optional_claude_review"]["untrusted_until_gpt_verified"]),
            "- Gates:",
        ])
        for gate in ["independent_review", "remediation", "successor_review", "integration"]:
            lines.append(f"  - {gate}: {scalar(packet['gates'][gate])}")
        lines.append("- Retrieval anchors: " + values(packet["retrieval_anchors"]))
    lines.extend(["", "## Blocked"])
    if not view["blocked"]:
        lines.append("- None.")
    for item in view["blocked"]:
        lines.append(f"- {scalar(item['slice_id'])}: " + values(item["reasons"]))
    return "\n".join(lines) + "\n"
