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

same_repo="$(write_repo 0.0.33)"
gate_run "$SCRIPT" --repo "$same_repo" --release-version v0.0.33
gate_expect_success "aligned versions"
gate_output_contains "aligned versions" "release versions are aligned: 0.0.33"

fake_bin="$GATE_SCRATCH/bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

case "$*" in
  *"https://api.github.com/repos/ScriptedAlchemy/tracedecay/releases/latest"*)
    printf '%s\n' '{"tag_name":"v0.0.33","draft":false,"prerelease":false}'
    ;;
  *"https://api.github.com/repos/ScriptedAlchemy/tracedecay/releases/tags/v0.1.0-beta.34"*)
    case "${FAKE_RELEASE_STATE:-published}" in
      draft)
        printf '%s\n' '{"tag_name":"v0.1.0-beta.34","draft":true,"prerelease":true}'
        ;;
      stable)
        printf '%s\n' '{"tag_name":"v0.1.0-beta.34","draft":false,"prerelease":false}'
        ;;
      published)
        printf '%s\n' '{"tag_name":"v0.1.0-beta.34","draft":false,"prerelease":true}'
        ;;
    esac
    ;;
  *)
    echo "unexpected GitHub release endpoint: $*" >&2
    exit 1
    ;;
esac
SH
chmod +x "$fake_bin/curl"

gate_run env PATH="$fake_bin:$PATH" "$SCRIPT" --repo "$same_repo"
gate_expect_success "GitHub release lookup"
gate_output_contains "GitHub release lookup" "release versions are aligned: 0.0.33"

prerelease_repo="$(write_repo 0.1.0-beta.34)"
gate_run env PATH="$fake_bin:$PATH" "$SCRIPT" --repo "$prerelease_repo"
gate_expect_success "published prerelease lookup"
gate_output_contains "published prerelease lookup" \
  "release versions are aligned: 0.1.0-beta.34"

gate_run env FAKE_RELEASE_STATE=draft PATH="$fake_bin:$PATH" \
  "$SCRIPT" --repo "$prerelease_repo"
gate_expect_status "draft prerelease" 1
gate_output_contains "draft prerelease" \
  "GitHub prerelease v0.1.0-beta.34 is not published"

gate_run env FAKE_RELEASE_STATE=stable PATH="$fake_bin:$PATH" \
  "$SCRIPT" --repo "$prerelease_repo"
gate_expect_status "release with wrong channel" 1
gate_output_contains "release with wrong channel" \
  "GitHub prerelease v0.1.0-beta.34 is not published"

ahead_repo="$(write_repo 0.0.34)"
gate_run "$SCRIPT" --repo "$ahead_repo" --release-version v0.0.33
gate_expect_status "local ahead of release" 1
gate_output_contains "local ahead of release" \
  "release drift detected: local Cargo.toml version 0.0.34 is ahead of GitHub release v0.0.33"
gate_output_contains "local ahead of release" \
  "Reset the unpublished release bump so release automation can recreate it, or create GitHub release v0.0.34 manually before merging more release changes."
