#!/usr/bin/env bash
set -euo pipefail

echo '::group::Build filesystem capacity'
df -h "$GITHUB_WORKSPACE" "$HOME" "$RUNNER_TEMP"
df -i "$GITHUB_WORKSPACE"
echo '::endgroup::'
echo '::group::Build outputs, caches, and temporary files'
for path in "$GITHUB_WORKSPACE/bazel-out/" "$HOME/.cache" "$RUNNER_TEMP"; do
  if [[ -d "$path" ]]; then
    du -sh "$path"
  fi
done
echo '::endgroup::'
