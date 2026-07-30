#!/usr/bin/env bash
set -euo pipefail

release_plz=".github/workflows/release-plz.yml"
release_workflow=".github/workflows/release.yml"
release_beta=".github/workflows/release-beta.yml"
release_pr_integrity=".github/workflows/release-pr-integrity.yml"
ci_workflow=".github/workflows/ci.yml"

if grep -q 'GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}' "$release_plz"; then
  echo "release-plz must not publish releases with GITHUB_TOKEN" >&2
  echo "GitHub suppresses downstream on: release workflows from GITHUB_TOKEN-created releases." >&2
  exit 1
fi

python3 - "$release_plz" <<'PY'
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read()
release_step = text.split("- name: Run release-plz release", 1)[1].split("- name:", 1)[0]
retry_step = text.split("- name: Retry release-plz release after transient GitHub API failure", 1)[1].split("- name:", 1)[0]
release_pr_step = text.split("- name: Run release-plz release-pr", 1)[1]

for name, step in [
    ("release", release_step),
    ("release retry", retry_step),
    ("release-pr", release_pr_step),
]:
    expected = "GITHUB_TOKEN: ${{ secrets.RELEASE_PLZ_TOKEN }}"
    if expected not in step:
        raise SystemExit(f"{name} step must use RELEASE_PLZ_TOKEN")
PY

grep -Fq "if: \${{ !cancelled() &&" "$ci_workflow"

grep -q 'release:' "$release_workflow"
grep -q 'types: \[published\]' "$release_workflow"

python3 - "$release_plz" "$release_workflow" "$release_beta" <<'PY'
import sys

for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    if "concurrency:" not in text:
        raise SystemExit(f"{path} must serialize release mutations")
    if "cancel-in-progress: false" not in text:
        raise SystemExit(f"{path} must never cancel in-progress publication")
PY

python3 - "$release_workflow" "$release_beta" <<'PY'
import re
import sys

stable = open(sys.argv[1], encoding="utf-8").read()
beta = open(sys.argv[2], encoding="utf-8").read()

for name, text in [("stable", stable), ("beta", beta)]:
    if "required: true" not in text:
        raise SystemExit(f"{name} manual rebuild must require an explicit release tag")
    expected = "github.event_name == 'workflow_dispatch' && inputs.release_tag || github.event.release.tag_name"
    if expected not in text:
        raise SystemExit(f"{name} release identity must normalize to the release tag")
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ]:
        if target not in text:
            raise SystemExit(f"{name} release must preserve package coverage for {target}")
    for forbidden in [
        r"cargo install[^\n]*--all-features",
        r'std_cargo_args, "--all-features"',
        r"python3 scripts/check-production-feature-profile.py",
    ]:
        if re.search(forbidden, text):
            raise SystemExit(
                f"{name} release artifact must not enable test features: {forbidden!r}"
            )
    for required in [
        "Verify all-feature release build compiles",
        "Build release binary for packaging",
        ".release-automation/scripts/resolve-release-source-profile.py",
        "${{ steps.release-profile.outputs.cargo_args }}",
        "Run historical release binary smoke",
        "steps.release-profile.outputs.profile == 'legacy-default'",
        "Verify release Cargo install",
        "path: release-source",
        "--source release-source",
    ]:
        if required not in text:
            raise SystemExit(
                f"{name} release must preserve production packaging guard {required!r}"
            )
    compile_index = text.index("Verify all-feature release build compiles")
    package_build_index = text.index("Build release binary for packaging")
    mcpb_index = text.index("Package MCPB")
    if not compile_index < package_build_index < mcpb_index:
        raise SystemExit(
            f"{name} release must leave the source-compatible production binary "
            "at the packaging path"
        )
    if "Validate immutable FastEmbed fixture pins\n        if: steps.release-profile.outputs.profile == 'production'" not in text:
        raise SystemExit(
            f"{name} historical rebuild must not require modern FastEmbed fixtures"
        )
    if 'feature_args = "${{ steps.release-profile.outputs.cargo_args }}".split' not in text:
        raise SystemExit(
            f"{name} Homebrew source install must use the resolved source profile"
        )

if "  validate-stable-release:" not in stable:
    raise SystemExit("stable manual rebuild must have a validation job")
stable_validation = stable.split("  validate-stable-release:", 1)[1].split("\n  build:", 1)[0]
for item in [
    "Validate manual stable rebuild",
    "if: github.event_name == 'workflow_dispatch'",
    'gh release view "$RELEASE_TAG"',
    "--json isPrerelease",
    "--jq .isPrerelease",
    '= false',
]:
    if item not in stable_validation:
        raise SystemExit(f"stable manual rebuild contract missing {item!r}")

for job_name, next_job in [("build", "package-workspace")]:
    job = stable.split(f"  {job_name}:", 1)[1].split(f"\n  {next_job}:", 1)[0]
    if "validate-stable-release" not in job:
        raise SystemExit(
            f"stable {job_name} must wait for manual release validation before checkout/build"
        )

if "  package-workspace:" in stable:
    package_job = stable.split("  package-workspace:", 1)[1].split(
        "\n  publish-assets:", 1
    )[0]
    if "validate-stable-release" not in package_job:
        raise SystemExit(
            "stable package-workspace must wait for manual release validation before checkout/build"
        )

dashboard_job = stable.split("  dashboard-assets:", 1)[1].split("\n  build:", 1)[0]
if "needs: validate-stable-release" not in dashboard_job:
    raise SystemExit(
        "stable dashboard-assets must wait for manual release validation before checkout/build"
    )

for item in [
    "Validate manual prerelease rebuild",
    "gh release view \"$RELEASE_TAG\"",
    "ref: ${{ env.RELEASE_TAG }}",
]:
    if item not in beta:
        raise SystemExit(f"beta manual rebuild contract missing {item!r}")

for item in [
    "uses: rui314/setup-mold@v1",
    "mold-version: 2.41.0",
    "make-default: true",
    "Verify mold is the default linker",
]:
    if item not in beta:
        raise SystemExit(f"beta old-tag rebuild must inline Linux mold setup: missing {item!r}")

if "uses: ./.github/actions/setup-linux-mold" in beta:
    raise SystemExit("beta old-tag rebuild must not depend on a tag-local mold action")
PY

python3 - "$release_pr_integrity" <<'PY'
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read()

required = [
    "pull_request_target:",
    "contents: read",
    "pull-requests: read",
    "persist-credentials: false",
    "github.event.pull_request.head.sha",
    "github.event.pull_request.head.repo.full_name == github.repository",
    "git show \"$BASE_SHA:scripts/check-release-pr-integrity.sh\"",
    "release-extra-files-approved",
    "scripts/check-release-pr-integrity.sh",
    "cancel-in-progress: true",
]
for item in required:
    if item not in text:
        raise SystemExit(f"release PR integrity workflow missing {item!r}")

if "contents: write" in text or "pull-requests: write" in text:
    raise SystemExit("release PR integrity workflow must remain read-only")
PY

python3 scripts/test-resolve-release-source-profile.py
