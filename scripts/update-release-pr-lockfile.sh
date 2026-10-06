#!/usr/bin/env bash
# Refreshes generated Cargo and Bazel metadata on a release-please release PR
# so both match the bumped crate version. Shared by the stable and beta
# release-please workflows so the branch extraction, drift guard, and bot
# commit cannot diverge between channels. It also neutralizes closing keywords
# in the PR body, because a manual `release-please release-pr` refresh
# rewrites the body without the workflow's neutralize step.
#
# Requires: the release PR branch checked out with push credentials, the
# release-please `pr` output JSON in $RELEASE_PR_JSON, and GH_TOKEN plus
# GITHUB_REPOSITORY for the body rewrite.
set -euo pipefail

"$(dirname -- "${BASH_SOURCE[0]}")/neutralize-release-pr-closing-keywords.sh"

RELEASE_PR_BRANCH=$(
  jq --exit-status --raw-output \
    '.headBranchName | select(type == "string" and length > 0)' \
    <<<"$RELEASE_PR_JSON"
)
release_version=$(tr -d '[:space:]' < version.txt)
if [[ -z "$release_version" ]]; then
  echo "Release version is empty" >&2
  exit 1
fi
toolchain=$(python3 -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
repository_root=$PWD
# The checkout's .cargo/config.toml replaces crates.io with pnpm-vendored
# sources, which `cargo update` refuses. Cargo reads config from its working
# directory, so resolve from outside the checkout with the pinned toolchain.
(cd / && cargo "+$toolchain" update --manifest-path "$repository_root/Cargo.toml" \
  -p tracedecay --precise "$release_version")
python3 scripts/bazel/gen_builds.py
pnpm --dir sdks/typescript run build
pnpm --dir plugin/chatgpt-extension run build

generated_paths=(
  Cargo.lock
  crates/tracedecay/BUILD.bazel
  crates/tracedecay-cli/BUILD.bazel
  crates/tracedecay-project/BUILD.bazel
  plugin/chatgpt-extension/embedded/app.html
  plugin/chatgpt-extension/embedded/server.mjs
)
unexpected_paths=$(git diff --name-only -- . "${generated_paths[@]/#/:(exclude)}")
if [[ -n "$unexpected_paths" ]]; then
  echo "Release metadata generation changed unexpected paths:" >&2
  echo "$unexpected_paths" >&2
  exit 1
fi

if git diff --quiet -- "${generated_paths[@]}"; then
  exit 0
fi

# The bot identity applies to this one commit only. `git config` would write
# the shared .git/config of every linked worktree and re-author the operator's
# later commits when the script runs locally.
bot_name="github-actions[bot]"
bot_email="41898282+github-actions[bot]@users.noreply.github.com"
git add "${generated_paths[@]}"
GIT_AUTHOR_NAME=$bot_name GIT_AUTHOR_EMAIL=$bot_email \
  GIT_COMMITTER_NAME=$bot_name GIT_COMMITTER_EMAIL=$bot_email \
  git commit -m "chore(release): update generated metadata"
git push origin "HEAD:$RELEASE_PR_BRANCH"
