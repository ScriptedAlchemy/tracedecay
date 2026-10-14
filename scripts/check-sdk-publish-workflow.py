#!/usr/bin/env python3
"""Enforce the release workflow's isolated TypeScript SDK npm publication boundary."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_PATH = REPOSITORY_ROOT / ".github/workflows/release.yml"
SHA_RE = re.compile(r"^[^@]+@[0-9a-f]{40}(\s*#.*)?$")
LOCAL_HELPER_RE = re.compile(r"^\./\.github/actions/[A-Za-z0-9_-]+$")
SOURCE_SHA_REF = "${{ needs.validate-release.outputs.source_sha }}"
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


def resolve_local_helpers(
    steps: list[dict[str, Any]], helper: str | None = None
) -> list[dict[str, Any]]:
    """Inline reviewed composite helpers so their steps face the build policy.

    A local helper runs from the job's checkout, so it is only as immutable as
    that checkout; assert_immutable_checkout pins it to the validated source.
    """
    resolved: list[dict[str, Any]] = []
    for step in steps:
        uses = step.get("uses")
        if not (isinstance(uses, str) and uses.startswith("./")):
            if isinstance(uses, str) and not SHA_RE.fullmatch(uses):
                via = f" helper {helper!r}" if helper else ""
                fail(f"'{BUILD_JOB}'{via} uses unpinned action {uses!r}")
            resolved.append(step)
            continue
        if not LOCAL_HELPER_RE.fullmatch(uses):
            fail(f"'{BUILD_JOB}' helper {uses!r} must be a directory under ./.github/actions")
        action = yaml.safe_load(
            (REPOSITORY_ROOT / uses / "action.yml").read_text(encoding="utf-8")
        )
        runs = action.get("runs") if isinstance(action, dict) else None
        if not isinstance(runs, dict) or runs.get("using") != "composite":
            fail(f"'{BUILD_JOB}' helper {uses!r} must be a composite action")
        helper_steps = [s for s in runs.get("steps", []) if isinstance(s, dict)]
        resolved.extend(resolve_local_helpers(helper_steps, uses))
    return resolved


def assert_immutable_checkout(steps: list[dict[str, Any]]) -> None:
    checkouts = [
        index
        for index, step in enumerate(steps)
        if str(step.get("uses", "")).startswith("actions/checkout@")
    ]
    checkout = steps[0].get("with", {}) if checkouts == [0] else {}
    if checkout != {"ref": SOURCE_SHA_REF, "persist-credentials": False}:
        fail(
            f"'{BUILD_JOB}' must first and only check out the validated source "
            "SHA without persisted credentials"
        )


def assert_master_dispatch_trigger(workflow: dict[str, Any]) -> None:
    triggers = workflow.get("on", workflow.get(True, {}))
    # A run whose ref is the release tag cannot restore the previous release's
    # Actions cache. release-please dispatches this workflow on master after
    # the immutable tag exists, so a `release` trigger is not an allowed path.
    if not isinstance(triggers, dict) or set(triggers) != {"workflow_dispatch"}:
        fail(
            "npm publication must be dispatched on master after the immutable "
            "tag exists; a tag-ref run cannot restore the release cache"
        )
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

    steps = resolve_local_helpers(job_steps(job))
    assert_immutable_checkout(steps)
    policy_text = f"{job}{steps}"
    if "secrets." in policy_text or "github.token" in policy_text:
        fail(f"'{BUILD_JOB}' must not receive repository credentials")
    for required in (
        "npm install -g npm@12.2.0",
        "pnpm install --frozen-lockfile",
        "pnpm run contracts:check",
        "pnpm run typecheck",
        "pnpm test",
        "npm pack --dry-run --json --ignore-scripts",
        "npm pack --json --ignore-scripts",
        "npm pack npm@12.2.0 --ignore-scripts",
        "6666b48816b39b86c3febac7b51a4ee4de6c5ca589c382ad8004b6b113f86677",
        "TRACEDECAY_SDK_TARBALL",
        "sha256sum --",
        "actions/upload-artifact@",
    ):
        if find_step(steps, required) is None:
            fail(f"'{BUILD_JOB}' is missing {required!r}")

    parity_index = find_step(steps, "pnpm run contracts:check")
    typecheck_index = find_step(steps, "pnpm run typecheck")
    tests_index = find_step(steps, "pnpm test")
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
        "npm-12.2.0.tgz",
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
        if isinstance(uses, str) and not SHA_RE.fullmatch(uses):
            fail(f"'{PUBLISH_JOB}' uses unpinned action {uses!r}")
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
    assert_master_dispatch_trigger(workflow)

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
    assert_build_job(build)
    assert_publish_job(publish)

    print(
        "release.yml isolates npm publish authority and exact artifact bytes "
        "behind the release verification and SDK readiness gates."
    )


if __name__ == "__main__":
    main()
