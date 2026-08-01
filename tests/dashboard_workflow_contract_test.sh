#!/usr/bin/env bash
set -euo pipefail

python3 - \
  .github/workflows/ci.yml \
  .github/workflows/release.yml \
  .github/workflows/release-beta.yml \
  .github/workflows/release-plz.yml \
  .github/workflows/release-pr-integrity.yml \
  .github/workflows/plugin-validation.yml <<'PY'
import fnmatch
import os
import pathlib
import re
import sys

ci, stable, beta, release_plz, release_integrity, plugin = [
    open(path, encoding="utf-8").read() for path in sys.argv[1:]
]


def job_block(workflow: str, job: str) -> str:
    marker = f"  {job}:"
    if marker not in workflow:
        raise SystemExit(f"workflow is missing expected job {job!r}")
    return re.split(r"\n  (?=\S)", workflow.split(marker, 1)[1], maxsplit=1)[0]


for name, workflow in [
    ("CI", ci),
    ("stable release", stable),
    ("beta release", beta),
    ("release-plz", release_plz),
    ("plugin validation", plugin),
]:
    for required in [
        "dashboard-assets:",
        "npm run build",
        "scripts/check-dashboard-bundle.py",
        "actions/upload-artifact@",
        "name: dashboard-app-dist",
        "actions/download-artifact@",
        "path: dashboard/app-dist",
        'TRACEDECAY_SKIP_DASHBOARD_BUILD: "1"',
    ]:
        if required not in workflow:
            raise SystemExit(f"{name} workflow is missing dashboard artifact contract {required!r}")

for job in [
    "test",
    "windows-build",
    "windows-platform-acceptance",
    "release-compatibility",
    "clippy",
    "dashboard",
    "hermes-integration",
]:
    block = job_block(ci, job)
    if "dashboard-assets" not in block:
        raise SystemExit(f"CI Rust job {job!r} must wait for dashboard-assets")
    if "actions/download-artifact@" not in block:
        raise SystemExit(f"CI Rust job {job!r} must download dashboard-app-dist")

dashboard_job = job_block(ci, "dashboard")
for required in [
    "npm run typecheck",
    "npm run contracts:check",
    "npm test",
    "cargo nextest run --all-features --test dashboard_api_test",
    "--no-tests=fail",
]:
    if required not in dashboard_job:
        raise SystemExit(
            f"CI dashboard integration job must preserve frontend check {required!r}"
        )

# The accessibility gates are their own job. Every Rust job declares
# `needs: dashboard-assets`, so an axe failure inside dashboard-assets skipped
# the entire Rust matrix and destroyed the signal about whether Rust passed.
# Each harness also runs its own full rsbuild build, which alone blew that
# job's timeout budget.
accessibility_job = job_block(ci, "dashboard-accessibility")
for required in [
    "playwright install",
    "npm run axe:audit",
    "npm run axe:explorer",
    "needs: dashboard-assets",
    "actions/download-artifact@",
    "path: dashboard/app-dist",
]:
    if required not in accessibility_job:
        raise SystemExit(
            f"CI dashboard-accessibility job must preserve {required!r}"
        )

# Keeping the gates out of dashboard-assets is the entire point of the split;
# a well-meaning "run them where the bundle is" edit would undo it silently.
assets_job = job_block(ci, "dashboard-assets")
for forbidden in ["playwright install", "npm run axe:"]:
    if forbidden in assets_job:
        raise SystemExit(
            f"CI dashboard-assets job must not run {forbidden!r}: the Rust "
            "matrix needs it, so an accessibility failure would skip every "
            "Rust job. Keep it in dashboard-accessibility."
        )

# Nothing may depend on the accessibility gate, or its failure would skip
# whatever does and reintroduce the blast radius this split removed.
jobs_section = ci.split("\njobs:\n", 1)[1]
for job_name in re.findall(r"(?m)^  ([A-Za-z0-9_-]+):$", jobs_section):
    if job_name == "dashboard-accessibility":
        continue
    if "dashboard-accessibility" in job_block(ci, job_name):
        raise SystemExit(
            f"CI job {job_name!r} must not depend on dashboard-accessibility"
        )

for name, workflow, jobs in [
    ("stable release", stable, ["build", "package-workspace"]),
    ("beta release", beta, ["build", "package-workspace"]),
    ("release-plz", release_plz, ["release-plz-release", "release-plz-pr"]),
    ("plugin validation", plugin, ["mcp-conformance-smoke"]),
]:
    for job in jobs:
        block = job_block(workflow, job)
        if "dashboard-assets" not in block:
            raise SystemExit(f"{name} job {job!r} must wait for dashboard-assets")
        if "actions/download-artifact@" not in block:
            raise SystemExit(f"{name} job {job!r} must download dashboard-app-dist")

if "cargo " in release_integrity or "npm run build" in release_integrity:
    raise SystemExit("release PR integrity must remain a read-only path guard")

# Windows durable behavior runs as one complete, non-vacuous test target.
durable_job = job_block(ci, "windows-pr8-temporal-durable")
for required in [
    "binary(=windows_durable_behavior)",
    "--no-tests=fail",
]:
    if required not in durable_job:
        raise SystemExit(
            f"Windows durable target must preserve {required!r}"
        )
for retired in ["$requiredFilters", "cargo-nextest nextest list", "$selected.Count"]:
    if retired in durable_job:
        raise SystemExit(
            f"Windows durable target must not retain filter inventory {retired!r}"
        )

# --------------------------------------------------------------------------
# libtest-filtered gates must prove a test ran.
#
# `cargo test <name> -- --exact` exits 0 when the filter matches NOTHING, so
# every name-filtered `cargo test` in CI is one rename away from becoming a
# silent no-op that still reports success. scripts/require-exact-test.sh
# asserts the count libtest prints.
# --------------------------------------------------------------------------
guard = pathlib.Path("scripts/require-exact-test.sh")
if not guard.is_file():
    raise SystemExit(f"{guard.as_posix()} must exist to guard name-filtered gates")
if not os.access(guard, os.X_OK):
    raise SystemExit(f"{guard.as_posix()} must be executable")

# Every exact-name cargo invocation in the platform jobs uses the non-vacuity
# wrapper. Full dedicated test targets intentionally do not.
for job_name in ["test", "windows-platform-acceptance"]:
    block = job_block(ci, job_name)
    # Drop comment lines (YAML and shell alike - neither runs anything), then
    # fold block scalars and shell line continuations so a guarded invocation
    # reads as one command; otherwise the wrapped `cargo test ...` continuation
    # line looks bare.
    commands = "\n".join(
        line for line in block.splitlines() if not line.lstrip().startswith("#")
    )
    folded = re.sub(r"\s+", " ", commands.replace("\\\n", " "))
    for match in re.finditer(r"--exact\b", folded):
        preceding = folded[max(0, match.start() - 300) : match.start()]
        if "require-exact-test.sh" not in preceding:
            raise SystemExit(
                f"{job_name} has an unguarded exact-name cargo test: "
                f"{folded[max(0, match.start() - 100) : match.start() + 20]!r}"
            )

# --------------------------------------------------------------------------
# Raw nextest junit must survive a failing test step.
#
# The OS-tagged pr12-pr13 upload runs only after the strict validators, so a
# red `Run tests` step ended the job before anything wrote the durations and
# failure detail anywhere durable. Every Linux/macOS run must therefore keep an
# unconditional raw copy, uploaded before the first gate that can abort.
# --------------------------------------------------------------------------


def steps_of(block: str) -> list[str]:
    return [step for step in re.split(r"\n(?=      - )", block) if step.strip()]


def step_title(step: str) -> str:
    match = re.match(r"\s*- (?:name: (.+)|uses: (.+))", step.lstrip("\n"))
    if match is None:
        return step.strip().splitlines()[0]
    return (match.group(1) or match.group(2)).strip()


def with_mapping(step: str) -> str:
    return step.split("with:", 1)[1] if "with:" in step else ""


test_steps = steps_of(job_block(ci, "test"))
raw_junit_indexes = [
    index
    for index, step in enumerate(test_steps)
    if "actions/upload-artifact@" in step
    and "path: target/nextest/ci/junit.xml" in step
]
if len(raw_junit_indexes) != 1:
    raise SystemExit(
        "CI test job must upload the raw nextest junit "
        "('path: target/nextest/ci/junit.xml') exactly once; found "
        f"{len(raw_junit_indexes)}"
    )
raw_junit_index = raw_junit_indexes[0]
raw_junit_step = test_steps[raw_junit_index]

if not re.search(r"(?m)^\s+if: always\(\)\s*$", raw_junit_step):
    raise SystemExit(
        "CI test job's raw nextest junit upload must be `if: always()`, or a "
        "failing test step keeps taking the junit down with it"
    )
if "retention-days: 7" not in with_mapping(raw_junit_step):
    raise SystemExit(
        "CI test job's raw nextest junit upload must set retention-days: 7"
    )
# A missing junit means the compile or the runner died. That is already a job
# failure; the upload must not add a second, misleading one, and must not
# swallow the fact that there was nothing to keep either.
if "if-no-files-found: warn" not in with_mapping(raw_junit_step):
    raise SystemExit(
        "CI test job's raw nextest junit upload must set "
        "'if-no-files-found: warn': a compile failure leaves no junit, and "
        "that must stay a warning on an already-failing job rather than a new "
        "error or a silent skip"
    )

try:
    run_tests_index = next(
        index
        for index, step in enumerate(test_steps)
        if step_title(step) == "Run tests"
    )
except StopIteration:
    raise SystemExit("CI test job must keep the 'Run tests' step")
if raw_junit_index < run_tests_index:
    raise SystemExit(
        "CI test job's raw nextest junit upload must come after 'Run tests'"
    )
for step in test_steps[run_tests_index + 1 : raw_junit_index]:
    if not re.search(r"(?m)^\s+if: always\(\)\s*$", step):
        raise SystemExit(
            "CI test job must upload the raw nextest junit before "
            f"{step_title(step)!r}: that step can abort the job first and the "
            "junit would be lost again"
        )

# Uploading it is only half the contract - the artifact names have to be
# distinct per OS, or the second runner collides with the first.
OS_TERNARY = re.compile(
    r"\$\{\{ matrix\.name == 'Linux' && '([^']+)' \|\| '([^']+)' \}\}"
)


def expand_matrix_os(name: str) -> list[str]:
    match = OS_TERNARY.search(name)
    if match is None:
        return [name]
    return [OS_TERNARY.sub(value, name) for value in match.groups()]


raw_junit_name = re.search(r"(?m)^\s+name: (.+)$", with_mapping(raw_junit_step))
if raw_junit_name is None:
    raise SystemExit("CI test job's raw nextest junit upload must name its artifact")
raw_junit_names = expand_matrix_os(raw_junit_name.group(1).strip())
if len(raw_junit_names) != 2:
    raise SystemExit(
        "CI test job's raw nextest junit artifact name must vary by OS "
        f"(found {raw_junit_name.group(1).strip()!r}); the Linux and macOS "
        "runners share this job and would otherwise collide on one name"
    )

uploaded: dict[str, str] = {}
for job_name in re.findall(r"(?m)^  ([A-Za-z0-9_-]+):$", jobs_section):
    for step in steps_of(job_block(ci, job_name)):
        if "actions/upload-artifact@" not in step:
            continue
        name_match = re.search(r"(?m)^\s+name: (.+)$", with_mapping(step))
        if name_match is None:
            continue
        for artifact in expand_matrix_os(name_match.group(1).strip()):
            owner = f"{job_name} / {step_title(step)}"
            if artifact in uploaded and uploaded[artifact] != owner:
                raise SystemExit(
                    f"CI uploads artifact {artifact!r} from two different "
                    f"steps ({uploaded[artifact]} and {owner}); "
                    "upload-artifact@v4 rejects duplicate names"
                )
            uploaded[artifact] = owner

# The junit consumers download by glob. A raw artifact that drifts into one of
# those patterns would feed unvalidated evidence into a strict packet gate.
for consumer_pattern in re.findall(r"(?m)^\s+pattern: (\S*junit\S*)$", ci):
    for artifact in raw_junit_names:
        if fnmatch.fnmatch(artifact, consumer_pattern):
            raise SystemExit(
                f"raw nextest junit artifact {artifact!r} matches the "
                f"download pattern {consumer_pattern!r}; the raw upload is "
                "unvalidated retention and must stay out of the packet "
                "aggregation inputs"
            )

# --------------------------------------------------------------------------
# The workflow-contract tests must protect master, not just pull requests.
# --------------------------------------------------------------------------
drift_job = job_block(ci, "release-version-drift")
if "bash tests/dashboard_workflow_contract_test.sh" not in drift_job:
    raise SystemExit(
        "release-version-drift must keep running this contract test"
    )
if "github.event_name == 'push'" not in drift_job:
    raise SystemExit(
        "release-version-drift must run on push as well as pull_request, or "
        "these contract checks never run on master"
    )
if not re.search(r"(?m)^  push:\s*\n\s+branches: \[master\]", plugin):
    raise SystemExit("plugin validation must run on master pushes")
platform_cursor_commands = [
    "npm --prefix plugin/cursor-native-extension ci",
    "npm --prefix plugin/cursor-native-extension run check",
    "npm --prefix plugin/cursor-native-extension test",
    "npm --prefix plugin/cursor-native-extension run package",
]
for name, block in [
    ("Linux/macOS test matrix", job_block(ci, "test")),
    ("Windows platform acceptance", job_block(ci, "windows-platform-acceptance")),
]:
    for required in platform_cursor_commands:
        if required not in block:
            raise SystemExit(f"{name} must preserve Cursor extension command {required!r}")
    if "npm publish" in block:
        raise SystemExit(f"{name} must package the Cursor extension without publishing")
for required in ["name: Linux", "name: macOS"]:
    if required not in job_block(ci, "test"):
        raise SystemExit(f"CI test matrix must preserve Cursor coverage for {required!r}")

PY
