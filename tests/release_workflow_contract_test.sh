#!/usr/bin/env bash
set -euo pipefail

release_plz=".github/workflows/release-plz.yml"
release_workflow=".github/workflows/release.yml"
release_beta=".github/workflows/release-beta.yml"
release_pr_integrity=".github/workflows/release-pr-integrity.yml"
ci_workflow=".github/workflows/ci.yml"
release_config="release-plz.toml"
root_manifest="Cargo.toml"

if grep -q 'GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}' "$release_plz"; then
  echo "release-plz must not publish releases with GITHUB_TOKEN" >&2
  echo "GitHub suppresses downstream on: release workflows from GITHUB_TOKEN-created releases." >&2
  exit 1
fi

python3 - "$release_plz" <<'PY'
import sys

text = open(sys.argv[1], encoding="utf-8").read()
for forbidden in [
    "environment: crates-io",
    "id-token: write",
    "Publish crate",
]:
    if forbidden in text:
        raise SystemExit(f"root-only GitHub release workflow must not contain {forbidden!r}")

for required in [
    "  github-release:",
    "name: Create GitHub release",
    "- name: Create GitHub release with release-plz",
    "- name: Retry GitHub release after transient API failure",
    "- name: Fail when GitHub release still fails",
    "- name: Check GitHub release version drift",
]:
    if required not in text:
        raise SystemExit(f"root-only GitHub release workflow missing {required!r}")
PY

python3 - "$release_config" "$root_manifest" <<'PY'
import sys
import tomllib
from pathlib import Path

config_path, root_path = sys.argv[1:]
with open(config_path, "rb") as handle:
    config = tomllib.load(handle)

workspace = config["workspace"]
for key in ("release", "publish"):
    if workspace.get(key) is not False:
        raise SystemExit(f"release-plz workspace {key} must default to false")

packages = {package["name"]: package for package in config.get("package", [])}
root_release = packages.get("tracedecay")
if root_release is None:
    raise SystemExit("release-plz must manage the tracedecay root package")
for key, value in {
    "release": True,
    "git_only": True,
    "publish": False,
    "git_release_enable": True,
    "git_release_name": "v{{ version }}",
    "git_tag_enable": True,
    "git_tag_name": "v{{ version }}",
}.items():
    if root_release.get(key) != value:
        raise SystemExit(f"tracedecay release-plz {key} must be {value!r}")

with open(root_path, "rb") as handle:
    root_manifest = tomllib.load(handle)
if root_manifest["package"].get("name") != "tracedecay":
    raise SystemExit("root package must remain named tracedecay")
if root_manifest["package"].get("publish") is not False:
    raise SystemExit("root package must set publish = false")
if not any(binary.get("name") == "tracedecay" for binary in root_manifest.get("bin", [])):
    raise SystemExit("root binary must remain named tracedecay")

internal_paths = [Path(member) / "Cargo.toml" for member in root_manifest["workspace"]["members"]]
internal_names = []
for path in internal_paths:
    with open(path, "rb") as handle:
        manifest = tomllib.load(handle)
    name = manifest["package"].get("name")
    internal_names.append(name)
    if manifest["package"].get("publish") is not False:
        raise SystemExit(f"internal crate {name} must set publish = false")
    if packages.get(name, {}).get("release") is not False:
        raise SystemExit(f"internal crate {name} must be ignored by release-plz")

if len(internal_names) != 13:
    raise SystemExit(f"expected 13 internal crates, found {len(internal_names)}")
PY

python3 - "$release_plz" <<'PY'
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read()
release_step = text.split("- name: Create GitHub release with release-plz", 1)[1].split("- name:", 1)[0]
retry_step = text.split("- name: Retry GitHub release after transient API failure", 1)[1].split("- name:", 1)[0]
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
import sys

stable = open(sys.argv[1], encoding="utf-8").read()
beta = open(sys.argv[2], encoding="utf-8").read()

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
