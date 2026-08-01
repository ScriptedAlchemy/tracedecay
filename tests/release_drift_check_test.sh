#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=../scripts/lib/gate-test.sh
. "$(dirname "${BASH_SOURCE[0]}")/../scripts/lib/gate-test.sh"

SCRIPT="$GATE_REPO_ROOT/scripts/check-release-drift.sh"

write_repo() {
  local version="$1"
  local path="$GATE_SCRATCH/repo"
  rm -rf "$path"
  mkdir -p "$path"
  cat >"$path/Cargo.toml" <<TOML
[package]
name = "tracedecay"
version = "$version"
TOML
  printf '%s\n' "$path"
}

gate_run "$SCRIPT" --repo "$(write_repo 0.0.33)" --registry-version 0.0.33
gate_expect_success "aligned versions"
gate_output_contains "aligned versions" "release versions are aligned: 0.0.33"

gate_run "$SCRIPT" --repo "$(write_repo 0.0.34)" --registry-version 0.0.33
gate_expect_status "local ahead of registry" 1
gate_output_contains "local ahead of registry" \
  "release drift detected: local Cargo.toml version 0.0.34 is ahead of crates.io 0.0.33"
gate_output_contains "local ahead of registry" \
  "Reset the unpublished release bump so release-plz can recreate it, or publish 0.0.34 manually before merging more release changes."
