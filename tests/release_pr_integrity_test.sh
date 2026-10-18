#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=../scripts/lib/gate-test.sh
. "$(dirname "${BASH_SOURCE[0]}")/../scripts/lib/gate-test.sh"

guard="$GATE_REPO_ROOT/scripts/check-release-pr-integrity.sh"
repo="$GATE_SCRATCH/repo"

new_repo() {
  rm -rf "$repo"
  mkdir -p "$repo"
  git -C "$repo" init -q -b master
  git -C "$repo" config user.name "Release Guard Test"
  git -C "$repo" config user.email "release-guard@example.com"
  printf '[package]\nname = "fixture"\nversion = "0.1.0"\n' >"$repo/Cargo.toml"
  printf 'version = 3\n\n[[package]]\nname = "fixture"\nversion = "0.1.0"\n' >"$repo/Cargo.lock"
  printf '# Changelog\n' >"$repo/CHANGELOG.md"
  printf '0.1.0\n' >"$repo/version.txt"
  printf '{"version":"0.1.0"}\n' >"$repo/server.json"
  printf '{".":"0.1.0"}\n' >"$repo/.release-please-manifest.json"
  git -C "$repo" add \
    .release-please-manifest.json \
    Cargo.toml \
    Cargo.lock \
    CHANGELOG.md \
    server.json \
    version.txt
  git -C "$repo" commit -qm "initial"
}

commit_all() {
  git -C "$repo" add -A
  git -C "$repo" commit -qm "$1"
}

head_sha() {
  git -C "$repo" rev-parse HEAD
}

run_guard() {
  gate_run bash -c 'cd "$1" && shift && "$@"' _ "$repo" "$guard" "$@"
}

new_repo
base=$(head_sha)
printf '\n## 0.2.0\n' >>"$repo/CHANGELOG.md"
printf '[package]\nname = "fixture"\nversion = "0.2.0"\n' >"$repo/Cargo.toml"
printf 'version = 3\n\n[[package]]\nname = "fixture"\nversion = "0.2.0"\n' >"$repo/Cargo.lock"
printf '0.2.0\n' >"$repo/version.txt"
printf '{"version":"0.2.0"}\n' >"$repo/server.json"
printf '{".":"0.2.0"}\n' >"$repo/.release-please-manifest.json"
commit_all "release"
run_guard "$base" "$(head_sha)"
gate_expect_success "release-only change"

new_repo
base=$(head_sha)
mkdir -p "$repo/src"
printf 'pub fn unexpected() {}\n' >"$repo/src/lib.rs"
commit_all "unexpected source change"
head=$(head_sha)
run_guard "$base" "$head"
gate_expect_failure "unexpected source changes must fail without explicit approval"
gate_output_contains "unexpected source change" "src/lib.rs"
run_guard "$base" "$head" --allow-extra-files
gate_expect_success "explicit approval accepts extra files"

new_repo
base=$(head_sha)
rm "$repo/Cargo.toml"
commit_all "delete manifest"
run_guard "$base" "$(head_sha)" --allow-extra-files
gate_expect_failure "approval must not permit deletion of release metadata"
gate_output_contains "delete manifest" "Cargo.toml"

new_repo
printf 'tracked.tmp\n' >"$repo/.gitignore"
printf 'must remain visible to release tooling\n' >"$repo/tracked.tmp"
git -C "$repo" add .gitignore
git -C "$repo" add -f tracked.tmp
git -C "$repo" commit -qm "track ignored file"
head=$(head_sha)
run_guard "$head" "$head" --allow-extra-files
gate_expect_failure "tracked ignored files must fail even with extra-file approval"
gate_output_contains "tracked ignored file" "tracked.tmp"

# The master break after the 1.0.0-beta.56 release merge: the workspace
# version moved but the lock still recorded the inheriting members at beta.55.
write_workspace_release() {
  local lock_version=$1
  mkdir -p "$repo/crates/tracedecay" "$repo/crates/tracedecay-api"
  printf '[workspace]\nmembers = ["crates/tracedecay", "crates/tracedecay-api"]\n\n[workspace.package]\nversion = "1.0.0-beta.56"\n' >"$repo/Cargo.toml"
  printf '[package]\nname = "tracedecay"\nversion.workspace = true\n' >"$repo/crates/tracedecay/Cargo.toml"
  printf '[package]\nname = "tracedecay-api"\nversion = "0.1.0"\n' >"$repo/crates/tracedecay-api/Cargo.toml"
  printf 'version = 4\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n\n[[package]]\nname = "tracedecay"\nversion = "%s"\n\n[[package]]\nname = "tracedecay-api"\nversion = "0.1.0"\n' "$lock_version" >"$repo/Cargo.lock"
  commit_all "workspace release with lock at $lock_version"
}

new_repo
base=$(head_sha)
write_workspace_release 1.0.0-beta.55
run_guard "$base" "$(head_sha)" --allow-extra-files
gate_expect_failure "a lock lagging the workspace version must fail"
gate_output_contains "lagging lock" "tracedecay: Cargo.toml 1.0.0-beta.56, Cargo.lock 1.0.0-beta.55"

new_repo
base=$(head_sha)
write_workspace_release 1.0.0-beta.56
run_guard "$base" "$(head_sha)" --allow-extra-files
gate_expect_success "a lock synced to the workspace version"
