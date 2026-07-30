#!/usr/bin/env python3
"""Structural policy gate for .github/workflows/sdk-publish.yml.

This does not execute a live OIDC publish (that requires real trusted
publishers registered on npm/PyPI and is out of scope for CI/local runs).
Instead it asserts, from the workflow's own YAML, the invariants a Sol
review found missing: OIDC authority isolated from build/test code,
publishing restricted to reviewed `master` commits, and the publish job
verifying a digest recorded by an unprivileged build job before publishing.

Exit non-zero with a descriptive message on any violation.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import yaml

WORKFLOW_PATH = Path(__file__).resolve().parents[1] / ".github/workflows/sdk-publish.yml"
SHA_RE = re.compile(r"^[^@]+@[0-9a-f]{40}(\s*#.*)?$")

# (build_job, publish_job) pairs that must satisfy the authority-separation
# contract below.
JOB_PAIRS = [
    ("build-typescript", "publish-typescript"),
    ("build-python", "publish-python"),
]


def fail(message: str) -> None:
    print(f"sdk-publish.yml policy violation: {message}", file=sys.stderr)
    sys.exit(1)


def job_permissions(job: dict[str, Any]) -> dict[str, Any]:
    permissions = job.get("permissions", {})
    return permissions if isinstance(permissions, dict) else {}


def job_steps(job: dict[str, Any]) -> list[dict[str, Any]]:
    steps = job.get("steps", [])
    return [step for step in steps if isinstance(step, dict)]


def assert_build_job_unprivileged(name: str, job: dict[str, Any]) -> None:
    permissions = job_permissions(job)
    if permissions.get("id-token") == "write":
        fail(f"'{name}' must not hold id-token: write (build/test must be unprivileged)")
    if "environment" in job:
        fail(f"'{name}' must not have a protected environment (that belongs to the publish job)")
    for step in job_steps(job):
        uses = step.get("uses")
        if isinstance(uses, str) and uses.startswith(("pypa/gh-action-pypi-publish", "actions/setup-node")):
            if "registry-url" in step.get("with", {}):
                fail(f"'{name}' step {step.get('name')!r} configures a registry-url; that belongs to the publish job")


def assert_publish_job_authority(name: str, job: dict[str, Any], needs: str) -> None:
    permissions = job_permissions(job)
    if permissions.get("id-token") != "write":
        fail(f"'{name}' must declare permissions.id-token: write")
    if "environment" not in job:
        fail(f"'{name}' must run under a protected GitHub environment")
    condition = job.get("if", "")
    if "refs/heads/master" not in condition:
        fail(f"'{name}' must gate on github.ref == 'refs/heads/master', found: {condition!r}")
    job_needs = job.get("needs")
    needed = [job_needs] if isinstance(job_needs, str) else (job_needs or [])
    if needs not in needed:
        fail(f"'{name}' must declare needs: {needs}")

    steps = job_steps(job)
    digest_step_index = next(
        (i for i, step in enumerate(steps) if "sha256sum -c" in str(step.get("run", ""))),
        None,
    )
    if digest_step_index is None:
        fail(f"'{name}' must verify a recorded artifact digest with sha256sum -c before publishing")

    publish_step_index = next(
        (
            i
            for i, step in enumerate(steps)
            if "npm publish" in str(step.get("run", ""))
            or str(step.get("uses", "")).startswith("pypa/gh-action-pypi-publish")
        ),
        None,
    )
    if publish_step_index is None:
        fail(f"'{name}' has no recognizable publish step")
    if publish_step_index < digest_step_index:
        fail(f"'{name}' publishes before verifying the artifact digest")

    for step in steps:
        uses = step.get("uses")
        if isinstance(uses, str) and not SHA_RE.match(uses):
            fail(f"'{name}' step uses an unpinned action reference: {uses!r} (pin to a full commit SHA)")


def main() -> None:
    workflow = yaml.safe_load(WORKFLOW_PATH.read_text(encoding="utf-8"))
    jobs = workflow.get("jobs", {})

    # PyYAML parses the bare `on:` key as the boolean True (YAML 1.1), not
    # the string "on" -- look it up defensively either way.
    triggers = workflow.get("on", workflow.get(True, {}))
    dispatch = triggers.get("workflow_dispatch")
    if dispatch is None:
        fail("workflow must remain manually dispatched, not automatically triggered")

    top_permissions = workflow.get("permissions", {})
    if top_permissions.get("id-token") == "write":
        fail("top-level permissions must not grant id-token: write; scope it to publish jobs only")

    for build_name, publish_name in JOB_PAIRS:
        build_job = jobs.get(build_name)
        publish_job = jobs.get(publish_name)
        if build_job is None:
            fail(f"missing expected unprivileged build job: {build_name}")
        if publish_job is None:
            fail(f"missing expected protected publish job: {publish_name}")
        assert_build_job_unprivileged(build_name, build_job)
        assert_publish_job_authority(publish_name, publish_job, needs=build_name)

    print("sdk-publish.yml satisfies the authority-separation and master-only publish policy.")


if __name__ == "__main__":
    main()
