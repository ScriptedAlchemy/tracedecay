#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

npm ci
git config core.hooksPath .githooks
chmod +x .githooks/commit-msg

echo "Installed repository Git hooks via core.hooksPath=.githooks"
echo "Commit messages will be checked with npm run lint:commit"
