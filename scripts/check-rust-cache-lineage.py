#!/usr/bin/env python3
"""Enforce rust-cache restore lineage on the hosted hotpath lanes.

Those lanes used to restore ``v0-rust-`` prefix matches (often another
lockfile generation, or a kache-era blob) and then fail to overwrite
rustc's read-only ``.rmeta`` files. The contract is: a unique shared-key
under ``v1-rust``, dependency artifacts only, and drop ``target/`` unless
the restore was an exact cache hit. No chmod, no skipped jobs.

Stdlib only: the pull-request cache-hygiene job checkouts scripts and
workflows onto a bare runner.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_cache_lineage import PREFIX_KEY

WORKFLOWS = Path(__file__).resolve().parents[1] / ".github" / "workflows"
DROP_SCRIPT = "scripts/drop-prefix-restored-rust-artifacts.sh"

# job id → required shared-key. These lanes compile after a rust-cache restore
# and previously inherited incompatible read-only compiler outputs.
HOSTED_COMPILER_LANES: dict[str, dict[str, str]] = {
    "hotpath-coverage.yml": {
        "slice-tests": "hotpath-coverage-slice",
        "sessions-slice-tests": "hotpath-coverage-sessions",
    },
    "hotpath-runtime-core.yml": {
        "git-authority": "hotpath-runtime-core",
    },
    "hotpath-profile.yml": {
        "profile": "hotpath-profile",
    },
}

JOB_HEADER = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$", re.MULTILINE)
RUST_CACHE_USES = re.compile(r"^\s+uses: Swatinem/rust-cache@", re.MULTILINE)


def fail(message: str) -> None:
    print(f"rust-cache lineage policy violation: {message}", file=sys.stderr)
    raise SystemExit(1)


def job_bodies(text: str) -> dict[str, str]:
    """Split a workflow into job-id → body (text until the next top-level job)."""
    matches = list(JOB_HEADER.finditer(text))
    jobs_marker = text.find("\njobs:\n")
    if jobs_marker < 0:
        fail("workflow has no jobs:")
    start = jobs_marker + len("\njobs:\n")
    bodies: dict[str, str] = {}
    job_matches = [match for match in matches if match.start() >= start]
    for index, match in enumerate(job_matches):
        end = job_matches[index + 1].start() if index + 1 < len(job_matches) else len(text)
        bodies[match.group(1)] = text[match.start() : end]
    return bodies


def assert_hosted_job(workflow: str, job_id: str, shared_key: str, body: str) -> None:
    caches = list(RUST_CACHE_USES.finditer(body))
    if len(caches) != 1:
        fail(f"{workflow}:{job_id} must have exactly one rust-cache step, found {len(caches)}")
    if f"prefix-key: {PREFIX_KEY}" not in body:
        fail(
            f"{workflow}:{job_id} rust-cache prefix-key must be {PREFIX_KEY!r} "
            f"so v0-rust blobs are never prefix-restored"
        )
    if f"shared-key: {shared_key}" not in body:
        fail(f"{workflow}:{job_id} rust-cache shared-key must be {shared_key!r}")
    if 'cache-workspace-crates: "false"' not in body and "cache-workspace-crates: false" not in body:
        fail(f"{workflow}:{job_id} must set cache-workspace-crates: false")
    if "id: rust-cache" not in body:
        fail(f"{workflow}:{job_id} rust-cache step must have id: rust-cache")
    drop = f'{DROP_SCRIPT} "${{{{ steps.rust-cache.outputs.cache-hit }}}}"'
    if drop not in body:
        fail(f"{workflow}:{job_id} must drop target/ after a rust-cache prefix restore via {DROP_SCRIPT}")


def main() -> int:
    seen_shared_keys: dict[str, str] = {}
    for filename, jobs in HOSTED_COMPILER_LANES.items():
        path = WORKFLOWS / filename
        if not path.is_file():
            fail(f"missing workflow {filename}")
        text = path.read_text(encoding="utf-8")
        if "CARGO_INCREMENTAL: \"0\"" not in text and "CARGO_INCREMENTAL: '0'" not in text:
            fail(f"{filename} must set CARGO_INCREMENTAL=0 so rustc does not update .rmeta in place")
        bodies = job_bodies(text)
        for job_id, shared_key in jobs.items():
            body = bodies.get(job_id)
            if body is None:
                fail(f"{filename} is missing job {job_id}")
            assert_hosted_job(filename, job_id, shared_key, body)
            owner = f"{filename}:{job_id}"
            previous = seen_shared_keys.get(shared_key)
            if previous is not None:
                fail(f"shared-key {shared_key!r} is reused by {previous} and {owner}")
            seen_shared_keys[shared_key] = owner
        if filename == "hotpath-profile.yml":
            if "CARGO_TARGET_DIR: target-base" not in text:
                fail(
                    "hotpath-profile base compile must use CARGO_TARGET_DIR=target-base "
                    "so it cannot overwrite HEAD's restored read-only .rmeta"
                )
            if "name: Base profile (timing)" not in text:
                fail("hotpath-profile.yml is missing the base profile compile step")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
