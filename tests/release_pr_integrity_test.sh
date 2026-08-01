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
  printf 'version = 3\n' >"$repo/Cargo.lock"
  printf '# Changelog\n' >"$repo/CHANGELOG.md"
  git -C "$repo" add Cargo.toml Cargo.lock CHANGELOG.md
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
