#!/usr/bin/env bash
set -euo pipefail

release_plz=".github/workflows/release-plz.yml"
release_workflow=".github/workflows/release.yml"

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

grep -q 'release:' "$release_workflow"
grep -q 'types: \[published\]' "$release_workflow"
