#!/usr/bin/env bash
# --workspace_status_command for the Bazel lane: product provenance consumed
# by //:product_git_sha and crates/tracedecay-cli's build script.
set -euo pipefail
echo "STABLE_PRODUCT_GIT_SHA $(git rev-parse HEAD)"
