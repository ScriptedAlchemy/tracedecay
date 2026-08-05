#!/usr/bin/env bash
# Release safety guards.
#
# These are the release-workflow properties whose violation is silent and
# expensive: a suppressed downstream release, a publication cancelled halfway,
# a mutable third-party action inside the publish path, or a
# pull_request_target guard that hands out write credentials. Everything else
# about how these workflows are spelled is free to change.
set -euo pipefail

release_please=".github/workflows/release-please.yml"
release_stable=".github/workflows/release.yml"
release_beta=".github/workflows/release-beta.yml"
sdk_publish=".github/workflows/sdk-publish.yml"
release_pr_integrity=".github/workflows/release-pr-integrity.yml"
sdk_conformance=".github/workflows/sdk-conformance.yml"

python3 - <<'PY'
import json
import tomllib
from pathlib import Path

with Path("Cargo.toml").open("rb") as handle:
    root = tomllib.load(handle)

version = Path("version.txt").read_text(encoding="utf-8").strip()
release_manifest = json.loads(
    Path(".release-please-manifest.json").read_text(encoding="utf-8")
)
if root["package"].get("publish") is not False:
    raise SystemExit("root Cargo package must remain private")
if root["package"]["version"] != version or release_manifest.get(".") != version:
    raise SystemExit("release version authorities are not aligned")
with Path("Cargo.lock").open("rb") as handle:
    lockfile = tomllib.load(handle)
root_locks = [
    package
    for package in lockfile["package"]
    if package.get("name") == root["package"]["name"]
]
if len(root_locks) != 1 or root_locks[0].get("version") != version:
    raise SystemExit("Cargo.lock root version is not aligned")

for member in root["workspace"]["members"]:
    manifest_path = Path(member, "Cargo.toml")
    with manifest_path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest["package"].get("publish") is not False:
        raise SystemExit(f"workspace package is publishable: {manifest_path}")
PY

# GitHub suppresses `on: release` workflows for releases created by
# GITHUB_TOKEN, so Release Please must use the dedicated release token.
if grep -q 'token: ${{ secrets.GITHUB_TOKEN }}' "$release_please"; then
  echo "Release Please must not publish releases with GITHUB_TOKEN" >&2
  exit 1
fi

python3 - "$release_please" "$release_stable" "$release_beta" <<'PY'
import sys

for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    if "cancel-in-progress: true" in text:
        raise SystemExit(f"{path} must never cancel in-progress publication")
PY

python3 - "$release_please" "$release_stable" "$release_beta" "$sdk_publish" \
  "$release_pr_integrity" "$sdk_conformance" <<'PY'
import re
import sys

sha_ref = re.compile(r"^[^@]+@[0-9a-f]{40}$")
for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    for uses in re.findall(r"^\s*-?\s*uses:\s+([^#\s]+)", text, re.MULTILINE):
        if uses.startswith("./"):
            continue
        if not sha_ref.fullmatch(uses):
            raise SystemExit(
                f"{path} external action must use an immutable SHA: {uses}"
            )
PY

python3 - "$release_stable" "$release_beta" <<'PY'
import re
import sys

stable_path, beta_path = sys.argv[1:]
stable = open(stable_path, encoding="utf-8").read()
beta = open(beta_path, encoding="utf-8").read()

for path, text, job, next_job in (
    (stable_path, stable, "validate-release", "dashboard-assets"),
    (beta_path, beta, "validate", "build"),
):
    section = text.split(f"  {job}:\n", 1)[1].split(f"\n  {next_job}:", 1)[0]
    match = re.search(r"^    permissions:\n((?:^      .+\n)+)", section, re.MULTILINE)
    if match is None:
        raise SystemExit(f"{path} {job} must declare job-level permissions")
    permissions = {
        line.strip()
        for line in match.group(1).splitlines()
        if line.strip()
    }
    if permissions != {"contents: read", "attestations: read"}:
        raise SystemExit(
            f"{path} {job} must grant exactly contents: read and attestations: read"
        )

external_publication_markers = (
    "homebrew-tap",
    "scoop-bucket",
    ".bottle.tar.gz",
    "update-homebrew:",
    "update-scoop:",
    "TAP_GITHUB_TOKEN",
)
for marker in external_publication_markers:
    if marker in stable:
        raise SystemExit(
            f"{stable_path} must not publish external package repositories: {marker}"
        )

for path, text in ((stable_path, stable), (beta_path, beta)):
    if "scripts/package-release-archive.py" not in text:
        raise SystemExit(f"{path} must use deterministic release archive packaging")
    for mutable_packager in ("tar czf", "tar -czf", "Compress-Archive", "7z a "):
        if mutable_packager in text:
            raise SystemExit(
                f"{path} contains timestamp-sensitive packaging: {mutable_packager}"
            )
    for required in (
        "scripts/plan-release-recovery.py",
        "gh attestation verify",
        "--signer-workflow",
        "--source-ref",
        "--source-digest",
        "outputs.build_required",
        'test "$GITHUB_REF" = "refs/tags/',
        'test "$GITHUB_SHA" = "$source_sha"',
    ):
        if required not in text:
            raise SystemExit(
                f"{path} must retain uploaded assets with exact source provenance: "
                f"{required}"
            )

for forbidden in (
    'cmp -s "$asset" "remote-assets/$name"',
    'cmp -s "$release_asset" "remote-assets/$name"',
):
    if forbidden in stable or forbidden in beta:
        raise SystemExit(
            "release recovery must not compare rebuilt mutable outputs: "
            f"{forbidden}"
        )
PY

python3 - "$release_pr_integrity" <<'PY'
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read()

# This workflow runs on pull_request_target, so it sees fork code with the
# base repository's token. It must never hand that token to the checkout, and
# must never hold write scopes.
if "persist-credentials: false" not in text:
    raise SystemExit(f"{path} must check out without persisted credentials")
if "contents: write" in text or "pull-requests: write" in text:
    raise SystemExit(f"{path} must remain read-only")
PY
