#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
guard="$repo_root/scripts/check-release-pr-integrity.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fail() {
  echo "$1" >&2
  exit 1
}

new_repo() {
  rm -rf "$fixture/repo"
  mkdir -p "$fixture/repo"
  git -C "$fixture/repo" init -q -b master
  git -C "$fixture/repo" config user.name "Release Guard Test"
  git -C "$fixture/repo" config user.email "release-guard@example.com"
  printf '[package]\nname = "fixture"\nversion = "0.1.0"\n' >"$fixture/repo/Cargo.toml"
  printf 'version = 3\n' >"$fixture/repo/Cargo.lock"
  printf '# Changelog\n' >"$fixture/repo/CHANGELOG.md"
  printf '0.1.0\n' >"$fixture/repo/version.txt"
  printf '{".":"0.1.0"}\n' >"$fixture/repo/.release-please-manifest.json"
  git -C "$fixture/repo" add \
    .release-please-manifest.json \
    Cargo.toml \
    Cargo.lock \
    CHANGELOG.md \
    version.txt
  git -C "$fixture/repo" commit -qm "initial"
}

commit_all() {
  git -C "$fixture/repo" add -A
  git -C "$fixture/repo" commit -qm "$1"
}

run_guard() {
  (cd "$fixture/repo" && "$guard" "$@")
}

new_repo
base=$(git -C "$fixture/repo" rev-parse HEAD)
printf '\n## 0.2.0\n' >>"$fixture/repo/CHANGELOG.md"
printf '[package]\nname = "fixture"\nversion = "0.2.0"\n' >"$fixture/repo/Cargo.toml"
printf '0.2.0\n' >"$fixture/repo/version.txt"
printf '{".":"0.2.0"}\n' >"$fixture/repo/.release-please-manifest.json"
commit_all "release"
head=$(git -C "$fixture/repo" rev-parse HEAD)
run_guard "$base" "$head"

new_repo
base=$(git -C "$fixture/repo" rev-parse HEAD)
mkdir -p "$fixture/repo/src"
printf 'pub fn unexpected() {}\n' >"$fixture/repo/src/lib.rs"
commit_all "unexpected source change"
head=$(git -C "$fixture/repo" rev-parse HEAD)
if output=$(run_guard "$base" "$head" 2>&1); then
  fail "unexpected source changes must fail without explicit approval"
fi
[[ "$output" == *"src/lib.rs"* ]] || fail "failure must name the unexpected path"
run_guard "$base" "$head" --allow-extra-files >/dev/null 2>&1

new_repo
base=$(git -C "$fixture/repo" rev-parse HEAD)
rm "$fixture/repo/Cargo.toml"
commit_all "delete manifest"
head=$(git -C "$fixture/repo" rev-parse HEAD)
if output=$(run_guard "$base" "$head" --allow-extra-files 2>&1); then
  fail "approval must not permit deletion of release metadata"
fi
[[ "$output" == *"Cargo.toml"* ]] || fail "destructive metadata failure must name Cargo.toml"

new_repo
printf 'tracked.tmp\n' >"$fixture/repo/.gitignore"
printf 'must remain visible to release tooling\n' >"$fixture/repo/tracked.tmp"
git -C "$fixture/repo" add .gitignore
git -C "$fixture/repo" add -f tracked.tmp
git -C "$fixture/repo" commit -qm "track ignored file"
head=$(git -C "$fixture/repo" rev-parse HEAD)
if output=$(run_guard "$head" "$head" --allow-extra-files 2>&1); then
  fail "tracked ignored files must fail even with extra-file approval"
fi
[[ "$output" == *"tracked.tmp"* ]] || fail "tracked ignored failure must name the path"
