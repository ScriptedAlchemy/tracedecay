#!/usr/bin/env bash
# Refreshes the root Cargo.lock on a release-please release PR so the lockfile
# matches the bumped crate version, committing and pushing only when the
# update changed Cargo.lock and touched nothing else. Shared by the stable and
# beta release-please workflows so the branch extraction, drift guard, and bot
# commit cannot diverge between channels.
#
# Requires: the release PR branch checked out with push credentials, and the
# release-please `pr` output JSON in $RELEASE_PR_JSON.
set -euo pipefail

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

if git diff --quiet -- Cargo.lock; then
  exit 0
fi

unexpected_paths=$(git diff --name-only -- . ':(exclude)Cargo.lock')
if [[ -n "$unexpected_paths" ]]; then
  echo "Cargo metadata changed unexpected paths:" >&2
  echo "$unexpected_paths" >&2
  exit 1
fi

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Cargo.lock
git commit -m "chore(release): update root lockfile"
git push origin "HEAD:$RELEASE_PR_BRANCH"
