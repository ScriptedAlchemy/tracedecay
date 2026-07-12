#!/usr/bin/env python3
"""Live checkout evidence for the V2 completion-ledger selector."""

from __future__ import annotations

import hashlib
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

import plan_inventory
import slice_authority as sa


def digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(sa._canonical_json(value).encode("utf-8")).hexdigest()


def source_set(root: Path) -> list[dict[str, Any]]:
    """Return canonical current plan-inventory observations used for freshness."""
    observations: list[dict[str, Any]] = []
    for path in plan_inventory.plan_files(root):
        observations.extend(plan_inventory.scan(path, root))
    return sorted(
        observations,
        key=lambda item: (
            str(item.get("path", "")),
            int(item.get("line", 0)),
            tuple(item.get("ids", [])),
            str(item.get("block_sha256", "")),
        ),
    )


def source_set_digest(root: Path) -> str:
    return digest(source_set(root))


@dataclass(frozen=True)
class LiveEvidence:
    root: Path
    repository: str | None
    canonical_ref: str
    canonical_commit: str | None
    source_set_digest: str | None
    ancestry: dict[str, dict[str, Any]]
    errors: tuple[str, ...]


def _git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=root, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=False,
    )


def inspect(root: Path, canonical_ref: str, candidates: Iterable[str]) -> LiveEvidence:
    """Observe authoritative ref, current source blocks, and candidate ancestry.

    Every Git/process failure remains an explicit error. Callers must suppress packets
    rather than converting failures into negative or positive ancestry assertions.
    """
    root = root.resolve()
    errors: list[str] = []
    repository: str | None = None
    canonical_commit: str | None = None
    current_digest: str | None = None
    ancestry: dict[str, dict[str, Any]] = {}

    top = _git(root, "rev-parse", "--show-toplevel")
    if top.returncode != 0:
        errors.append(f"live.git.repository: command failed with exit {top.returncode}")
    else:
        actual_root = Path(top.stdout.strip()).resolve()
        if actual_root != root:
            errors.append(f"live.root: {root} is not repository root {actual_root}")

    remote = _git(root, "remote", "get-url", "origin")
    if remote.returncode != 0 or not remote.stdout.strip():
        errors.append(f"live.git.repository_identity: command failed with exit {remote.returncode}")
    else:
        repository = "git:" + remote.stdout.strip()

    resolved = _git(root, "rev-parse", "--verify", f"{canonical_ref}^{{commit}}")
    if resolved.returncode != 0 or not resolved.stdout.strip():
        errors.append(f"live.git.canonical_ref: command failed with exit {resolved.returncode}")
    else:
        canonical_commit = resolved.stdout.strip()

    try:
        current_digest = source_set_digest(root)
    except (OSError, UnicodeError, ValueError, TypeError, OverflowError) as error:
        errors.append(f"live.source_set: {type(error).__name__}: {error}")

    if repository is not None and canonical_commit is not None:
        for candidate in sorted(set(candidates)):
            observed = _git(root, "merge-base", "--is-ancestor", candidate, canonical_commit)
            if observed.returncode not in {0, 1}:
                errors.append(
                    f"live.git.ancestry:{candidate}: command failed with exit {observed.returncode}"
                )
                continue
            payload: dict[str, Any] = {
                "repository": repository,
                "candidate_commit": candidate,
                "canonical_commit": canonical_commit,
                "canonical_ref": canonical_ref,
                "method": "git merge-base --is-ancestor",
                "command_exit_code": observed.returncode,
                "status": "ancestor" if observed.returncode == 0 else "not_ancestor",
            }
            ancestry[candidate] = {**payload, "observation_digest": digest(payload)}

    return LiveEvidence(
        root=root,
        repository=repository,
        canonical_ref=canonical_ref,
        canonical_commit=canonical_commit,
        source_set_digest=current_digest,
        ancestry=ancestry,
        errors=tuple(sorted(set(errors))),
    )
