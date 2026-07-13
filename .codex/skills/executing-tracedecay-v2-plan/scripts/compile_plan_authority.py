#!/usr/bin/env python3
"""Compile an immutable checked V2 plan registry into verify-only authority."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
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
REGISTRY_SCHEMA = "tracedecay.v2.plan-authority-registry/v2"
REGISTRY_PATH = ".codex/skills/executing-tracedecay-v2-plan/scripts/plan_authority_registry.json"
MASTER_PATH = "docs/plans/2026-07-09-tracedecay-brain-rewrite.md"
PLAN_PREFIX = "docs/plans/tracedecay-v2/"
CANONICAL_MANIFEST_PATH = f"{PLAN_PREFIX}execution-authority.json"
COMPILER_VERSION = es.COMPILER_VERSION


@dataclass(frozen=True)
class RegistryCriterion:
    criterion_id: str
    text: str
    source_anchors: tuple[str, ...]


@dataclass(frozen=True)
class RegistryDependency:
    parent: str
    kind: str
    payload: tuple[tuple[str, object], ...]
    source_anchors: tuple[str, ...]


@dataclass(frozen=True)
class RegistrySlice:
    slice_id: str
    owner: sa.Anchor
    owner_heading: str
    source_anchors: tuple[sa.Anchor, ...]
    phase: int
    commit_subject: str
    acceptance: tuple[RegistryCriterion, ...]
    dependencies: tuple[RegistryDependency, ...]

    @property
    def parent_ids(self) -> tuple[str, ...]:
        return tuple(dependency.parent for dependency in self.dependencies)


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


def _sorted_unique_strings(value: object, label: str) -> tuple[str, ...]:
    if (
        not isinstance(value, list)
        or not value
        or value != sorted(set(value))
        or not all(isinstance(item, str) and item and item == item.strip() for item in value)
    ):
        raise ValueError(f"{label} must be non-empty sorted unique strings")
    return tuple(value)


def _registry_anchor(value: object, label: str) -> tuple[sa.Anchor, str]:
    owner = _exact_keys(
        value,
        {"path", "heading", "start_line", "end_line", "block_sha256"},
        label,
    )
    if (
        not isinstance(owner["path"], str)
        or not owner["path"].startswith((PLAN_PREFIX, "docs/plans/2026-"))
        or isinstance(owner["start_line"], bool)
        or not isinstance(owner["start_line"], int)
        or isinstance(owner["end_line"], bool)
        or not isinstance(owner["end_line"], int)
        or not isinstance(owner["heading"], str)
        or not owner["heading"]
        or not isinstance(owner["block_sha256"], str)
    ):
        raise ValueError(f"{label} is malformed")
    anchor = sa.Anchor(
        owner["path"], owner["start_line"], owner["end_line"], owner["block_sha256"]
    )
    diagnostics = sa.validate_source_anchor(anchor)
    if diagnostics:
        raise ValueError(f"{label} is malformed: {diagnostics[0].violated_rule}")
    return anchor, owner["heading"]


def _registry_source_anchors(value: object, label: str) -> tuple[sa.Anchor, ...]:
    if not isinstance(value, list) or not value:
        raise ValueError(f"{label} must be a non-empty array")
    anchors: list[sa.Anchor] = []
    for index, raw in enumerate(value):
        body = _exact_keys(
            raw, {"path", "start_line", "end_line", "block_sha256"}, f"{label}[{index}]"
        )
        anchor, _ = _registry_anchor({**body, "heading": "authority source"}, f"{label}[{index}]")
        anchors.append(anchor)
    refs = [anchor.ref() for anchor in anchors]
    if refs != sorted(set(refs)):
        raise ValueError(f"{label} must be sorted and unique")
    return tuple(anchors)


def _registry_acceptance(value: object, label: str) -> tuple[RegistryCriterion, ...]:
    if not isinstance(value, list) or not value:
        raise ValueError(f"{label} must be a non-empty array")
    result: list[RegistryCriterion] = []
    for index, raw in enumerate(value):
        body = _exact_keys(
            raw, {"criterion_id", "text", "source_anchors"}, f"{label}[{index}]"
        )
        criterion_id = body["criterion_id"]
        text = body["text"]
        if (
            not isinstance(criterion_id, str)
            or not criterion_id
            or criterion_id != criterion_id.strip()
            or not isinstance(text, str)
            or not sa.canonicalize_text(text)
        ):
            raise ValueError(f"{label}[{index}] has an invalid ID or text")
        anchors = _sorted_unique_strings(
            body["source_anchors"], f"{label}[{index}].source_anchors"
        )
        result.append(RegistryCriterion(criterion_id, text, anchors))
    if [item.criterion_id for item in result] != sorted({item.criterion_id for item in result}):
        raise ValueError(f"{label} must be sorted by unique criterion_id")
    return tuple(result)


def _registry_dependencies(value: object, label: str) -> tuple[RegistryDependency, ...]:
    if not isinstance(value, list):
        raise ValueError(f"{label} must be an array")
    result: list[RegistryDependency] = []
    for index, raw in enumerate(value):
        body = _exact_keys(
            raw, {"parent", "kind", "payload", "source_anchors"}, f"{label}[{index}]"
        )
        parent = body["parent"]
        kind = body["kind"]
        payload = body["payload"]
        if not isinstance(parent, str) or not parent or not isinstance(kind, str) or not kind:
            raise ValueError(f"{label}[{index}] has an invalid parent or kind")
        if not isinstance(payload, dict):
            raise ValueError(f"{label}[{index}].payload must be an object")
        anchors = _sorted_unique_strings(
            body["source_anchors"], f"{label}[{index}].source_anchors"
        )
        dependency = sa.Dependency(
            parent, kind, tuple(sorted(payload.items())), source_anchors=anchors
        )
        payload_error = sa._validate_payload(dependency)
        if kind not in sa.EDGE_KINDS or payload_error:
            raise ValueError(
                f"{label}[{index}] has invalid typed payload: "
                f"{payload_error or f'unknown edge kind {kind!r}'}"
            )
        result.append(RegistryDependency(parent, kind, dependency.payload, anchors))
    keys = [
        (item.parent, item.kind, sa._canonical_json(dict(item.payload)), item.source_anchors)
        for item in result
    ]
    if keys != sorted(set(keys)):
        raise ValueError(f"{label} must be sorted and contain unique typed edges")
    return tuple(result)


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
        body = _exact_keys(
            raw,
            {
                "owner", "source_anchors", "phase", "commit_subject", "acceptance",
                "dependencies",
            },
            slice_id,
        )
        owner, owner_heading = _registry_anchor(body["owner"], f"{slice_id}.owner")
        source_anchors = _registry_source_anchors(
            body["source_anchors"], f"{slice_id}.source_anchors"
        )
        if owner.ref() not in {anchor.ref() for anchor in source_anchors}:
            raise ValueError(f"{slice_id}.source_anchors must include the exact owner anchor")
        phase = body["phase"]
        if isinstance(phase, bool) or not isinstance(phase, int) or phase not in range(6):
            raise ValueError(f"{slice_id}.phase must be 0..5")
        subject = body["commit_subject"]
        if not isinstance(subject, str) or not subject or len(subject) > 72:
            raise ValueError(f"{slice_id}.commit_subject must be 1..72 characters")
        acceptance = _registry_acceptance(body["acceptance"], f"{slice_id}.acceptance")
        dependencies = _registry_dependencies(body["dependencies"], f"{slice_id}.dependencies")
        slices[slice_id] = RegistrySlice(
            slice_id, owner, owner_heading, source_anchors, phase, subject, acceptance,
            dependencies,
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
        unknown = set(record.parent_ids) - set(slices)
        if unknown:
            raise ValueError(f"{slice_id}: unknown dependencies {sorted(unknown)!r}")
        if slice_id in record.parent_ids:
            raise ValueError(f"{slice_id}: self dependency")
    _assert_acyclic({key: set(value.parent_ids) for key, value in slices.items()})
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
        if child not in slices or not parents <= set(slices[child].parent_ids):
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


_BULLET = re.compile(r"^\s*-\s+(?:\[[ xX]\]\s*)?(?P<text>.*\S)\s*$")
_CHECKED_BULLET = re.compile(r"^\s*-\s+\[[ xX]\]\s*(?P<text>.*\S)\s*$")
_METADATA_REQUIREMENT = re.compile(
    r"^(?:\*\*)?(?:Ordering|Files?|Commit(?: separately)?)(?::|\*\*:)", re.IGNORECASE
)


def _anchor_lines(materialized: Path, anchor: sa.Anchor) -> list[str]:
    lines = (materialized / anchor.path).read_text(encoding="utf-8").splitlines()
    if anchor.end_line > len(lines):
        raise ValueError(f"authority anchor is outside {anchor.path}")
    block = lines[anchor.start_line - 1:anchor.end_line]
    if sa.block_sha256(block) != anchor.block_sha256:
        raise ValueError(f"stale authority anchor {anchor.ref()}")
    return block


def _normative_requirements(materialized: Path, anchor: sa.Anchor) -> tuple[str, ...]:
    """Extract exact requirements only to detect drift from checked registry authority."""
    block = _anchor_lines(materialized, anchor)
    checked = [match.group("text").strip() for line in block if (match := _CHECKED_BULLET.match(line))]
    candidates = checked or [
        match.group("text").strip() for line in block if (match := _BULLET.match(line))
    ]
    normalized = {
        sa.canonicalize_text(text): text
        for text in candidates
        if text and not _METADATA_REQUIREMENT.match(text)
    }
    return tuple(normalized[key] for key in sorted(normalized))


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
            if _anchor(record) == authority.owner
            and record["heading"] == authority.owner_heading
        ]
        if len(owners) != 1:
            raise ValueError(f"{slice_id}: checked owner anchor resolves {len(owners)} declarations")
        owner = owners[0]
        declaration_headings = {_anchor(record).ref(): str(record["heading"]) for record in declarations}
        source_anchors = [authority.owner, *[
            anchor for anchor in authority.source_anchors if anchor != authority.owner
        ]]
        source_refs = {anchor.ref() for anchor in source_anchors}
        for anchor in source_anchors:
            _anchor_lines(materialized, anchor)
        anchor_names = {
            anchor.ref(): "owner" if index == 0 else f"companions[{index - 1}]"
            for index, anchor in enumerate(source_anchors)
        }
        normative = {
            anchor.ref(): {
                sa.canonicalize_text(text) for text in _normative_requirements(materialized, anchor)
            }
            for anchor in source_anchors
        }
        owner_expected = {
            (
                "AC-" + sa.criterion_digest(text)[:16].upper(),
                sa.canonicalize_text(text),
                authority.owner.ref(),
            )
            for text in _normative_requirements(materialized, authority.owner)
        }
        checked_acceptance = {
            (criterion.criterion_id, sa.canonicalize_text(criterion.text), anchor)
            for criterion in authority.acceptance
            for anchor in criterion.source_anchors
        }
        if not owner_expected <= checked_acceptance:
            raise ValueError(f"{slice_id}: checked acceptance omits owner requirements")
        acceptance: list[sa.Criterion] = []
        for criterion in authority.acceptance:
            expected_id = "AC-" + sa.criterion_digest(criterion.text)[:16].upper()
            if criterion.criterion_id != expected_id:
                raise ValueError(f"{slice_id}: criterion ID does not bind its exact text")
            unknown = set(criterion.source_anchors) - source_refs
            if unknown:
                raise ValueError(f"{slice_id}: acceptance has unregistered source anchors")
            canonical_text = sa.canonicalize_text(criterion.text)
            if not any(canonical_text in normative[anchor] for anchor in criterion.source_anchors):
                raise ValueError(f"{slice_id}: acceptance text is stale at its source anchors")
            acceptance.append(sa.Criterion(
                criterion.criterion_id,
                criterion.text,
                tuple(sorted(anchor_names[anchor] for anchor in criterion.source_anchors)),
            ))
        dependencies: list[sa.Dependency] = []
        for dependency in authority.dependencies:
            if not set(dependency.source_anchors) <= source_refs:
                raise ValueError(f"{slice_id}: dependency has unregistered source anchors")
            dependencies.append(sa.Dependency(
                dependency.parent,
                dependency.kind,
                dependency.payload,
                source_anchors=dependency.source_anchors,
            ))
        for index, anchor in enumerate(source_anchors):
            sections.append(sa.Section(
                raw_id=slice_id,
                role="owner" if index == 0 else "companion",
                anchor=anchor,
                heading=(
                    authority.owner_heading if index == 0
                    else declaration_headings.get(anchor.ref(), "registered ordering authority")
                ),
                phase=authority.phase if index == 0 else None,
                commit_subject=authority.commit_subject if index == 0 else None,
                acceptance=tuple(acceptance) if index == 0 else (),
                dependencies=tuple(dependencies) if index == 0 else (),
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
    manifest_digest = "sha256:" + hashlib.sha256(_canonical_json_bytes(manifest)).hexdigest()
    nodes = []
    for slice_id, record in sorted(reconciled.records.items()):
        unassigned = {"activation_mode": "verify_only", "slice_id": slice_id,
                      "workspace_policy": "unassigned"}
        nodes.append({
            "id": slice_id,
            "owner": f"{slice_id}@{record.owner.ref()}",
            "content_digest": record.content_digest,
            "dispatch_digest": es.dispatch_digest(unassigned),
            "dependencies": list(registry.slices[slice_id].parent_ids),
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
        "receipt_id": f"activation:verify-only:{revision}:{manifest_digest[7:23]}",
        "repository": live.repository,
        "source_commit": commit,
        "source_set_digest": live.source_set_digest,
        "graph_revision": revision,
        "graph_digest": graph["graph_digest"],
        "manifest_digest": manifest_digest,
        "candidate_graph_revision": revision,
        "activated_graph_revision": revision,
        "slice_count": len(nodes),
        "edge_count": sum(len(node["dependencies"]) for node in nodes),
        "series_count": len(registry.series),
        "compiler_version": COMPILER_VERSION,
        "validator_version": es.VALIDATOR_VERSION,
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
    payload = _canonical_json_bytes(document)
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


def _canonical_json_bytes(document: dict[str, Any]) -> bytes:
    return (json.dumps(document, indent=2, sort_keys=True) + "\n").encode("utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--canonical-ref", required=True)
    parser.add_argument("--graph-revision", type=int)
    parser.add_argument("--manifest-output", type=Path)
    parser.add_argument("--state-output", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        committed: bytes | None = None
        revision = args.graph_revision
        if args.check:
            commit = resolve_commit(args.root.resolve(), args.canonical_ref)
            committed = _git_bytes(
                args.root.resolve(), "show", f"{commit}:{CANONICAL_MANIFEST_PATH}"
            )
            if revision is None:
                document = json.loads(committed)
                if not isinstance(document, dict) or document.get("schema") != MANIFEST_SCHEMA:
                    raise ValueError("canonical manifest mismatch: invalid committed schema")
                revision = document.get("graph_revision")
        elif revision is None:
            revision = 1
        if isinstance(revision, bool) or not isinstance(revision, int) or revision < 1:
            raise ValueError("graph revision must be positive")
        compiled, live = compile_from_ref(args.root, args.canonical_ref, revision)
        if args.check:
            assert committed is not None
            if committed != _canonical_json_bytes(compiled.manifest):
                raise ValueError(
                    f"canonical manifest mismatch: regenerate {CANONICAL_MANIFEST_PATH}"
                )
        else:
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
