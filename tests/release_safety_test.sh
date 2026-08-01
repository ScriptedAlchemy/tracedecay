#!/usr/bin/env bash
# Release safety guards.
#
# These are the release-workflow properties whose violation is silent and
# expensive: a suppressed downstream release, a publication cancelled halfway,
# a mutable third-party action inside the publish path, or a
# pull_request_target guard that hands out write credentials. Everything else
# about how these workflows are spelled is free to change.
set -euo pipefail

release_plz=".github/workflows/release-plz.yml"
sdk_publish=".github/workflows/sdk-publish.yml"
release_pr_integrity=".github/workflows/release-pr-integrity.yml"

# GitHub suppresses `on: release` workflows for releases created by
# GITHUB_TOKEN, so release-plz publishing under it silently breaks downstream.
if grep -q 'GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}' "$release_plz"; then
  echo "release-plz must not publish releases with GITHUB_TOKEN" >&2
  exit 1
fi

python3 - "$release_plz" ".github/workflows/release.yml" \
  ".github/workflows/release-beta.yml" <<'PY'
import sys

for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    if "cancel-in-progress: true" in text:
        raise SystemExit(f"{path} must never cancel in-progress publication")
PY

python3 - "$sdk_publish" <<'PY'
import re
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read()
sha_ref = re.compile(r"^[^@]+@[0-9a-f]{40}$")
for uses in re.findall(r"^\s*-?\s*uses:\s+([^#\s]+)", text, re.MULTILINE):
    if uses.startswith("./"):
        continue
    if not sha_ref.fullmatch(uses):
        raise SystemExit(f"{path} external action must use an immutable SHA: {uses}")
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
