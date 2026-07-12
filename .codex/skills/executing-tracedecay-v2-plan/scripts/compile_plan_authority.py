#!/usr/bin/env python3
"""Compile an immutable checked V2 plan registry into verify-only authority."""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator

import execution_state as es
import live_evidence
import plan_execution
import plan_inventory
import slice_authority as sa
from git_observation import run_git


MANIFEST_SCHEMA = "tracedecay.v2.slice-dag/v1"
REGISTRY_SCHEMA = "tracedecay.v2.plan-authority-registry/v1"
REGISTRY_PATH = ".codex/skills/executing-tracedecay-v2-plan/scripts/plan_authority_registry.json"
MASTER_PATH = "docs/plans/2026-07-09-tracedecay-brain-rewrite.md"
PLAN_PREFIX = "docs/plans/tracedecay-v2/"


@dataclass(frozen=True)
class RegistrySlice:
    slice_id: str
    owner_path: str
    owner_line: int
    owner_heading: str
    phase: int
    commit_subject: str
    dependencies: tuple[str, ...]


@dataclass(frozen=True)
class Registry:
    slices: dict[str, RegistrySlice]
    series: dict[str, tuple[str, ...]]


@dataclass(frozen=True)
class Compiled:
    manifest: dict[str, Any]
    state: dict[str, Any]
    records: dict[str, sa.SliceRecord]
    registry: Registry
    limitations: tuple[str, ...]


def _git_bytes(root: Path, *args: str, maximum: int = 4 * 1024 * 1024) -> bytes:
    result = run_git(root, *args, max_output_bytes=maximum)
    if result.error is not None or result.returncode != 0:
        detail = result.error or result.stderr.decode("utf-8", "replace")
        raise ValueError(f"git {' '.join(args)} failed: {detail}")
    return result.stdout


def resolve_commit(root: Path, canonical_ref: str) -> str:
    commit = _git_bytes(root, "rev-parse", "--verify", f"{canonical_ref}^{{commit}}").decode().strip()
    if not es.COMMIT.fullmatch(commit):
        raise ValueError(f"canonical ref {canonical_ref!r} did not resolve to one full commit")
    return commit


@contextlib.contextmanager
def materialized_commit(root: Path, commit: str) -> Iterator[Path]:
    """Materialize only compiler inputs from immutable Git blobs, never the checkout."""
    listed = _git_bytes(
        root, "ls-tree", "-r", "-z", "--name-only", commit, "--",
        MASTER_PATH, PLAN_PREFIX, REGISTRY_PATH,
    ).decode("utf-8").split("\0")
    paths = sorted(path for path in listed if path)
    if MASTER_PATH not in paths or REGISTRY_PATH not in paths:
        raise ValueError("canonical commit lacks the master plan or checked authority registry")
    with tempfile.TemporaryDirectory(prefix="tracedecay-v2-plan-") as directory:
        materialized = Path(directory)
        for relative in paths:
            destination = materialized / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(_git_bytes(root, "show", f"{commit}:{relative}"))
        yield materialized


def _exact_keys(value: object, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        raise ValueError(f"{label}: expected fields {sorted(expected)!r}; got {actual!r}")
    return value


def load_registry(materialized: Path) -> Registry:
    document = plan_execution.strict_json(materialized / REGISTRY_PATH)
    _exact_keys(document, {"schema", "series", "slices"}, "registry")
    if document["schema"] != REGISTRY_SCHEMA:
        raise ValueError(f"registry.schema must be {REGISTRY_SCHEMA!r}")
    raw_slices = document["slices"]
    raw_series = document["series"]
    if not isinstance(raw_slices, dict) or not raw_slices:
        raise ValueError("registry.slices must be a non-empty object")
    if not isinstance(raw_series, dict) or not raw_series:
        raise ValueError("registry.series must be a non-empty object")

    slices: dict[str, RegistrySlice] = {}
    for slice_id, raw in sorted(raw_slices.items()):
        classification = sa.classify_token(slice_id)
        if classification.kind != "declaration" or classification.ids != (slice_id,):
            raise ValueError(f"registry slice {slice_id!r} is not one canonical scalar")
        body = _exact_keys(raw, {"owner", "phase", "commit_subject", "dependencies"}, slice_id)
        owner = _exact_keys(body["owner"], {"path", "line", "heading"}, f"{slice_id}.owner")
        if (
            not isinstance(owner["path"], str)
            or not owner["path"].startswith((PLAN_PREFIX, "docs/plans/2026-"))
            or isinstance(owner["line"], bool)
            or not isinstance(owner["line"], int)
            or owner["line"] < 1
            or not isinstance(owner["heading"], str)
            or not owner["heading"]
        ):
            raise ValueError(f"{slice_id}.owner is malformed")
        phase = body["phase"]
        if isinstance(phase, bool) or not isinstance(phase, int) or phase not in range(6):
            raise ValueError(f"{slice_id}.phase must be 0..5")
        subject = body["commit_subject"]
        if not isinstance(subject, str) or not subject or len(subject) > 72:
            raise ValueError(f"{slice_id}.commit_subject must be 1..72 characters")
        dependencies = body["dependencies"]
        if (
            not isinstance(dependencies, list)
            or dependencies != sorted(set(dependencies))
            or not all(isinstance(parent, str) and parent for parent in dependencies)
        ):
            raise ValueError(f"{slice_id}.dependencies must be sorted unique strings")
        slices[slice_id] = RegistrySlice(
            slice_id, owner["path"], owner["line"], owner["heading"], phase, subject,
            tuple(dependencies),
        )

    series: dict[str, tuple[str, ...]] = {}
    claimed: dict[str, str] = {}
    for series_id, raw_members in sorted(raw_series.items()):
        if sa.classify_token(series_id).kind != "series":
            raise ValueError(f"registry series {series_id!r} is malformed")
        if (
            not isinstance(raw_members, list)
            or not raw_members
            or raw_members != sorted(set(raw_members))
            or not set(raw_members) <= set(slices)
        ):
            raise ValueError(f"{series_id}: members must be sorted unique executable IDs")
        for member in raw_members:
            prior = claimed.setdefault(member, series_id)
            if prior != series_id:
                raise ValueError(f"{member}: claimed by both {prior} and {series_id}")
        series[series_id] = tuple(raw_members)

    aggregate_ids = {series_id.removesuffix(" series") for series_id in series}
    overlap = aggregate_ids & set(slices)
    if overlap:
        raise ValueError(f"series aggregates are not executable slices: {sorted(overlap)!r}")
    for slice_id, record in slices.items():
        unknown = set(record.dependencies) - set(slices)
        if unknown:
            raise ValueError(f"{slice_id}: unknown dependencies {sorted(unknown)!r}")
        if slice_id in record.dependencies:
            raise ValueError(f"{slice_id}: self dependency")
    _assert_acyclic({key: set(value.dependencies) for key, value in slices.items()})
    _validate_required_edges(slices)
    return Registry(slices, series)


def _assert_acyclic(dependencies: dict[str, set[str]]) -> None:
    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(node: str, trail: list[str]) -> None:
        if node in visiting:
            start = trail.index(node)
            raise ValueError("dependency cycle: " + " -> ".join(trail[start:] + [node]))
        if node in visited:
            return
        visiting.add(node)
        for parent in sorted(dependencies[node]):
            visit(parent, trail + [node])
        visiting.remove(node)
        visited.add(node)

    for node in sorted(dependencies):
        visit(node, [])


def _validate_required_edges(slices: dict[str, RegistrySlice]) -> None:
    required = {
        "PR 4E": {"PR 4C"},
        "PR 10F": {"PR 10E"},
        "PR 14E": {"PR 6C", "PR 18D", "PR 14A"},
        "PR 22F-LS": {"PR 22F"},
        "PR 24O": {"PR 24L", "PR 24M", "PR 24N"},
        "PR 24P": {"PR 24O"},
        "PR 35H": {"PR 35G", "PR 35I", "PR 35J"},
        "PR 36S": {"PR 33I", "PR 35"},
        "PR 37L": {"PR 36S", "PR 37K"},
        "PR 38I": {"PR 38D", "PR 24F", "PR 24P", "PR 24S", "PR 22F-LE", "PR 30J"},
        "PR 38J": {"PR 38E", "PR 38F", "PR 38G", "PR 38H", "PR 38I"},
        "PR 38K": {"PR 38J", "PR 33", "PR 34", "PR 35", "PR 36"},
    }
    for child, parents in required.items():
        if child not in slices or not parents <= set(slices[child].dependencies):
            raise ValueError(f"checked registry is missing required edge(s) for {child}")


def _inventory(materialized: Path) -> list[dict[str, object]]:
    plan_inventory.validate_failure_matrix(materialized)
    records = [
        record
        for path in plan_inventory.plan_files(materialized)
        for record in plan_inventory.scan(path, materialized)
    ]
    return sorted(records, key=lambda item: (str(item["path"]), int(item["line"])))


def _anchor(record: dict[str, object]) -> sa.Anchor:
    return sa.Anchor(
        str(record["path"]), int(record["line"]), int(record["end_line"]),
        str(record["block_sha256"]),
    )


def _criteria(materialized: Path, record: dict[str, object], anchor_name: str) -> tuple[sa.Criterion, ...]:
    lines = (materialized / str(record["path"])).read_text(encoding="utf-8").splitlines()
    result: list[sa.Criterion] = []
    for line in lines[int(record["line"]) - 1:int(record["end_line"])]:
        if plan_inventory.CHECKBOX.match(line):
            text = re.sub(r"^\s*- \[[ xX]\]\s*", "", line).strip()
            if text:
                result.append(sa.Criterion(
                    "AC-" + sa.criterion_digest(text)[:16].upper(), text, (anchor_name,),
                ))
    return tuple(result)


def _sections(materialized: Path, registry: Registry,
              inventory: list[dict[str, object]]) -> list[sa.Section]:
    by_id: dict[str, list[dict[str, object]]] = {slice_id: [] for slice_id in registry.slices}
    for record in inventory:
        for slice_id in record["ids"]:
            if slice_id in by_id:
                by_id[slice_id].append(record)

    sections: list[sa.Section] = []
    for slice_id, authority in registry.slices.items():
        declarations = by_id[slice_id]
        owners = [
            record for record in declarations
            if record["path"] == authority.owner_path
            and record["line"] == authority.owner_line
            and record["heading"] == authority.owner_heading
        ]
        if len(owners) != 1:
            raise ValueError(f"{slice_id}: checked owner anchor resolves {len(owners)} declarations")
        owner = owners[0]
        companions = sorted(
            [record for record in declarations if record is not owner],
            key=lambda item: (str(item["path"]), int(item["line"])),
        )
        for index, record in enumerate([owner, *companions]):
            name = "owner" if index == 0 else f"companions[{index - 1}]"
            anchor = _anchor(record)
            dependencies = ()
            if index == 0:
                dependencies = tuple(
                    sa.Dependency(parent, "requires_success", source_anchors=(anchor.ref(),))
                    for parent in authority.dependencies
                )
            sections.append(sa.Section(
                raw_id=slice_id,
                role="owner" if index == 0 else "companion",
                anchor=anchor,
                heading=str(record["heading"]),
                phase=authority.phase if index == 0 else None,
                commit_subject=authority.commit_subject if index == 0 else None,
                acceptance=_criteria(materialized, record, name),
                dependencies=dependencies,
            ))

    aggregate_records: dict[str, list[dict[str, object]]] = {}
    for series_id in registry.series:
        aggregate = series_id.removesuffix(" series")
        aggregate_records[series_id] = [
            record for record in inventory if aggregate in record["ids"]
        ]
    for series_id, records in sorted(aggregate_records.items()):
        if not records:
            raise ValueError(f"{series_id}: no aggregate declaration anchors the series")
        for record in records:
            sections.append(sa.Section(
                raw_id=series_id, role="companion", anchor=_anchor(record),
                heading=str(record["heading"]),
            ))
    return sections


def compile_materialized(root: Path, materialized: Path, commit: str,
                         live: live_evidence.LiveEvidence, revision: int = 1) -> Compiled:
    if live.errors or live.canonical_commit != commit or not live.repository or not live.source_set_digest:
        raise ValueError("live Git authority is incomplete: " + "; ".join(live.errors))
    registry = load_registry(materialized)
    inventory = _inventory(materialized)
    indexed_paths = frozenset(str(path.relative_to(materialized)) for path in plan_inventory.plan_files(materialized))
    reconciled = sa.reconcile(
        _sections(materialized, registry, inventory),
        authority_keys=frozenset(registry.slices),
        series=registry.series,
        repo_root=root,
        source_commit=commit,
        indexed_plan_paths=indexed_paths,
    )
    if reconciled.errors:
        raise ValueError("slice reconciliation failed:\n" + "\n".join(
            f"{item.code}:{item.normalized_id}:{item.violated_rule}" for item in reconciled.errors
        ))

    slices = {
        slice_id: {
            **record.reconciled_body(),
            "content_digest": record.content_digest,
            "idempotency_key": record.idempotency_key,
        }
        for slice_id, record in sorted(reconciled.records.items())
    }
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "graph_revision": revision,
        "source_set_digest": live.source_set_digest,
        "slices": slices,
        "series": {key: list(value) for key, value in sorted(registry.series.items())},
    }
    nodes = []
    for slice_id, record in sorted(reconciled.records.items()):
        unassigned = {"activation_mode": "verify_only", "slice_id": slice_id,
                      "workspace_policy": "unassigned"}
        nodes.append({
            "id": slice_id,
            "owner": f"{slice_id}@{record.owner.ref()}",
            "content_digest": record.content_digest,
            "dispatch_digest": es.dispatch_digest(unassigned),
            "dependencies": list(registry.slices[slice_id].dependencies),
        })
    graph: dict[str, Any] = {
        "schema": es.DAG_SCHEMA,
        "repository": live.repository,
        "source_commit": commit,
        "source_set_digest": live.source_set_digest,
        "graph_revision": revision,
        "graph_digest": "",
        "activation_receipt": {},
        "nodes": nodes,
    }
    graph["graph_digest"] = es.graph_digest(graph)
    graph["activation_receipt"] = {
        "receipt_id": f"activation:verify-only:{revision}",
        "repository": live.repository,
        "source_commit": commit,
        "source_set_digest": live.source_set_digest,
        "graph_revision": revision,
        "graph_digest": graph["graph_digest"],
        "activated": True,
    }
    state = {
        "schema": es.EXPORT_SCHEMA,
        "activation_mode": "verify_only",
        "canonical_dag": graph,
        "completion_ledger": {
            "schema": es.LEDGER_SCHEMA,
            "repository": live.repository,
            "source_commit": commit,
            "source_set_digest": live.source_set_digest,
            "graph_revision": revision,
            "graph_digest": graph["graph_digest"],
            "entries": [],
        },
        "dispatch_specs": [],
        "retired_obligations": ["FM-168"],
    }
    validation = es.validate(state, live)
    if validation.errors:
        raise ValueError("compiled execution state is invalid:\n" + "\n".join(validation.errors))
    return Compiled(
        manifest, state, reconciled.records, registry,
        ("verify-only: dispatch packets, workspaces, branches, tests, and completion are absent",),
    )


def compile_from_ref(root: Path, canonical_ref: str, revision: int = 1
                     ) -> tuple[Compiled, live_evidence.LiveEvidence]:
    root = root.resolve()
    commit = resolve_commit(root, canonical_ref)
    live = live_evidence.inspect(root, canonical_ref, [])
    with materialized_commit(root, commit) as materialized:
        return compile_materialized(root, materialized, commit, live, revision), live


def _atomic_json(path: Path, document: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = (json.dumps(document, indent=2, sort_keys=True) + "\n").encode("utf-8")
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=f".{path.name}.", delete=False) as file:
        temporary = Path(file.name)
        file.write(payload)
        file.flush()
        os.fsync(file.fileno())
    try:
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--canonical-ref", required=True)
    parser.add_argument("--graph-revision", type=int, default=1)
    parser.add_argument("--manifest-output", type=Path)
    parser.add_argument("--state-output", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        if args.graph_revision < 1:
            raise ValueError("graph revision must be positive")
        compiled, live = compile_from_ref(args.root, args.canonical_ref, args.graph_revision)
        if not args.check:
            if args.manifest_output is None or args.state_output is None:
                raise ValueError("--manifest-output and --state-output are required unless --check is used")
            _atomic_json(args.manifest_output.resolve(), compiled.manifest)
            _atomic_json(args.state_output.resolve(), compiled.state)
        view = es.next_ready(es.validate(compiled.state, live))
        print(json.dumps({
            "valid": True,
            "activation_mode": "verify_only",
            "slice_count": len(compiled.records),
            "series_count": len(compiled.registry.series),
            "edge_count": sum(len(node["dependencies"]) for node in compiled.state["canonical_dag"]["nodes"]),
            "execution_order_count": len(view["execution_order"]),
            "next_ready_count": len(view["next_ready"]),
            "manifest": str(args.manifest_output.resolve()) if args.manifest_output else None,
            "state": str(args.state_output.resolve()) if args.state_output else None,
            "limitations": list(compiled.limitations),
        }, indent=2, sort_keys=True))
        return 0
    except (OSError, UnicodeError, ValueError, TypeError, OverflowError) as error:
        print(json.dumps({"valid": False, "errors": [f"{type(error).__name__}: {error}"]}, indent=2))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
