#!/usr/bin/env bash
# Rewrites the open release PR body so closing keywords copied from commit
# messages cannot auto-close issues when the release PR merges.
#
# Requires the release-please `pr` output JSON in $RELEASE_PR_JSON, plus
# GH_TOKEN and GITHUB_REPOSITORY. Edits the body only when a keyword changed.
set -euo pipefail

RELEASE_PR_NUMBER=$(
  jq --exit-status --raw-output \
    '.number | select(type == "number")' \
    <<<"$RELEASE_PR_JSON"
)

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
body_file=$(mktemp)
rewritten_file=$(mktemp)
trap 'rm -f -- "$body_file" "$rewritten_file"' EXIT

gh pr view "$RELEASE_PR_NUMBER" --repo "$GITHUB_REPOSITORY" --json body --jq .body >"$body_file"
python3 "$root/scripts/neutralize-release-pr-closing-keywords.py" <"$body_file" >"$rewritten_file"
if cmp -s "$body_file" "$rewritten_file"; then
  exit 0
fi
gh pr edit "$RELEASE_PR_NUMBER" --repo "$GITHUB_REPOSITORY" --body-file "$rewritten_file"
