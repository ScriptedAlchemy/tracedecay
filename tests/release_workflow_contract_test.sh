#!/usr/bin/env bash
set -euo pipefail

release_please=".github/workflows/release-please.yml"
release_workflow=".github/workflows/release.yml"
release_beta=".github/workflows/release-beta.yml"
release_pr_integrity=".github/workflows/release-pr-integrity.yml"
ci_workflow=".github/workflows/ci.yml"
release_config="release-please-config.json"
release_manifest=".release-please-manifest.json"
root_manifest="Cargo.toml"
lockfile="Cargo.lock"
version_file="version.txt"

[[ ! -e .github/workflows/release-plz.yml ]]
[[ ! -e release-plz.toml ]]
[[ -x install.sh ]]

python3 - \
  "$release_config" \
  "$release_manifest" \
  "$root_manifest" \
  "$lockfile" \
  "$version_file" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

config_path, release_manifest_path, root_path, lock_path, version_path = sys.argv[1:]
config = json.loads(Path(config_path).read_text(encoding="utf-8"))
release_manifest = json.loads(Path(release_manifest_path).read_text(encoding="utf-8"))
version = Path(version_path).read_text(encoding="utf-8").strip()

if config.get("release-type") != "simple":
    raise SystemExit("release automation must use package-neutral tag versioning")
if config.get("include-v-in-tag") is not True:
    raise SystemExit("GitHub release tags must use the v prefix")

root_release = config.get("packages", {}).get(".")
if root_release is None:
    raise SystemExit("release automation must manage the repository root")
if root_release.get("package-name") != "tracedecay":
    raise SystemExit("root release package must remain named tracedecay")

extra_files = {
    (entry.get("type"), entry.get("path"), entry.get("jsonpath"))
    for entry in root_release.get("extra-files", [])
}
required_extra_files = {
    ("toml", "Cargo.toml", "$.package.version"),
}
if extra_files != required_extra_files:
    raise SystemExit("release automation must update only the root Cargo manifest")

with open(root_path, "rb") as handle:
    root_manifest = tomllib.load(handle)
if root_manifest["package"].get("name") != "tracedecay":
    raise SystemExit("root package must remain named tracedecay")
if root_manifest["package"].get("publish") is not False:
    raise SystemExit("root package must set publish = false")
if root_manifest["package"].get("version") != version:
    raise SystemExit("Cargo.toml and version.txt must stay aligned")
if release_manifest.get(".") != version:
    raise SystemExit("release manifest and version.txt must stay aligned")
if not any(binary.get("name") == "tracedecay" for binary in root_manifest.get("bin", [])):
    raise SystemExit("root binary must remain named tracedecay")

lock_text = Path(lock_path).read_text(encoding="utf-8")
if "x-release-please-version" in lock_text:
    raise SystemExit("Cargo.lock must not use markers that Cargo strips")
with open(lock_path, "rb") as handle:
    lockfile = tomllib.load(handle)
root_locks = [
    package for package in lockfile["package"]
    if package.get("name") == "tracedecay"
]
if len(root_locks) != 1 or root_locks[0].get("version") != version:
    raise SystemExit("Cargo.lock root package and version.txt must stay aligned")

internal_paths = [Path(member) / "Cargo.toml" for member in root_manifest["workspace"]["members"]]
for path in internal_paths:
    with open(path, "rb") as handle:
        manifest = tomllib.load(handle)
    name = manifest["package"].get("name")
    if manifest["package"].get("publish") is not False:
        raise SystemExit(f"internal crate {name} must set publish = false")
PY

python3 - "$release_please" "$release_workflow" "$release_beta" <<'PY'
import sys

release_please = open(sys.argv[1], encoding="utf-8").read()
stable = open(sys.argv[2], encoding="utf-8").read()
beta = open(sys.argv[3], encoding="utf-8").read()

for forbidden in [
    "cargo publish",
    "release-plz/action",
    "environment: crates-io",
    "id-token: write",
]:
    if forbidden in release_please:
        raise SystemExit(f"GitHub-only release workflow must not contain {forbidden!r}")

for required in [
    "googleapis/release-please-action@v5",
    "token: ${{ secrets.RELEASE_PLZ_TOKEN }}",
    "target-branch: master",
    "steps.release.outputs.prs_created == 'true'",
    "fromJSON(steps.release.outputs.pr).headBranchName",
    'cargo update -p tracedecay --precise "$release_version"',
    'git push origin "HEAD:$RELEASE_PR_BRANCH"',
    "Check GitHub release version drift",
]:
    if required not in release_please:
        raise SystemExit(f"GitHub-only release workflow missing {required!r}")

for path, text in [
    (sys.argv[1], release_please),
    (sys.argv[2], stable),
    (sys.argv[3], beta),
]:
    if "concurrency:" not in text:
        raise SystemExit(f"{path} must serialize release mutations")
    if "cancel-in-progress: false" not in text:
        raise SystemExit(f"{path} must never cancel an in-progress release")

for item in [
    "types: [published]",
    "SHA256SUMS",
    "install.sh",
    "sha256sum tracedecay-*",
]:
    if item not in stable:
        raise SystemExit(f"stable release workflow missing {item!r}")

for name, text in [("stable", stable), ("beta", beta)]:
    if "required: true" not in text:
        raise SystemExit(f"{name} manual rebuild must require an explicit release tag")
    expected = "github.event_name == 'workflow_dispatch' && inputs.release_tag || github.event.release.tag_name"
    if expected not in text:
        raise SystemExit(f"{name} release identity must normalize to the release tag")

for item in [
    "Validate manual prerelease rebuild",
    "gh release view \"$RELEASE_TAG\"",
    "ref: ${{ env.RELEASE_TAG }}",
]:
    if item not in beta:
        raise SystemExit(f"beta manual rebuild contract missing {item!r}")
PY

grep -Fq "if: \${{ !cancelled() &&" "$ci_workflow"
grep -Fq "startsWith(github.head_ref, 'release-please--')" "$ci_workflow"

python3 - "$release_pr_integrity" <<'PY'
import sys

text = open(sys.argv[1], encoding="utf-8").read()
required = [
    "pull_request_target:",
    "contents: read",
    "pull-requests: read",
    "persist-credentials: false",
    "github.event.pull_request.head.sha",
    "github.event.pull_request.head.repo.full_name == github.repository",
    "startsWith(github.event.pull_request.head.ref, 'release-please--')",
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
