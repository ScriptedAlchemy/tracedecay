#!/usr/bin/env python3
"""Report (default), remove merged, and reclaim idle build output of linked Git worktrees.

A lane (linked worktree) is MERGED when its work already reached the
integration branch by any of the ways this repository lands work:

  ancestor  HEAD is an ancestor of the integration tip
  pr        the branch's pull request is merged into the integration branch and
            HEAD is (an ancestor of) the merged PR head; one batched
            `gh api graphql` request covers every lane branch
  patch     every lane commit, or the lane's combined diff (a squash merge),
            is patch-equivalent (`git patch-id`, as `git cherry`) to an
            integration commit, or the lane tree equals its merge base

Guarantees: every mutated path is an exact absolute path from
`git worktree list`; the primary checkout is never mutated; ACTIVE (a live
process cwd or executable inside), DIRTY, LOCKED, and NESTED lanes are never
removed; removal re-verifies state at delete time and uses plain
`git worktree remove` (no --force); branch deletion is a compare-and-swap on
the verified SHA. Build reclamation deletes only regenerable, git-ignored
build output (`target/`, `node_modules/` beside a tracked package-lock.json)
of idle, inactive lanes, dirty or not, and never source.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path

HOUR = 3600
GH_TIMEOUT_SECONDS = 60
# Regenerable build output, relative to a lane root, with the tracked file
# that proves the build tool can regenerate it.
BUILD_OUTPUTS = (
    ("target", "Cargo.toml"),
    ("node_modules", "package-lock.json"),
    ("dashboard/node_modules", "dashboard/package-lock.json"),
)
STATUSES = ("PRIMARY", "PRUNABLE", "ACTIVE", "NESTED", "DIRTY", "LOCKED", "UNMERGED", "FRESH", "MERGED")


class GcError(Exception):
    pass


def git(*args: str, cwd: str | None = None, check: bool = True, input_bytes: bytes | None = None) -> bytes:
    cmd = ["git"]
    if cwd is not None:
        cmd += ["-C", cwd]
    cmd += ["--no-optional-locks", *args]
    proc = subprocess.run(cmd, input=input_bytes, capture_output=True)
    if check and proc.returncode != 0:
        raise GcError(f"`{' '.join(cmd)}` failed: {proc.stderr.decode(errors='replace').strip()}")
    return proc.stdout


def git_ok(*args: str, cwd: str | None = None) -> bool:
    cmd = ["git"] + (["-C", cwd] if cwd else []) + ["--no-optional-locks", *args]
    return subprocess.run(cmd, capture_output=True).returncode == 0


def inside(parent: str, child: str) -> bool:
    return child == parent or child.startswith(parent.rstrip("/") + "/")


def live_process_paths() -> list[tuple[int, str]]:
    """One inventory of (pid, cwd-or-exe) for every visible process."""
    if os.path.isdir("/proc/self"):
        found = []
        for entry in os.scandir("/proc"):
            if not entry.name.isdigit():
                continue
            for link in ("cwd", "exe"):
                try:
                    target = os.readlink(f"/proc/{entry.name}/{link}")
                except OSError:
                    continue
                found.append((int(entry.name), target.removesuffix(" (deleted)")))
        return found
    if shutil.which("lsof") is None:
        raise GcError("cannot inventory live processes: no /proc and no lsof")
    proc = subprocess.run(["lsof", "-w", "-n", "-P", "-d", "cwd,txt", "-F", "pn"], capture_output=True)
    found, pid = [], None
    for line in proc.stdout.decode(errors="replace").splitlines():
        if line.startswith("p"):
            pid = int(line[1:])
        elif line.startswith("n") and pid is not None:
            found.append((pid, line[1:]))
    if not found:
        raise GcError(f"lsof returned no processes (exit {proc.returncode})")
    return found


def pids_inside(real_path: str, processes: list[tuple[int, str]]) -> list[int]:
    return sorted({pid for pid, path in processes if inside(real_path, path)})


@dataclass
class Lane:
    path: str
    head: str | None
    branch: str | None
    locked: bool
    prunable: bool
    primary: bool = False
    real: str = ""
    gitdir: str | None = None
    dirty: list[str] = field(default_factory=list)
    status_error: str | None = None
    pids: list[int] = field(default_factory=list)
    nested: list[str] = field(default_factory=list)
    last_write: float = 0.0
    merged_by: str | None = None
    unmerged_why: str = ""
    status: str = ""
    reason: str = ""
    builds: list[tuple[str, int]] = field(default_factory=list)


def parse_worktrees(repo: str) -> list[Lane]:
    lanes: list[Lane] = []
    record: dict[str, str] = {}

    def flush() -> None:
        if "worktree" in record and "bare" not in record:
            path = record["worktree"]
            if not os.path.isabs(path):
                raise GcError(f"refusing non-absolute worktree path: {path!r}")
            branch = record.get("branch")
            lanes.append(
                Lane(
                    path=path,
                    head=record.get("HEAD"),
                    branch=branch.removeprefix("refs/heads/") if branch else None,
                    locked="locked" in record,
                    prunable="prunable" in record,
                )
            )
        record.clear()

    for item in git("worktree", "list", "--porcelain", "-z", cwd=repo).decode().split("\0"):
        if not item:
            flush()
            continue
        key, _, value = item.partition(" ")
        record[key] = value
    flush()
    if not lanes:
        raise GcError("git worktree list returned no worktrees")
    lanes[0].primary = True
    return lanes


def lane_gitdir(lane: Lane) -> str | None:
    try:
        text = Path(lane.path, ".git").read_text()
    except OSError:
        return None
    if not text.startswith("gitdir: "):
        return None
    return os.path.normpath(os.path.join(lane.path, text[len("gitdir: ") :].strip()))


def porcelain_paths(raw: bytes) -> list[str]:
    items = raw.decode(errors="surrogateescape").split("\0")
    paths, i = [], 0
    while i < len(items):
        item = items[i]
        i += 1
        if not item:
            continue
        paths.append(item[3:])
        if item[0] in "RC":
            i += 1
    return paths


def newest_mtime(paths: list[str]) -> float:
    newest = 0.0
    for path in paths:
        try:
            newest = max(newest, os.lstat(path).st_mtime)
        except OSError:
            continue
    return newest


def build_dir_markers(path: str) -> list[str]:
    """The build dir and its directories two levels down: new artifacts touch them."""
    marks = [path]
    try:
        for level1 in os.scandir(path):
            if level1.is_dir(follow_symlinks=False):
                marks.append(level1.path)
                for level2 in os.scandir(level1.path):
                    if level2.is_dir(follow_symlinks=False):
                        marks.append(level2.path)
    except OSError:
        pass
    return marks


def inspect_lane(lane: Lane) -> None:
    lane.real = os.path.realpath(lane.path)
    if lane.primary or lane.prunable or not os.path.isdir(lane.path):
        return
    lane.gitdir = lane_gitdir(lane)
    proc = subprocess.run(
        ["git", "-C", lane.path, "--no-optional-locks", "status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        capture_output=True,
    )
    if proc.returncode != 0:
        lane.status_error = proc.stderr.decode(errors="replace").strip() or f"git status exit {proc.returncode}"
    else:
        lane.dirty = porcelain_paths(proc.stdout)
    # Only signals that housekeeping leaves alone: `git gc` rewrites reflog
    # files and git/tool scans touch the lane root and admin dir, so those
    # mtimes say nothing about the lane owner's last write.
    marks = [os.path.join(lane.path, p) for p in lane.dirty]
    if lane.gitdir:
        marks += [os.path.join(lane.gitdir, n) for n in ("index", "HEAD")]
    for rel, _ in BUILD_OUTPUTS:
        candidate = os.path.join(lane.path, rel)
        if os.path.isdir(candidate) and not os.path.islink(candidate):
            marks += build_dir_markers(candidate)
    lane.last_write = max(newest_mtime(marks), last_reflog_time(lane.gitdir))


def last_reflog_time(gitdir: str | None) -> float:
    if gitdir is None:
        return 0.0
    try:
        with open(os.path.join(gitdir, "logs/HEAD"), "rb") as log:
            log.seek(0, os.SEEK_END)
            log.seek(max(0, log.tell() - 4096))
            lines = log.read().splitlines()
    except OSError:
        return 0.0
    # "<old> <new> <name> <email> <epoch> <tz>\t<message>"
    for line in reversed(lines):
        fields = line.split(b"\t", 1)[0].split()
        if len(fields) >= 2 and fields[-2].isdigit():
            return float(fields[-2])
    return 0.0


def resolve_integration(repo: str, ref: str | None) -> tuple[str, str, str]:
    if ref is None:
        head = (
            git("symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD", cwd=repo, check=False)
            .decode()
            .strip()
        )
        ref = head.removeprefix("origin/") if head else "master"
    for candidate in (
        [ref] if ref.startswith(("origin/", "refs/")) else [f"refs/remotes/origin/{ref}", f"refs/heads/{ref}", ref]
    ):
        tip = git("rev-parse", "--verify", "--quiet", f"{candidate}^{{commit}}", cwd=repo, check=False).decode().strip()
        if tip:
            name = candidate.removeprefix("refs/remotes/").removeprefix("refs/heads/")
            short = name.removeprefix("origin/")
            return name, short, tip
    raise GcError(f"unknown integration ref: {ref}")


def github_repo_slug(repo: str, explicit: str | None) -> tuple[str | None, str]:
    if explicit:
        return explicit, ""
    url = git("config", "--get", "remote.origin.url", cwd=repo, check=False).decode().strip()
    match = re.search(r"github\.com[:/]([^/]+)/([^/]+?)(?:\.git)?/?$", url)
    if not match:
        return None, f"origin {url or '(none)'} is not a GitHub remote"
    return f"{match.group(1)}/{match.group(2)}", ""


def merged_pull_requests(slug: str, heads: list[str], base: str) -> tuple[dict[str, list[tuple[int, str]]], str]:
    """One GraphQL request: head branch -> [(pr number, merged head oid)] merged into `base`."""
    if not heads:
        return {}, "ok (no branches)"
    if shutil.which("gh") is None:
        return {}, "unavailable (gh not found)"
    owner, name = slug.split("/", 1)
    aliases = "\n".join(
        f"b{i}: pullRequests(headRefName: {json.dumps(h)}, baseRefName: {json.dumps(base)}, states: MERGED, first: 5)"
        " { nodes { number headRefOid headRepository { nameWithOwner } } }"
        for i, h in enumerate(heads)
    )
    query = f"query {{ repository(owner: {json.dumps(owner)}, name: {json.dumps(name)}) {{\n{aliases}\n}} }}"
    try:
        proc = subprocess.run(
            ["gh", "api", "graphql", "--input", "-"],
            input=json.dumps({"query": query}).encode(),
            capture_output=True,
            timeout=GH_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return {}, f"unavailable (gh api graphql timed out after {GH_TIMEOUT_SECONDS}s)"
    if proc.returncode != 0:
        return (
            {},
            f"unavailable (gh api graphql exit {proc.returncode}: {proc.stderr.decode(errors='replace').strip()[:200]})",
        )
    try:
        repository = json.loads(proc.stdout)["data"]["repository"]
    except (ValueError, KeyError, TypeError) as err:
        return {}, f"unavailable (unexpected graphql response: {err})"
    if repository is None:
        return {}, f"unavailable (repository {slug} not visible)"
    merged: dict[str, list[tuple[int, str]]] = {}
    for i, head in enumerate(heads):
        nodes = (repository.get(f"b{i}") or {}).get("nodes") or []
        merged[head] = [
            (n["number"], n["headRefOid"])
            for n in nodes
            if (n.get("headRepository") or {}).get("nameWithOwner", "").lower() == slug.lower()
        ]
    return merged, f"ok ({len(heads)} branches, 1 request)"


@dataclass(frozen=True)
class Integration:
    name: str
    short: str
    tip: str


LOG_PATCH = ("log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "--format=commit %H")
DIFF_PATCH = ("diff", "--no-color", "--no-ext-diff")


@contextmanager
def patch_id_stream(repo: str, *git_args: str) -> Iterator[Iterator[str]]:
    """Patch ids (`git patch-id --stable`) of `git <git_args>` output, streamed, never buffered."""
    src = subprocess.Popen(
        ["git", "-C", repo, "--no-optional-locks", *git_args], stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    ids = subprocess.Popen(["git", "patch-id", "--stable"], stdin=src.stdout, stdout=subprocess.PIPE)
    src.stdout.close()
    finished = False

    def lines() -> Iterator[str]:
        nonlocal finished
        for line in ids.stdout:
            if line.strip():
                yield line.split()[0].decode()
        finished = True

    try:
        yield lines()
    finally:
        if not finished:
            ids.kill()
            src.kill()
        ids_rc, src_rc = ids.wait(), src.wait()
        if finished and (ids_rc != 0 or src_rc != 0):
            detail = src.stderr.read().decode(errors="replace").strip()
            raise GcError(f"git {' '.join(git_args[:1])} | git patch-id failed: {detail}")


@dataclass(frozen=True)
class Landed:
    """Integration commits newer than the oldest lane merge base."""

    patch_ids: frozenset[str]
    # Changed-path sets; a squash commit touches exactly the lane's paths.
    path_sets: frozenset[frozenset[str]]


def changed_path_sets(listing: str) -> set[frozenset[str]]:
    sets, current = set(), None
    for line in listing.splitlines():
        if line.startswith("commit "):
            if current:
                sets.add(frozenset(current))
            current = []
        elif line and current is not None:
            current.append(line)
    if current:
        sets.add(frozenset(current))
    return sets


def landed_on_integration(repo: str, integ: Integration, merge_bases: set[str]) -> Landed:
    if not merge_bases:
        return Landed(frozenset(), frozenset())
    counts = {mb: int(git("rev-list", "--count", f"{mb}..{integ.tip}", cwd=repo)) for mb in merge_bases}
    revs = f"{max(counts, key=counts.__getitem__)}..{integ.tip}"
    with patch_id_stream(repo, *LOG_PATCH, revs) as ids:
        patch_ids = frozenset(ids)
    listing = git("log", "--no-merges", "--name-only", "--no-renames", "--format=commit %H", revs, cwd=repo)
    return Landed(patch_ids, frozenset(changed_path_sets(listing.decode(errors="surrogateescape"))))


def merge_base(repo: str, tip: str, head: str) -> str | None:
    return git("merge-base", tip, head, cwd=repo, check=False).decode().strip() or None


def patch_equivalence(repo: str, landed: Landed, head: str, mb: str) -> str | None:
    if git("rev-parse", f"{head}^{{tree}}", cwd=repo) == git("rev-parse", f"{mb}^{{tree}}", cwd=repo):
        return "patch (tree equals merge base)"
    lane_paths = git("diff", "--name-only", "--no-renames", mb, head, cwd=repo).decode(errors="surrogateescape")
    if frozenset(lane_paths.splitlines()) in landed.path_sets:
        with patch_id_stream(repo, *DIFF_PATCH, mb, head) as ids:
            if next(ids, None) in landed.patch_ids:
                return "patch (squash-equivalent)"
    # `git cherry` semantics, stopping at the first lane commit with no landed equivalent.
    with patch_id_stream(repo, *LOG_PATCH, f"{mb}..{head}") as ids:
        seen = False
        for pid in ids:
            if pid not in landed.patch_ids:
                return None
            seen = True
        return "patch (every commit patch-equivalent)" if seen else None


def pr_head_name(branch: str, config: dict[str, str], integ_short: str) -> str:
    merge = config.get(f"branch.{branch}.merge", "")
    if config.get(f"branch.{branch}.remote") == "origin" and merge.startswith("refs/heads/"):
        upstream = merge.removeprefix("refs/heads/")
        if upstream != integ_short:
            return upstream
    return branch


def evaluate_merged(lanes: list[Lane], repo: str, integ: Integration, slug: str | None, gh_note: str) -> str:
    candidates = [lane for lane in lanes if not lane.primary and not lane.prunable and lane.head]
    reachable = set(git("rev-list", integ.tip, cwd=repo).decode().split())
    for lane in candidates:
        if lane.head in reachable:
            lane.merged_by = "ancestor"

    config: dict[str, str] = {}
    for line in git("config", "--get-regexp", r"^branch\.", cwd=repo, check=False).decode().splitlines():
        key, _, value = line.partition(" ")
        config[key] = value
    heads = {
        lane.path: pr_head_name(lane.branch, config, integ.short)
        for lane in candidates
        if lane.branch and not lane.merged_by
    }
    if slug is None:
        github = f"unavailable ({gh_note})"
    else:
        prs, github = merged_pull_requests(slug, sorted(set(heads.values())), integ.short)
        for lane in candidates:
            for number, oid in prs.get(heads.get(lane.path, ""), []):
                if lane.head == oid or (
                    git_ok("cat-file", "-e", f"{oid}^{{commit}}", cwd=repo)
                    and git_ok("merge-base", "--is-ancestor", lane.head, oid, cwd=repo)
                ):
                    lane.merged_by = f"pr #{number}"
                    break

    # Patch equivalence costs a diff per lane (and history walks for long-lived
    # branches), so it runs only where a verdict can lead to removal.
    pending = [
        lane
        for lane in candidates
        if not lane.merged_by and not (lane.pids or lane.nested or lane.dirty or lane.status_error or lane.locked)
    ]
    with ThreadPoolExecutor(max_workers=8) as pool:
        bases = list(pool.map(lambda lane: merge_base(repo, integ.tip, lane.head), pending))
    landed = landed_on_integration(repo, integ, {mb for mb in bases if mb})

    def check(pair: tuple[Lane, str | None]) -> None:
        lane, mb = pair
        if mb is None:
            lane.unmerged_why = f"no merge base with {integ.name}"
            return
        lane.merged_by = patch_equivalence(repo, landed, lane.head, mb)
        if not lane.merged_by:
            unique = int(git("rev-list", "--count", "--no-merges", f"{mb}..{lane.head}", cwd=repo))
            lane.unmerged_why = f"{unique} commit(s) not in {integ.name} by ancestry, PR, or patch"

    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(check, zip(pending, bases, strict=True)))
    return github


def classify(lane: Lane, now: float, stale_age_hours: int) -> None:
    idle = f"idle {format_age(now - lane.last_write)}" if lane.last_write else ""
    merged = f"merged: {lane.merged_by}" if lane.merged_by else ""
    note = "; ".join(x for x in (merged, idle) if x)
    if lane.primary:
        lane.status, lane.reason = "PRIMARY", "primary checkout, never touched"
    elif lane.prunable or not os.path.isdir(lane.path):
        lane.status, lane.reason = "PRUNABLE", "directory missing; `git worktree prune` drops its metadata"
    elif lane.pids:
        shown = ",".join(map(str, lane.pids[:8])) + (f",... ({len(lane.pids)} pids)" if len(lane.pids) > 8 else "")
        lane.status, lane.reason = "ACTIVE", f"live process inside (pid {shown}); {note}"
    elif lane.nested:
        lane.status, lane.reason = "NESTED", f"contains registered worktree {lane.nested[0]}; {note}"
    elif lane.status_error:
        lane.status, lane.reason = "DIRTY", f"git status failed ({lane.status_error}); treated as dirty"
    elif lane.dirty:
        lane.status, lane.reason = "DIRTY", f"{len(lane.dirty)} uncommitted path(s), e.g. {lane.dirty[0]}; {note}"
    elif lane.locked:
        lane.status, lane.reason = "LOCKED", f"git worktree lock; {note}"
    elif not lane.merged_by:
        lane.status, lane.reason = "UNMERGED", f"{lane.unmerged_why or 'no HEAD'}; {idle}"
    elif now - lane.last_write < stale_age_hours * HOUR:
        lane.status, lane.reason = "FRESH", f"{note} (< {stale_age_hours}h)"
    else:
        lane.status, lane.reason = "MERGED", note
    lane.reason = lane.reason.rstrip("; ")


def reclaimable_builds(lane: Lane, worktrees: list[str]) -> list[str]:
    """Existing regenerable, git-ignored, untracked build dirs under the exact lane path."""
    present = []
    for rel, marker in BUILD_OUTPUTS:
        path = os.path.join(lane.path, rel)
        real = os.path.join(lane.real, rel)
        if (
            os.path.isdir(path)
            and not os.path.islink(path)
            and os.path.realpath(path) == real
            and not any(inside(real, wt) for wt in worktrees)
        ):
            present.append((rel, marker))
    if not present:
        return []
    rels = [rel for rel, _ in present]
    check_ignore = subprocess.run(["git", "-C", lane.path, "check-ignore", "--", *rels], capture_output=True)
    if check_ignore.returncode > 1:
        raise GcError(f"git check-ignore in {lane.path}: {check_ignore.stderr.decode(errors='replace').strip()}")
    ignored = set(check_ignore.stdout.decode().splitlines())
    tracked = git("ls-files", "-z", "--", *rels, *(m for _, m in present), cwd=lane.path).decode().split("\0")
    out = []
    for rel, marker in present:
        holds_tracked = any(inside(rel, t) for t in tracked if t)
        if rel in ignored and marker in tracked and not holds_tracked:
            out.append(os.path.join(lane.path, rel))
    return out


def disk_bytes(path: str) -> int:
    proc = subprocess.run(["du", "-sk", path], capture_output=True)
    out = proc.stdout.decode().split()
    if not out or not out[0].isdigit():
        raise GcError(f"du -sk {path}: {proc.stderr.decode(errors='replace').strip()}")
    return int(out[0]) * 1024


def format_age(secs: float) -> str:
    secs = max(0, int(secs))
    days, hours = divmod(secs // HOUR, 24)
    return f"{days}d{hours}h" if days else f"{hours}h"


def format_bytes(n: int) -> str:
    return f"{n / 2**30:.1f}GiB"


def remove_lane(repo: str, lane: Lane) -> str:
    """Re-verify at delete time, then remove the exact path and CAS-delete its branch."""
    if pids_inside(lane.real, live_process_paths()):
        return "skip (ACTIVE at delete time)"
    current = parse_worktrees(repo)
    now_lane = next((w for w in current if w.path == lane.path), None)
    if now_lane is None or now_lane.locked or now_lane.head != lane.head or now_lane.branch != lane.branch:
        return "skip (worktree changed since classification)"
    if any(w.path != lane.path and inside(lane.real, os.path.realpath(w.path)) for w in current):
        return "skip (NESTED at delete time)"
    if git("status", "--porcelain", "--untracked-files=normal", cwd=lane.path).strip():
        return "skip (DIRTY at delete time)"
    proc = subprocess.run(["git", "-C", repo, "worktree", "remove", "--", lane.path], capture_output=True)
    if proc.returncode != 0:
        raise GcError(f"git worktree remove {lane.path}: {proc.stderr.decode(errors='replace').strip()}")
    if lane.branch and not any(other.branch == lane.branch for other in parse_worktrees(repo)):
        if not git_ok("update-ref", "-d", f"refs/heads/{lane.branch}", lane.head, cwd=repo):
            return f"removed; kept branch {lane.branch} (moved since verification)"
        return f"removed; deleted branch {lane.branch}@{lane.head[:10]}"
    return "removed"


def reclaim(lane: Lane, build: str, idle_hours: int) -> str | None:
    if pids_inside(lane.real, live_process_paths()):
        return "skip (ACTIVE at delete time)"
    if time.time() - newest_mtime(build_dir_markers(build)) < idle_hours * HOUR:
        return "skip (written since classification)"
    proc = subprocess.run(["rm", "-rf", "--", build], capture_output=True)
    if proc.returncode != 0:
        raise GcError(f"rm -rf {build}: {proc.stderr.decode(errors='replace').strip()}")
    return None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--repo", default=None, help="any worktree of the repository (default: cwd, else this script's repo)"
    )
    parser.add_argument("--integration", default=None, help="integration branch (default: origin/HEAD, else master)")
    parser.add_argument("--delete", action="store_true", help="remove MERGED lanes (default is report only)")
    parser.add_argument(
        "--reclaim-builds",
        action="store_true",
        help="with --delete, delete regenerable build output of idle lanes; alone, report it",
    )
    parser.add_argument(
        "--stale-age-hours", type=int, default=24, help="merged lanes must be idle this long to be removed (default 24)"
    )
    parser.add_argument(
        "--idle-hours", type=int, default=24, help="lanes must be idle this long for build reclamation (default 24)"
    )
    parser.add_argument("--github-repo", default=None, help="OWNER/NAME for PR lookup (default: parsed from origin)")
    parser.add_argument("--quiet", action="store_true", help="print only the one-line summary and actions")
    args = parser.parse_args(argv)
    if args.stale_age_hours < 0 or args.idle_hours < 0:
        parser.error("hour thresholds must be non-negative")

    start = os.getcwd() if args.repo is None and git_ok("rev-parse", "--is-inside-work-tree") else args.repo
    start = start or str(Path(__file__).resolve().parent.parent)
    repo = git("rev-parse", "--show-toplevel", cwd=start).decode().strip()
    integ = Integration(*resolve_integration(repo, args.integration))
    slug, gh_note = github_repo_slug(repo, args.github_repo)

    lanes = parse_worktrees(repo)
    repo = lanes[0].path
    processes = live_process_paths()
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(inspect_lane, lanes))
    for lane in lanes:
        if not lane.primary:
            lane.pids = pids_inside(lane.real, processes)
            lane.nested = [o.path for o in lanes if o is not lane and inside(lane.real, o.real)]
    github = evaluate_merged(lanes, repo, integ, slug, gh_note)
    now = time.time()
    for lane in lanes:
        classify(lane, now, args.stale_age_hours)

    reclaim_lanes = [
        lane
        for lane in lanes
        if args.reclaim_builds
        and lane.status not in ("PRIMARY", "PRUNABLE", "ACTIVE", "MERGED")
        and now - lane.last_write >= args.idle_hours * HOUR
    ]
    worktrees = [lane.real for lane in lanes]
    with ThreadPoolExecutor(max_workers=4) as pool:
        for lane, builds in zip(
            reclaim_lanes, pool.map(lambda l: reclaimable_builds(l, worktrees), reclaim_lanes), strict=True
        ):
            lane.builds = [(b, disk_bytes(b)) for b in builds]

    counts = dict.fromkeys(STATUSES, 0)
    for lane in lanes:
        counts[lane.status] += 1
    by = {"ancestor": 0, "pr": 0, "patch": 0}
    for lane in lanes:
        if lane.merged_by:
            by[lane.merged_by.split()[0]] += 1
    mode = "delete" if args.delete else "report"
    if not args.quiet:
        print(f"worktree-gc  mode={mode}  integration={integ.name} ({integ.tip[:10]})  primary={repo}  github={github}")
        print(f"\n{'STATUS':<9} {'HEAD':<10} {'BRANCH':<40} TREE")
        for lane in lanes:
            print(f"{lane.status:<9} {(lane.head or '-')[:10]:<10} {(lane.branch or '(detached)'):<40} {lane.path}")
            print(f"{'':<10}{lane.reason}")
        if args.reclaim_builds:
            print(f"\nidle build output (no live process, no writes for {args.idle_hours}h; source never touched)")
            for lane in reclaim_lanes:
                for build, size in lane.builds:
                    print(f"  {format_bytes(size):>9}  {lane.status:<9} {build}")

    rc = 0
    removed = failed = reclaimed = 0
    freed = 0
    if args.delete:
        for lane in lanes:
            if lane.status != "MERGED":
                continue
            size = disk_bytes(lane.path)
            try:
                outcome = remove_lane(repo, lane)
            except GcError as err:
                print(f"worktree-gc: {err}", file=sys.stderr)
                failed, rc = failed + 1, 1
                continue
            if outcome.startswith("removed"):
                removed, freed = removed + 1, freed + size
            print(f"  worktree remove {lane.path} ({lane.merged_by}): {outcome}")
        if args.reclaim_builds:
            for lane in reclaim_lanes:
                for build, size in lane.builds:
                    try:
                        skipped = reclaim(lane, build, args.idle_hours)
                    except GcError as err:
                        print(f"worktree-gc: {err}", file=sys.stderr)
                        failed, rc = failed + 1, 1
                        continue
                    if skipped is None:
                        reclaimed, freed = reclaimed + 1, freed + size
                    print(f"  reclaim {build} ({format_bytes(size)}): {skipped or 'deleted'}")
        prune = subprocess.run(["git", "-C", repo, "worktree", "prune"], capture_output=True)
        if prune.returncode != 0:
            print(f"worktree-gc: git worktree prune: {prune.stderr.decode(errors='replace').strip()}", file=sys.stderr)
            failed, rc = failed + 1, 1

    reclaimable = sum(size for lane in reclaim_lanes for _, size in lane.builds)
    status_counts = " ".join(f"{s.lower()}={n}" for s, n in counts.items())
    superseded = sum(1 for lane in lanes if lane.merged_by)
    line = (
        f"worktree-gc mode={mode} lanes={len(lanes)} {status_counts} "
        f"merged-evidence={superseded} (ancestor={by['ancestor']} pr={by['pr']} patch={by['patch']}) "
        f"idle-builds={sum(len(lane.builds) for lane in reclaim_lanes)}/{format_bytes(reclaimable)}"
    )
    if args.delete:
        line += f" removed={removed} reclaimed={reclaimed} freed={format_bytes(freed)} failed={failed}"
    print(("" if args.quiet else "\n") + line + f" github={github.split(' (')[0]}")
    return rc


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except GcError as err:
        print(f"worktree-gc: {err}", file=sys.stderr)
        sys.exit(1)
