#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

git config core.hooksPath .githooks
chmod +x .githooks/commit-msg scripts/check-conventional-commits.sh

echo "Installed repository Git hooks via core.hooksPath=.githooks"
echo "Commit messages will be checked with scripts/check-conventional-commits.sh"
