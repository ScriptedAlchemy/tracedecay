#!/usr/bin/env bash
# This script is the lane's --workspace_status_command. It prints product
# provenance that //:product_git_sha and crates/tracedecay-cli's build
# script consume.
set -euo pipefail
echo "STABLE_PRODUCT_GIT_SHA $(git rev-parse HEAD)"
