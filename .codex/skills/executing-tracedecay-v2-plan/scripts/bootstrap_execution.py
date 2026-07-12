#!/usr/bin/env python3
"""Validate and atomically install one shared V2 execution-state export."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any

import execution_state
import live_evidence
import plan_execution
import slice_authority


GENERATIONS = Path(".tracedecay/v2-execution-generations")
ACTIVE_POINTER = Path(".tracedecay/v2-execution-active.json")
POINTER_SCHEMA = "tracedecay.v2.execution-generation-pointer/v1"


def _load_manifest(path: Path) -> dict[str, Any]:
    document = plan_execution.strict_json(path)
    if document.get("schema") != "tracedecay.v2.slice-dag/v1":
        raise ValueError("bootstrap manifest has the wrong schema")
    return document


def _cross_validate(manifest: dict[str, Any], state: dict[str, Any],
                    live: live_evidence.LiveEvidence) -> list[str]:
    errors: list[str] = []
    graph = state.get("canonical_dag", {})
    slices = manifest.get("slices", {})
    nodes = graph.get("nodes", []) if isinstance(graph, dict) else []
    node_map = {
        node.get("id"): node for node in nodes
        if isinstance(node, dict) and isinstance(node.get("id"), str)
    }
    if set(slices) != set(node_map):
        errors.append("manifest/state slice IDs differ")
    for slice_id in sorted(set(slices) & set(node_map)):
        body = slices[slice_id]
        node = node_map[slice_id]
        if not isinstance(body, dict):
            errors.append(f"{slice_id}: manifest slice is not an object")
            continue
        if body.get("content_digest") != node.get("content_digest"):
            errors.append(f"{slice_id}: content digest differs")
        dependencies = body.get("dependencies", [])
        parents = sorted(
            edge.get("parent") for edge in dependencies
            if isinstance(edge, dict) and isinstance(edge.get("parent"), str)
        )
        if parents != node.get("dependencies"):
            errors.append(f"{slice_id}: dependencies differ")
    for field in ("graph_revision", "source_set_digest"):
        if manifest.get(field) != graph.get(field):
            errors.append(f"manifest/state {field} differs")
    if graph.get("source_set_digest") != live.source_set_digest:
        errors.append("manifest/state source_set_digest differs from canonical Git tree")
    return sorted(set(errors))


def _json_bytes(document: dict[str, Any]) -> bytes:
    return (json.dumps(document, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _write_staged(output: Path, payload: bytes) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("xb") as file:
        file.write(payload)
        file.flush()
        os.fsync(file.fileno())
    os.chmod(output, 0o600)


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _atomic_install(output: Path, document: dict[str, Any]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    payload = _json_bytes(document)
    with tempfile.NamedTemporaryFile(dir=output.parent, prefix=f".{output.name}.", delete=False) as file:
        temporary = Path(file.name)
        file.write(payload)
        file.flush()
        os.fsync(file.fileno())
    try:
        os.chmod(temporary, 0o600)
        os.replace(temporary, output)
        _fsync_directory(output.parent)
    finally:
        temporary.unlink(missing_ok=True)


def _install_generation(root: Path, manifest: dict[str, Any], state: dict[str, Any]) -> tuple[Path, Path, Path]:
    manifest_bytes = _json_bytes(manifest)
    state_bytes = _json_bytes(state)
    manifest_digest = hashlib.sha256(manifest_bytes).hexdigest()
    state_digest = hashlib.sha256(state_bytes).hexdigest()
    generation = f"r{manifest['graph_revision']}-{manifest_digest[:16]}-{state_digest[:16]}"
    generations = root / GENERATIONS
    generations.mkdir(parents=True, exist_ok=True)
    final = generations / generation
    if not final.exists():
        staging = Path(tempfile.mkdtemp(prefix=f".{generation}.", dir=generations))
        try:
            _write_staged(staging / "manifest.json", manifest_bytes)
            _write_staged(staging / "state.json", state_bytes)
            _fsync_directory(staging)
            os.replace(staging, final)
            _fsync_directory(generations)
        finally:
            if staging.exists():
                shutil.rmtree(staging)
    elif (
        (final / "manifest.json").read_bytes() != manifest_bytes
        or (final / "state.json").read_bytes() != state_bytes
    ):
        raise ValueError(f"existing execution generation {generation} has different bytes")

    pointer = {
        "schema": POINTER_SCHEMA,
        "generation": generation,
        "manifest": f"v2-execution-generations/{generation}/manifest.json",
        "state": f"v2-execution-generations/{generation}/state.json",
        "manifest_sha256": manifest_digest,
        "state_sha256": state_digest,
    }
    active = root / ACTIVE_POINTER
    _atomic_install(active, pointer)
    return final / "manifest.json", final / "state.json", active


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate compiler-produced V2 authority and activate one shared generation."
    )
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--state-export", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--canonical-ref", required=True)
    args = parser.parse_args()

    root = args.root.resolve()
    selected, failure = slice_authority.locate_bootstrap_manifest(root, explicit=args.manifest)
    if failure is not None or selected is None:
        print(json.dumps({"valid": False, "errors": [f"bootstrap: {failure.reason}: {failure.detail}"]}))
        return 2
    try:
        manifest = _load_manifest(selected)
        state = plan_execution.strict_json(args.state_export)
        if state.get("activation_mode") != "verify_only":
            raise ValueError(
                "pre-V2 bootstrap accepts compiler-produced activation_mode=verify_only only"
            )
        live = live_evidence.inspect(root, args.canonical_ref, plan_execution.candidate_commits(state))
        validation = execution_state.validate(state, live)
        errors = [*live.errors, *validation.errors, *_cross_validate(manifest, state, live)]
        if errors:
            print(json.dumps({"valid": False, "errors": sorted(set(errors))}, indent=2))
            return 2
        manifest_output, output, active = _install_generation(root, manifest, state)
    except (OSError, UnicodeError, ValueError, TypeError, OverflowError) as error:
        print(json.dumps({"valid": False, "errors": [f"bootstrap: {type(error).__name__}: {error}"]}))
        return 2
    print(json.dumps({
        "valid": True,
        "manifest_source": str(selected),
        "manifest": str(manifest_output),
        "state": str(output),
        "active_pointer": str(active),
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
