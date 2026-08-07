#!/usr/bin/env python3
"""Enforce the release workflow's isolated TypeScript SDK npm publication boundary."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import yaml

WORKFLOW_PATH = Path(__file__).resolve().parents[1] / ".github/workflows/release.yml"
SHA_RE = re.compile(r"^[^@]+@[0-9a-f]{40}(\s*#.*)?$")
REPO_GUARD = "github.repository == 'ScriptedAlchemy/tracedecay'"
VALIDATE_JOB = "validate-release"
VERIFY_JOB = "verify-release"
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
    print(f"release.yml SDK publication policy violation: {message}", file=sys.stderr)
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


def assert_repository_guard(name: str, job: dict[str, Any]) -> None:
    condition = job.get("if")
    if condition != REPO_GUARD:
        fail(f"'{name}' must have exact guard {REPO_GUARD!r}, found {condition!r}")


def assert_actions_pinned(name: str, job: dict[str, Any]) -> None:
    for step in job_steps(job):
        uses = step.get("uses")
        if isinstance(uses, str) and not SHA_RE.fullmatch(uses):
            fail(f"'{name}' uses unpinned action {uses!r}")


def assert_release_trigger(workflow: dict[str, Any]) -> None:
    triggers = workflow.get("on", workflow.get(True, {}))
    if not isinstance(triggers, dict) or set(triggers) != {"release", "workflow_dispatch"}:
        fail("npm publication must ride the GitHub Release trigger plus tag recovery only")
    release = triggers.get("release")
    if not isinstance(release, dict) or release.get("types") != ["published"]:
        fail("the release trigger must fire on published releases only")
    dispatch = triggers.get("workflow_dispatch")
    inputs = dispatch.get("inputs") if isinstance(dispatch, dict) else None
    if not isinstance(inputs, dict) or set(inputs) != {"release_tag"}:
        fail("manual dispatch is release-tag recovery only; no SDK selector is allowed")


def assert_build_job(job: dict[str, Any]) -> None:
    if job.get("needs") != VALIDATE_JOB:
        fail(f"'{BUILD_JOB}' must depend on '{VALIDATE_JOB}' only")
    if job.get("permissions") != {"contents": "read"}:
        fail(f"'{BUILD_JOB}' must grant contents: read only")
    if "environment" in job:
        fail(f"'{BUILD_JOB}' must not hold protected-environment authority")

    steps = job_steps(job)
    for required in (
        "npm install -g npm@12.0.2",
        "npm ci",
        "scripts/check-sdk-codegen.sh",
        "npm run typecheck",
        "npm test",
        "npm pack --dry-run --json --ignore-scripts",
        "npm pack --json --ignore-scripts",
        "npm pack npm@12.0.2 --ignore-scripts",
        "5dbb86c71d07a1957f2e90734092dd6a58bdcd9ebc2d8d41ca1c6e6a21d364e1",
        "TRACEDECAY_SDK_TARBALL",
        "sha256sum --",
        "actions/upload-artifact@",
    ):
        if find_step(steps, required) is None:
            fail(f"'{BUILD_JOB}' is missing {required!r}")

    parity_index = find_step(steps, "scripts/check-sdk-codegen.sh")
    typecheck_index = find_step(steps, "npm run typecheck")
    tests_index = find_step(steps, "npm test")
    dry_run_index = find_step(steps, "npm pack --dry-run --json --ignore-scripts")
    pack_index = find_step(steps, "npm pack --json --ignore-scripts")
    conformance_index = find_step(steps, "TRACEDECAY_SDK_TARBALL")
    upload_index = find_step(steps, "actions/upload-artifact@")
    if not (
        isinstance(parity_index, int)
        and isinstance(typecheck_index, int)
        and isinstance(tests_index, int)
        and isinstance(dry_run_index, int)
        and isinstance(pack_index, int)
        and isinstance(conformance_index, int)
        and isinstance(upload_index, int)
        and parity_index < typecheck_index
        and parity_index < tests_index
        and tests_index < dry_run_index < pack_index < conformance_index < upload_index
    ):
        fail(
            "SDK registry-client parity, SDK tests, package dry-run, exact packing, "
            "conformance, and upload must run in fail-closed order"
        )


def assert_publish_job(job: dict[str, Any]) -> None:
    needs = job.get("needs")
    if not isinstance(needs, list) or set(needs) != {VALIDATE_JOB, BUILD_JOB, VERIFY_JOB}:
        fail(
            f"'{PUBLISH_JOB}' must depend on exactly "
            f"'{VALIDATE_JOB}', '{BUILD_JOB}', and '{VERIFY_JOB}'"
        )
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

    publish_step = steps[publish_index]

    # Trusted publishing is tokenless: any configured registry credential
    # would shadow the npm CLI's OIDC exchange.
    job_text = str(job)
    for forbidden_credential in ("NPM_TOKEN", "NODE_AUTH_TOKEN", "_authToken", ".npmrc"):
        if forbidden_credential in job_text:
            fail(
                f"'{PUBLISH_JOB}' must stay tokenless for OIDC trusted publishing; "
                f"found {forbidden_credential!r}"
            )

    publish_command = str(publish_step.get("run", ""))
    for required in (
        "npm-12.0.2.tgz",
        "node npm-cli/package/bin/npm-cli.js",
        'publish "$tarball"',
        "--access public",
        '--tag "$dist_tag"',
        "dist.integrity",
        "trusted publisher",
    ):
        if required not in publish_command:
            fail(f"'{PUBLISH_JOB}' publish command is missing {required!r}")

    commands = "\n".join(str(step.get("run", "")) for step in steps)
    if "npm install" in commands or "npx " in commands:
        fail(f"'{PUBLISH_JOB}' must not install executable packages with publish authority")

    for step in steps:
        uses = step.get("uses")
        if isinstance(uses, str) and not uses.startswith(
            ("actions/download-artifact@", "actions/setup-node@")
        ):
            fail(f"'{PUBLISH_JOB}' uses unnecessary privileged action {uses!r}")
        if "registry-url" in step.get("with", {}):
            fail(
                f"'{PUBLISH_JOB}' must not configure setup-node registry auth; "
                "it would shadow the OIDC exchange"
            )
        command = str(step.get("run", ""))
        if command and "sha256sum -c" not in command and step is not publish_step:
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
    assert_release_trigger(workflow)

    if workflow.get("permissions") != {"contents": "read"}:
        fail("top-level permissions must grant contents: read only")

    jobs = workflow.get("jobs", {})
    if not isinstance(jobs, dict) or not {BUILD_JOB, PUBLISH_JOB} <= set(jobs):
        fail("workflow must contain the TypeScript build and npm publish jobs")
    for gate in (VALIDATE_JOB, VERIFY_JOB):
        if gate not in jobs:
            fail(f"workflow must retain the '{gate}' release verification job")
    build = jobs[BUILD_JOB]
    publish = jobs[PUBLISH_JOB]
    if not isinstance(build, dict) or not isinstance(publish, dict):
        fail("workflow jobs must be mappings")

    for name, job in ((BUILD_JOB, build), (PUBLISH_JOB, publish)):
        assert_repository_guard(name, job)
        assert_actions_pinned(name, job)
    assert_build_job(build)
    assert_publish_job(publish)

    print(
        "release.yml isolates npm publish authority and exact artifact bytes "
        "behind the release verification and SDK readiness gates."
    )


if __name__ == "__main__":
    main()
