#!/usr/bin/env python3
"""Enforce crates.io/npm-only, authority-separated SDK publication."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import yaml

WORKFLOW_PATH = Path(__file__).resolve().parents[1] / ".github/workflows/sdk-publish.yml"
SHA_RE = re.compile(r"^[^@]+@[0-9a-f]{40}(\s*#.*)?$")
MASTER_GUARD = (
    "github.repository == 'ScriptedAlchemy/tracedecay' "
    "&& github.ref == 'refs/heads/master'"
)
BUILD_JOB = "build-typescript"
PUBLISH_JOB = "publish-typescript"
FORBIDDEN_PYTHON_PUBLICATION = (
    "pypi",
    "publish-python",
    "build-python",
    "gh-action-pypi-publish",
    "twine",
    "sdist",
    ".whl",
)


def fail(message: str) -> None:
    print(f"sdk-publish.yml policy violation: {message}", file=sys.stderr)
    raise SystemExit(1)


def job_steps(job: dict[str, Any]) -> list[dict[str, Any]]:
    steps = job.get("steps", [])
    return [step for step in steps if isinstance(step, dict)]


def find_step(steps: list[dict[str, Any]], fragment: str) -> int | None:
    return next(
        (
            index
            for index, step in enumerate(steps)
            if fragment in str(step)
        ),
        None,
    )


def assert_master_only(name: str, job: dict[str, Any]) -> None:
    condition = job.get("if")
    if condition != MASTER_GUARD:
        fail(f"'{name}' must have exact guard {MASTER_GUARD!r}, found {condition!r}")


def assert_actions_pinned(name: str, job: dict[str, Any]) -> None:
    for step in job_steps(job):
        uses = step.get("uses")
        if isinstance(uses, str) and not SHA_RE.fullmatch(uses):
            fail(f"'{name}' uses unpinned action {uses!r}")


def assert_build_job(job: dict[str, Any]) -> None:
    if job.get("permissions") != {"contents": "read"}:
        fail(f"'{BUILD_JOB}' must grant contents: read only")
    if "environment" in job:
        fail(f"'{BUILD_JOB}' must not hold protected-environment authority")

    steps = job_steps(job)
    for required in (
        "npm install -g npm@12.0.2",
        "npm ci",
        "npm run typecheck",
        "npm test",
        "npm pack --json --ignore-scripts",
        "npm pack npm@12.0.2 --ignore-scripts",
        "5dbb86c71d07a1957f2e90734092dd6a58bdcd9ebc2d8d41ca1c6e6a21d364e1",
        "TRACEDECAY_SDK_TARBALL",
        "sha256sum --",
        "actions/upload-artifact@",
    ):
        if find_step(steps, required) is None:
            fail(f"'{BUILD_JOB}' is missing {required!r}")

    pack_index = find_step(steps, "npm pack --json --ignore-scripts")
    conformance_index = find_step(steps, "TRACEDECAY_SDK_TARBALL")
    upload_index = find_step(steps, "actions/upload-artifact@")
    if not (
        isinstance(pack_index, int)
        and isinstance(conformance_index, int)
        and isinstance(upload_index, int)
        and pack_index < conformance_index < upload_index
    ):
        fail("the exact npm tarball must be packed, conformance-tested, then uploaded")


def assert_publish_job(job: dict[str, Any]) -> None:
    if job.get("needs") != BUILD_JOB:
        fail(f"'{PUBLISH_JOB}' must depend on '{BUILD_JOB}'")
    if job.get("environment") != "npm-tracedecay-sdk":
        fail(f"'{PUBLISH_JOB}' must use the protected npm-tracedecay-sdk environment")
    if job.get("permissions") != {"contents": "read", "id-token": "write"}:
        fail(f"'{PUBLISH_JOB}' must hold only contents: read and id-token: write")

    steps = job_steps(job)
    digest_index = find_step(steps, "sha256sum -c")
    publish_index = find_step(steps, "npm-cli/package/bin/npm-cli.js")
    if not (
        isinstance(digest_index, int)
        and isinstance(publish_index, int)
        and digest_index < publish_index
    ):
        fail("the downloaded artifacts must be digest-verified before npm publication")

    publish_command = str(steps[publish_index].get("run", ""))
    for required in (
        "npm-12.0.2.tgz",
        "node npm-cli/package/bin/npm-cli.js",
        'publish "$tarball"',
        "--provenance",
        "--access public",
    ):
        if required not in publish_command:
            fail(f"'{PUBLISH_JOB}' publish command is missing {required!r}")

    commands = "\n".join(str(step.get("run", "")) for step in steps)
    if "npm install" in commands or "npx " in commands:
        fail(f"'{PUBLISH_JOB}' must not install executable packages with OIDC authority")

    for step in steps:
        uses = step.get("uses")
        if isinstance(uses, str) and not uses.startswith(
            ("actions/download-artifact@", "actions/setup-node@")
        ):
            fail(f"'{PUBLISH_JOB}' uses unnecessary privileged action {uses!r}")
        if "registry-url" in step.get("with", {}):
            fail(f"'{PUBLISH_JOB}' must not configure token-based npm authentication")
        command = str(step.get("run", ""))
        if command and "sha256sum -c" not in command and step is not steps[publish_index]:
            fail(f"'{PUBLISH_JOB}' runs unnecessary privileged setup code {command!r}")


def main() -> None:
    text = WORKFLOW_PATH.read_text(encoding="utf-8")
    lowered = text.lower()
    for forbidden in FORBIDDEN_PYTHON_PUBLICATION:
        if forbidden in lowered:
            fail(
                "Python is source/local-conformance only; "
                f"found forbidden publication term {forbidden!r}"
            )

    workflow = yaml.safe_load(text)
    triggers = workflow.get("on", workflow.get(True, {}))
    if not isinstance(triggers, dict) or set(triggers) != {"workflow_dispatch"}:
        fail("publication must be manually dispatched only")
    dispatch = triggers.get("workflow_dispatch")
    if not isinstance(dispatch, dict) or dispatch:
        fail("npm is the only SDK registry authority; no SDK selector is allowed")

    if workflow.get("permissions") != {"contents": "read"}:
        fail("top-level permissions must grant contents: read only")

    jobs = workflow.get("jobs", {})
    if not isinstance(jobs, dict) or set(jobs) != {BUILD_JOB, PUBLISH_JOB}:
        fail("workflow must contain only TypeScript build and npm publish jobs")
    build = jobs[BUILD_JOB]
    publish = jobs[PUBLISH_JOB]
    if not isinstance(build, dict) or not isinstance(publish, dict):
        fail("workflow jobs must be mappings")

    for name, job in ((BUILD_JOB, build), (PUBLISH_JOB, publish)):
        assert_master_only(name, job)
        assert_actions_pinned(name, job)
    assert_build_job(build)
    assert_publish_job(publish)

    print(
        "sdk-publish.yml satisfies crates.io/npm-only authority separation "
        "and exact-artifact publication."
    )


if __name__ == "__main__":
    main()
