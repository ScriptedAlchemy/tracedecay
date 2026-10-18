#!/usr/bin/env python3
"""Check the SDK release trust boundary, not the commands executed by its build.

Readiness is established by the build job's real checks and failure propagation.
This check covers immutable source/actions, credentials, protected publication,
and the immutable artifact ID connecting those two jobs.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any

import yaml

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_PATH = REPOSITORY_ROOT / ".github/workflows/release.yml"
SOURCE_SHA_REF = "${{ needs.validate-release.outputs.source_sha }}"
REPO_GUARD = "github.repository == 'ScriptedAlchemy/tracedecay'"
BUILD_JOB = "build-typescript"
PUBLISH_JOB = "publish-typescript"


def fail(message: str) -> None:
    raise ValueError(message)


def steps(job: dict[str, Any]) -> list[dict[str, Any]]:
    return job.get("steps", [])


def actions(job_steps: list[dict[str, Any]], action: str) -> list[dict[str, Any]]:
    return [step for step in job_steps if step.get("uses", "").startswith(action + "@")]


def reviewed_steps(job_steps: list[dict[str, Any]], parents: tuple[str, ...] = ()) -> list[dict[str, Any]]:
    """Inspect local composites from the immutable checkout, including nested pins."""
    result = []
    for step in job_steps:
        uses = step.get("uses", "")
        if uses.startswith("./"):
            if not re.fullmatch(r"\./\.github/actions/[A-Za-z0-9_-]+", uses) or uses in parents:
                fail(f"invalid or recursive local action: {uses}")
            helper = yaml.safe_load((REPOSITORY_ROOT / uses / "action.yml").read_text())
            if helper["runs"]["using"] != "composite":
                fail(f"local action must be a reviewed composite: {uses}")
            result.extend(reviewed_steps(helper["runs"]["steps"], (*parents, uses)))
        else:
            if uses and not re.fullmatch(r"[^@]+@[0-9a-f]{40}", uses):
                fail(f"action must be pinned to a commit: {uses}")
            result.append(step)
    return result


def validate(workflow: dict[str, Any]) -> None:
    if workflow.get("permissions") != {"contents": "read"}:
        fail("workflow must grant contents: read only")
    triggers = workflow.get("on", workflow.get(True, {}))
    if not isinstance(triggers, dict) or set(triggers) != {"workflow_dispatch"}:
        fail("release publication must retain its validated dispatch entry point")
    dispatch = triggers["workflow_dispatch"]
    inputs = dispatch.get("inputs") if isinstance(dispatch, dict) else None
    release_tag = inputs.get("release_tag") if isinstance(inputs, dict) else None
    if (not isinstance(release_tag, dict) or release_tag.get("required") is not True
            or release_tag.get("type") != "string"):
        fail("release dispatch must require a string release_tag input")
    jobs = workflow.get("jobs", {})
    if not {"validate-release", "verify-release", BUILD_JOB, PUBLISH_JOB} <= jobs.keys():
        fail("source validation, release verification, SDK build and publisher are required")
    build, publish = jobs[BUILD_JOB], jobs[PUBLISH_JOB]
    for name, job in ((BUILD_JOB, build), (PUBLISH_JOB, publish)):
        if job.get("if") != REPO_GUARD or job.get("continue-on-error", False) is not False:
            fail(f"{name} must require successful dependencies in the owning repository")
    if build.get("needs") != "validate-release" or build.get("permissions") != {"contents": "read"}:
        fail("SDK build must depend on source validation and have read-only permissions")
    if "environment" in build:
        fail("SDK build must not hold protected-environment authority")
    build_steps = reviewed_steps(steps(build))
    checkouts = actions(build_steps, "actions/checkout")
    if len(checkouts) != 1 or not build_steps or checkouts[0] is not build_steps[0]:
        fail("SDK build must first and only check out the validated source")
    checkout = checkouts[0]
    options = checkout.get("with", {})
    if (options.get("ref") != SOURCE_SHA_REF or options.get("persist-credentials") is not False
            or "repository" in options or checkout.get("if") or checkout.get("continue-on-error")):
        fail("SDK checkout must use the validated source SHA without persisted credentials")
    checkout_path = options.get("path", ".")
    if not isinstance(checkout_path, str) or PurePosixPath(checkout_path) != PurePosixPath("."):
        fail("SDK checkout must place the validated source at the workspace root")
    if any(secret in str((build, build_steps)) for secret in ("secrets.", "github.token")):
        fail("SDK build must not receive repository credentials")

    if set(publish.get("needs", [])) != {"validate-release", "verify-release", BUILD_JOB}:
        fail("SDK publisher must depend on source validation, release verification and the SDK build")
    if publish.get("environment") != "npm-tracedecay-sdk":
        fail("SDK publisher must use the protected npm-tracedecay-sdk environment")
    if publish.get("permissions") != {"contents": "read", "id-token": "write"}:
        fail("SDK publisher must grant only contents: read and id-token: write")
    if any(token in str(publish) for token in ("secrets.", "NPM_TOKEN", "NODE_AUTH_TOKEN", "_authToken", ".npmrc")):
        fail("SDK publisher must remain tokenless for OIDC trusted publishing")
    publish_steps = steps(publish)
    for step in publish_steps:
        uses = step.get("uses", "")
        if uses and not uses.startswith(("actions/download-artifact@", "actions/setup-node@")):
            fail("SDK publisher may only download the verified artifact and set up Node")
        if "registry-url" in step.get("with", {}):
            fail("SDK publisher must not configure registry token authentication")
        if re.search(r"\b(?:npm\s+install|npx\s)", step.get("run", "")):
            fail("SDK publisher must not install executable packages with publish authority")
    reviewed_steps(publish_steps)

    uploaded = actions(steps(build), "actions/upload-artifact")
    downloaded = actions(publish_steps, "actions/download-artifact")
    if len(uploaded) != 1 or len(downloaded) != 1 or not uploaded[0].get("id"):
        fail("SDK release must hand off one identified build artifact")
    upload, download = uploaded[0], downloaded[0]
    if any(step.get("if") or step.get("continue-on-error") for step in (upload, download)):
        fail("SDK artifact handoff must propagate failure")
    artifact_id = "${{ steps." + upload["id"] + ".outputs.artifact-id }}"
    if build.get("outputs", {}).get("artifact_id") != artifact_id:
        fail("SDK build must export the immutable upload artifact ID")
    source = download.get("with", {})
    if (source.get("artifact-ids") != "${{ needs.build-typescript.outputs.artifact_id }}"
            or any(key in source for key in ("name", "pattern", "repository", "run-id", "github-token"))
            or source.get("digest-mismatch", "error") != "error"):
        fail("SDK publisher must download only this build's artifact ID and reject digest mismatch")


def main() -> None:
    try:
        validate(yaml.safe_load(WORKFLOW_PATH.read_text(encoding="utf-8")))
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(f"release.yml SDK publication policy violation: {error}", file=sys.stderr)
        raise SystemExit(1) from error
    print("SDK release source, publication authority and immutable artifact handoff are valid.")


if __name__ == "__main__":
    main()
