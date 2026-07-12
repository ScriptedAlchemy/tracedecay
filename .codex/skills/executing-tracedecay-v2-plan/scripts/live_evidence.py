#!/usr/bin/env python3
"""Live checkout evidence for the V2 completion-ledger selector."""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from git_observation import MAX_GIT_OUTPUT_BYTES, run_git
import slice_authority as sa


MAX_WORKTREES = 256
MAX_PLAN_FILE_BYTES = 4 * 1024 * 1024
COMMIT = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
FULL_REF = re.compile(r"^refs/(?:heads|remotes)/[^\x00-\x20~^:?*\\]+(?:/[^\x00-\x20~^:?*\\]+)*$")


def digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(sa._canonical_json(value).encode("utf-8")).hexdigest()


def source_set(root: Path, commit: str) -> list[list[str]]:
    """Hash the exact indexed plan blobs from one immutable Git tree."""
    listed = _git(
        root, "ls-tree", "-r", "-z", "--name-only", commit, "--",
        "docs/plans/2026-07-09-tracedecay-brain-rewrite.md", "docs/plans/tracedecay-v2",
    )
    if listed.error is not None or listed.returncode != 0:
        raise ValueError(f"cannot list canonical plan tree: {listed.error or listed.stderr}")
    paths = sorted(path for path in listed.stdout.split("\0") if path.endswith(".md"))
    if "docs/plans/2026-07-09-tracedecay-brain-rewrite.md" not in paths:
        raise ValueError("canonical plan tree is missing the master V2 plan")
    observations: list[list[str]] = []
    for path in paths:
        sized = _git(root, "cat-file", "-s", f"{commit}:{path}")
        if sized.error is not None or sized.returncode != 0:
            raise ValueError(f"cannot size canonical plan blob {path}: {sized.error or sized.stderr}")
        try:
            size = int(sized.stdout.strip())
        except ValueError as error:
            raise ValueError(f"invalid canonical plan blob size for {path}") from error
        if size > MAX_PLAN_FILE_BYTES:
            raise ValueError(f"canonical plan blob {path} exceeds {MAX_PLAN_FILE_BYTES} bytes")
        shown = _git(root, "show", f"{commit}:{path}", max_output_bytes=MAX_PLAN_FILE_BYTES)
        if shown.error is not None or shown.returncode != 0:
            raise ValueError(f"cannot read canonical plan blob {path}: {shown.error or shown.stderr}")
        observations.append([path, "sha256:" + hashlib.sha256(shown.stdout_bytes).hexdigest()])
    return observations


def source_set_digest(root: Path, commit: str) -> str:
    return digest(source_set(root, commit))


@dataclass(frozen=True)
class LiveEvidence:
    root: Path
    repository: str | None
    canonical_ref: str
    canonical_commit: str | None
    source_set_digest: str | None
    ancestry: dict[str, dict[str, Any]]
    workspaces: dict[str, dict[str, Any]]
    review_receipts: frozenset[str]
    test_receipts: frozenset[str]
    errors: tuple[str, ...]


@dataclass(frozen=True)
class GitResult:
    returncode: int
    stdout: str
    stderr: str
    stdout_bytes: bytes
    error: str | None = None


def _git(root: Path, *args: str, max_output_bytes: int = MAX_GIT_OUTPUT_BYTES) -> GitResult:
    """Text adapter over the shared bounded Git runner."""
    result = run_git(root, *args, max_output_bytes=max_output_bytes)
    try:
        out = result.stdout.decode("utf-8")
        err = result.stderr.decode("utf-8")
    except UnicodeDecodeError as error:
        return GitResult(result.returncode, "", "", b"", f"Git output is not UTF-8: {error}")
    return GitResult(result.returncode, out, err, result.stdout, result.error)


def workspace_key(commit: str, branch_ref: str, worktree: str) -> str:
    return "\0".join((commit, branch_ref, str(Path(worktree).resolve())))


def _worktree_observations(root: Path, repository: str) -> tuple[dict[str, dict[str, Any]], str | None]:
    result = _git(root, "worktree", "list", "--porcelain", "-z")
    if result.error is not None:
        return {}, f"live.git.worktrees: {result.error}"
    if result.returncode != 0:
        return {}, f"live.git.worktrees: command failed with exit {result.returncode}"
    observations: dict[str, dict[str, Any]] = {}
    blocks = [block for block in result.stdout.split("\0\0") if block]
    if len(blocks) > MAX_WORKTREES:
        return {}, f"live.git.worktrees: exceeds bound {MAX_WORKTREES}"
    for block in blocks:
        fields: dict[str, str] = {}
        for item in block.split("\0"):
            if " " in item:
                key, field_value = item.split(" ", 1)
                fields[key] = field_value
        if not all(fields.get(field) for field in ("worktree", "HEAD", "branch")):
            continue
        if not COMMIT.fullmatch(fields["HEAD"]) or not FULL_REF.fullmatch(fields["branch"]):
            return {}, "live.git.worktrees: malformed commit or branch ref"
        status = _git(root, "-C", fields["worktree"], "status", "--porcelain")
        if status.error is not None:
            return {}, f"live.git.worktree_status:{fields['worktree']}: {status.error}"
        if status.returncode != 0:
            return {}, (
                f"live.git.worktree_status:{fields['worktree']}: "
                f"command failed with exit {status.returncode}"
            )
        payload: dict[str, Any] = {
            "repository": repository,
            "candidate_commit": fields["HEAD"],
            "branch_ref": fields["branch"],
            "worktree": str(Path(fields["worktree"]).resolve()),
            "method": "git worktree list --porcelain -z",
            "status_method": "git status --porcelain",
            "clean": status.stdout == "",
        }
        payload["observation_digest"] = digest(payload)
        observations[workspace_key(fields["HEAD"], fields["branch"], fields["worktree"])] = payload
    return observations, None


def inspect(root: Path, canonical_ref: str, candidates: Iterable[str], *,
            review_receipts: Iterable[str] = (),
            test_receipts: Iterable[str] = ()) -> LiveEvidence:
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
    workspaces: dict[str, dict[str, Any]] = {}
    resolved_ref = canonical_ref

    top = _git(root, "rev-parse", "--show-toplevel")
    if top.error is not None:
        errors.append(f"live.git.repository: {top.error}")
    elif top.returncode != 0:
        errors.append(f"live.git.repository: command failed with exit {top.returncode}")
    elif len(top.stdout.splitlines()) != 1 or not top.stdout.strip():
        errors.append("live.git.repository: malformed repository root output")
    else:
        actual_root = Path(top.stdout.strip()).resolve()
        if actual_root != root:
            errors.append(f"live.root: {root} is not repository root {actual_root}")

    remote = _git(root, "remote", "get-url", "origin")
    if remote.error is not None:
        errors.append(f"live.git.repository_identity: {remote.error}")
    elif remote.returncode != 0:
        errors.append(f"live.git.repository_identity: command failed with exit {remote.returncode}")
    elif len(remote.stdout.splitlines()) != 1 or not remote.stdout.strip():
        errors.append("live.git.repository_identity: malformed remote URL output")
    else:
        repository = "git:" + remote.stdout.strip()

    symbolic = _git(root, "rev-parse", "--symbolic-full-name", "--verify", canonical_ref)
    if symbolic.error is not None:
        errors.append(f"live.git.canonical_ref: {symbolic.error}")
    elif (
        symbolic.returncode != 0
        or len(symbolic.stdout.splitlines()) != 1
        or not FULL_REF.fullmatch(symbolic.stdout.strip())
    ):
        errors.append("live.git.canonical_ref: must resolve to one exact full ref")
    else:
        resolved_ref = symbolic.stdout.strip()

    resolved = _git(root, "rev-parse", "--verify", f"{resolved_ref}^{{commit}}")
    if resolved.error is not None:
        errors.append(f"live.git.canonical_ref: {resolved.error}")
    elif resolved.returncode != 0:
        errors.append(f"live.git.canonical_ref: command failed with exit {resolved.returncode}")
    elif not COMMIT.fullmatch(resolved.stdout.strip()):
        errors.append("live.git.canonical_ref: malformed resolved commit output")
    else:
        canonical_commit = resolved.stdout.strip()

    if canonical_commit is not None:
        try:
            current_digest = source_set_digest(root, canonical_commit)
        except (OSError, UnicodeError, ValueError, TypeError, OverflowError) as error:
            errors.append(f"live.source_set: {type(error).__name__}: {error}")

    if repository is not None:
        workspaces, worktree_error = _worktree_observations(root, repository)
        if worktree_error is not None:
            errors.append(worktree_error)

    if repository is not None and canonical_commit is not None:
        for candidate in sorted(set(candidates)):
            observed = _git(root, "merge-base", "--is-ancestor", candidate, canonical_commit)
            if observed.error is not None:
                errors.append(f"live.git.ancestry:{candidate}: {observed.error}")
                continue
            if observed.returncode not in {0, 1}:
                errors.append(
                    f"live.git.ancestry:{candidate}: command failed with exit {observed.returncode}"
                )
                continue
            payload: dict[str, Any] = {
                "repository": repository,
                "candidate_commit": candidate,
                "canonical_commit": canonical_commit,
                "canonical_ref": resolved_ref,
                "method": "git merge-base --is-ancestor",
                "command_exit_code": observed.returncode,
                "status": "ancestor" if observed.returncode == 0 else "not_ancestor",
            }
            ancestry[candidate] = {**payload, "observation_digest": digest(payload)}

    return LiveEvidence(
        root=root,
        repository=repository,
        canonical_ref=resolved_ref,
        canonical_commit=canonical_commit,
        source_set_digest=current_digest,
        ancestry=ancestry,
        workspaces=workspaces,
        review_receipts=frozenset(review_receipts),
        test_receipts=frozenset(test_receipts),
        errors=tuple(sorted(set(errors))),
    )
