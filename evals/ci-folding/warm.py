#!/usr/bin/env python3
"""Compare fresh targets with a warm, sequential Hauler lane on TraceDecay.

This is a local/Actions experiment, not a check-result cache or PR scheduler.
The caller supplies a trusted TraceDecay checkout with pnpm install completed.
Only this probe's temporary worktree and targets are changed or removed.
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


def measure(worker, target, sha, mode):
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL="0",
               CARGO_HAULER_STATE_DIR=str(worker.parent / "state"),
               CARGO_PROFILE_DEV_DEBUG="0", CARGO_PROFILE_TEST_DEBUG="0",
               RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="",
               CARGO_BUILD_RUSTC_WRAPPER="", CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER="")
    command = ["hauler", "exec", "--session", "ci-folding-probe", "--cwd", str(worker),
               "--", "cargo", "test", "--locked", "-p", "tracedecay-framing", "--lib",
               "--message-format=json", "--", "--test-threads=2"]
    started = time.monotonic()
    result = subprocess.run(command, env=env, text=True, capture_output=True)
    seconds = time.monotonic() - started
    artifacts = []
    for line in result.stdout.splitlines():
        if line.startswith('{"reason":'):
            message = json.loads(line)
            if message["reason"] == "compiler-artifact":
                artifacts.append(message)
    tests = re.findall(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed", result.stdout)
    row = dict(mode=mode, sha=sha, seconds=round(seconds, 3), exit_code=result.returncode,
               compiled=sum(not a["fresh"] for a in artifacts),
               fresh=sum(a["fresh"] for a in artifacts),
               passed=sum(int(p) for p, _ in tests), failed=sum(int(f) for _, f in tests))
    print(json.dumps(row), flush=True)
    if not tests or (mode != "negative-control" and result.returncode != 0):
        raise RuntimeError(f"Probe did not pass a nonempty test run: {row}\n{result.stderr[-8000:]}\n{result.stdout[-4000:]}")
    if mode == "negative-control" and (result.returncode == 0 or row["failed"] == 0):
        raise RuntimeError("Changed failing source was incorrectly accepted")
    return row


def probe(repo, head):
    # Resolve before any checkout so moving branch names cannot change the experiment.
    head = git(repo, "rev-parse", "--verify", "--end-of-options", head + "^{commit}")
    refs = list(reversed(git(repo, "rev-list", "--first-parent", "--max-count=3", head).splitlines()))
    if len(refs) != 3:
        raise ValueError("The probe needs three commits of first-parent history")
    if not (repo / ".pnpm/crates").is_dir():
        raise ValueError("Run pnpm install in the source checkout before this probe")
    # Source vending is shared read-only. Comparing across a lock/config/toolchain
    # change needs a separate install; refuse that case instead of using wrong deps.
    inputs = ["Cargo.lock", "pnpm-lock.yaml", ".cargo/config.toml", "rust-toolchain.toml"]
    if any(git(repo, "diff", "--name-only", sha, "--", *inputs) for sha in refs):
        raise ValueError("Choose three commits with unchanged dependency and toolchain inputs")
    rows = []
    root = Path(tempfile.mkdtemp(prefix="hauler-ci-folding-"))
    worker = root / "worker"
    git(repo, "worktree", "add", "--detach", str(worker), refs[0])
    try:
        (worker / ".pnpm").symlink_to((repo / ".pnpm").resolve(), target_is_directory=True)
        # ponytail: one sequential lane; add parallel lanes only after measuring
        # throughput. Never put two worktrees on this mutable target directory.
        for mode in ("fresh", "warm"):
            for index, sha in enumerate(refs):
                git(worker, "checkout", "--detach", sha)
                target = root / (f"fresh-{index}" if mode == "fresh" else "warm")
                rows.append(measure(worker, target, sha, mode))
        source = worker / "crates/tracedecay-framing/src/lib.rs"
        with source.open("a") as stream:
            stream.write('\n#[cfg(test)]\n#[test]\nfn ci_folding_negative_control() { assert!(false, "fresh source must fail"); }\n')
        control = measure(worker, root / "warm", refs[-1] + "+failing-test", "negative-control")
    finally:
        stopped = subprocess.run(["hauler", "daemon", "stop"],
                                 env=dict(os.environ, CARGO_HAULER_STATE_DIR=str(root / "state")),
                                 text=True, capture_output=True)
        if stopped.returncode:
            raise RuntimeError(f"Probe daemon did not stop; retained {root}: {stopped.stdout}{stopped.stderr}")
        # This exact path belongs to the probe; never select worktrees by prefix.
        git(repo, "worktree", "remove", "--force", str(worker))
        shutil.rmtree(root)
    fresh = sum(row["seconds"] for row in rows if row["mode"] == "fresh")
    warm = sum(row["seconds"] for row in rows if row["mode"] == "warm")
    return dict(repository=str(repo), head=refs[-1], command="cargo test --locked -p tracedecay-framing --lib",
                hauler=subprocess.check_output(["hauler", "--version"], text=True).strip(),
                rustc=subprocess.check_output(["rustc", "-Vv"], cwd=repo, text=True).strip(),
                rows=rows, negative_control=control, fresh_seconds=round(fresh, 3),
                warm_seconds=round(warm, 3), speedup=round(fresh / warm, 3))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repository", type=Path)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = probe(args.repository.resolve(), args.head)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
