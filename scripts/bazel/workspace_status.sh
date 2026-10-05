#!/usr/bin/env bash
set -euo pipefail

sha=$(git rev-parse HEAD)
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
  sha="${sha}.dirty"
fi
printf 'STABLE_PRODUCT_GIT_SHA %s\n' "$sha"
