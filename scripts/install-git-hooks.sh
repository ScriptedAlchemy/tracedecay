#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

pnpm install
# Written to the shared repository config, so every linked worktree inherits
# it; the relative path resolves against each worktree's own root.
git config core.hooksPath .githooks
chmod +x .githooks/commit-msg .githooks/pre-commit

echo "Installed repository Git hooks via core.hooksPath=.githooks"
echo "Commit messages will be checked with pnpm run lint:commit"
