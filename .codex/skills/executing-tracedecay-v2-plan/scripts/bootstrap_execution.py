#!/usr/bin/env python3
"""Validate and atomically install one shared V2 execution-state export."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any

import compile_plan_authority
import execution_state
import live_evidence
import plan_execution
import slice_authority


GENERATIONS = Path(".tracedecay/v2-execution-generations")
ACTIVE_POINTER = Path(".tracedecay/v2-execution-active.json")
ACTIVATION_LOCK = Path(".tracedecay/v2-execution-activation.lock")
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
    receipt = graph.get("activation_receipt", {}) if isinstance(graph, dict) else {}
    expected = {
        "manifest_digest": "sha256:" + hashlib.sha256(_json_bytes(manifest)).hexdigest(),
        "candidate_graph_revision": manifest.get("graph_revision"),
        "activated_graph_revision": manifest.get("graph_revision"),
        "slice_count": len(slices) if isinstance(slices, dict) else -1,
        "edge_count": sum(
            len(body.get("dependencies", []))
            for body in slices.values()
            if isinstance(body, dict) and isinstance(body.get("dependencies"), list)
        ) if isinstance(slices, dict) else -1,
        "series_count": len(manifest.get("series", {}))
        if isinstance(manifest.get("series"), dict) else -1,
        "compiler_version": compile_plan_authority.COMPILER_VERSION,
        "validator_version": execution_state.VALIDATOR_VERSION,
    }
    for field, value in expected.items():
        if receipt.get(field) != value:
            errors.append(f"activation receipt {field} differs from compiled manifest/state")
    return sorted(set(errors))


def _json_bytes(document: dict[str, Any]) -> bytes:
    return compile_plan_authority._canonical_json_bytes(document)


@contextlib.contextmanager
def _activation_lock(root: Path):
    path = root / ACTIVATION_LOCK
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def _active_generation(root: Path) -> tuple[dict[str, Any], dict[str, Any]] | None:
    pointer = root / ACTIVE_POINTER
    if not pointer.exists():
        return None
    manifest_path, manifest_failure = slice_authority.resolve_active_generation(
        pointer, root, "manifest"
    )
    state_path, state_failure = slice_authority.resolve_active_generation(pointer, root, "state")
    failures = [failure for failure in (manifest_failure, state_failure) if failure is not None]
    if failures or manifest_path is None or state_path is None:
        detail = "; ".join(f"{failure.reason}: {failure.detail}" for failure in failures)
        raise ValueError(f"active execution generation is invalid: {detail}")
    return _load_manifest(manifest_path), plan_execution.strict_json(state_path)


def _check_compare_and_swap(root: Path, manifest: dict[str, Any], state: dict[str, Any]) -> bool:
    active = _active_generation(root)
    if active is None:
        return False
    active_manifest, active_state = active
    incoming_revision = manifest["graph_revision"]
    active_revision = active_manifest["graph_revision"]
    if incoming_revision < active_revision:
        raise ValueError(
            f"execution graph revision regression: {incoming_revision} < {active_revision}"
        )
    identical = (
        _json_bytes(manifest) == _json_bytes(active_manifest)
        and _json_bytes(state) == _json_bytes(active_state)
    )
    if incoming_revision == active_revision and not identical:
        raise ValueError(
            f"execution graph revision {incoming_revision} already activated with different bytes"
        )
    return identical


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
    with _activation_lock(root):
        replay = _check_compare_and_swap(root, manifest, state)
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
        if not replay:
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
        graph = state.get("canonical_dag", {})
        revision = graph.get("graph_revision") if isinstance(graph, dict) else None
        if isinstance(revision, bool) or not isinstance(revision, int) or revision < 1:
            raise ValueError("state graph revision must be a positive integer")
        compiled, live = compile_plan_authority.compile_from_ref(
            root, args.canonical_ref, revision
        )
        if selected.read_bytes() != _json_bytes(compiled.manifest):
            raise ValueError("supplied manifest bytes differ from canonical compiler output")
        if args.state_export.read_bytes() != _json_bytes(compiled.state):
            raise ValueError("supplied state bytes differ from canonical compiler output")
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
