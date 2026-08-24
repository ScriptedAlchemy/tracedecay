#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/check-release-drift.sh"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

write_repo() {
  local version="$1"
  local path="$tmpdir/repo"
  rm -rf "$path"
  mkdir -p "$path"
  cat >"$path/Cargo.toml" <<TOML
[package]
name = "tracedecay"
version = "$version"
TOML
  printf '%s\n' "$path"
}

same_repo="$(write_repo 0.0.33)"
same_output="$("$SCRIPT" --repo "$same_repo" --release-version v0.0.33)"
[[ "$same_output" == *"release versions are aligned: 0.0.33"* ]]

alias_output="$("$SCRIPT" --repo "$same_repo" --registry-version 0.0.33)"
[[ "$alias_output" == *"release versions are aligned: 0.0.33"* ]]

fake_bin="$tmpdir/bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

[[ "$*" == *"https://api.github.com/repos/ScriptedAlchemy/tracedecay/releases/latest"* ]]
printf '%s\n' '{"tag_name":"v0.0.33"}'
SH
chmod +x "$fake_bin/curl"

default_output="$(PATH="$fake_bin:$PATH" "$SCRIPT" --repo "$same_repo")"
[[ "$default_output" == *"release versions are aligned: 0.0.33"* ]]

ahead_repo="$(write_repo 0.0.34)"
set +e
ahead_output="$("$SCRIPT" --repo "$ahead_repo" --release-version v0.0.33 2>&1)"
ahead_status=$?
set -e

[[ "$ahead_status" -eq 1 ]]
[[ "$ahead_output" == *"release drift detected: local Cargo.toml version 0.0.34 is ahead of GitHub release v0.0.33"* ]]
[[ "$ahead_output" == *"Reset the unpublished release bump so release automation can recreate it, or create GitHub release v0.0.34 manually before merging more release changes."* ]]
