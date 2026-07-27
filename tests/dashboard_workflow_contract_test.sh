#!/usr/bin/env bash
set -euo pipefail

python3 - \
  .github/workflows/ci.yml \
  .github/workflows/release.yml \
  .github/workflows/release-beta.yml \
  .github/workflows/release-plz.yml \
  .github/workflows/release-pr-integrity.yml \
  .github/workflows/plugin-validation.yml <<'PY'
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
    "windows-pr12-pr13-packets",
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
    "npm run boundary:check",
]:
    if required not in dashboard_job:
        raise SystemExit(
            f"CI dashboard integration job must preserve frontend check {required!r}"
        )

# The boundary gate needs ast-grep on PATH, and the install must come after
# setup-node or the global bin can belong to a different Node toolchain.
if dashboard_job.index("actions/setup-node@") > dashboard_job.index("Install ast-grep"):
    raise SystemExit("CI dashboard job must install ast-grep after actions/setup-node")
if dashboard_job.index("Install ast-grep") > dashboard_job.index("npm run boundary:check"):
    raise SystemExit("CI dashboard job must install ast-grep before the boundary gate")

# Plan 11 makes WCAG 2.2 AA and the payload ceilings acceptance criteria, so
# they are gates rather than scripts a developer may remember to run. The budget
# check belongs to the artifact build: it measures the bytes being uploaded.
assets_job = job_block(ci, "dashboard-assets")
if "scripts/check-dashboard-budget.mjs" not in assets_job:
    raise SystemExit(
        "CI dashboard-assets job must preserve the payload budget gate "
        "'scripts/check-dashboard-budget.mjs'"
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

# A boundary step wired to an empty rule set passes every time and proves
# nothing, so the gate's contents are part of the contract, not just its
# invocation. Plan 11's acceptance names the semantics renderers may not
# compute; each id below carries one of them.
package_json = pathlib.Path("dashboard/package.json").read_text(encoding="utf-8")
if '"boundary:check"' not in package_json:
    raise SystemExit("dashboard/package.json must define the boundary:check script")

sgconfig = pathlib.Path("sgconfig.yml")
if not sgconfig.is_file():
    raise SystemExit("sgconfig.yml must exist for the renderer boundary gate")
rule_dir = pathlib.Path("tools/ast-grep/rules")
if rule_dir.as_posix() not in sgconfig.read_text(encoding="utf-8"):
    raise SystemExit(f"sgconfig.yml must list {rule_dir.as_posix()} in ruleDirs")

rules = "".join(
    path.read_text(encoding="utf-8") for path in sorted(rule_dir.glob("*.yml"))
)
for rule_id in [
    "viz-renderer-imports-server-state",
    "viz-renderer-opens-transport",
    "dashboard-ad-hoc-eventsource",
    "viz-renderer-persists-adapter-state",
    "viz-renderer-grades-state",
    "viz-adapter-ranks-locally",
    "viz-renderer-owns-routes",
]:
    # ast-grep resolves .ts and .tsx as separate languages and applies neither
    # rule to the other file type, so a rule that lost its Tsx twin would stop
    # covering GraphCanvas.tsx and Chart.tsx while still reporting success.
    for suffix in ["", "-tsx"]:
        full_id = f"{rule_id}{suffix}"
        if f"id: {full_id}\n" not in rules:
            raise SystemExit(
                f"{rule_dir.as_posix()} must keep Plan 11 boundary rule {full_id!r}"
            )
PY
