#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
source_binary=${TRACEDECAY_DOGFOOD_SOURCE_BINARY:-"$target_dir/release/tracedecay"}
stage_dir=${TRACEDECAY_DOGFOOD_STAGE_DIR:-"$HOME/.local/lib/tracedecay/dogfood"}
install_dir=${TRACEDECAY_DOGFOOD_INSTALL_DIR:-"$HOME/.local/bin"}
staged_binary="$stage_dir/tracedecay"
installed_binary="$install_dir/tracedecay"

cd "$repo_root"
if [[ -z "${TRACEDECAY_DOGFOOD_SOURCE_BINARY:-}" ]]; then
  cargo build --release --all-features --bin tracedecay
fi

if [[ ! -x "$source_binary" ]]; then
  printf 'dogfood build did not produce %s\n' "$source_binary" >&2
  exit 1
fi

mkdir -p "$stage_dir" "$install_dir"

install_atomically() {
  local source=$1
  local destination=$2
  local temporary
  temporary=$(mktemp "${destination}.new.XXXXXX")
  trap 'rm -f "$temporary"' RETURN
  install -m 0755 "$source" "$temporary"
  mv -f "$temporary" "$destination"
  trap - RETURN
}

install_atomically "$source_binary" "$staged_binary"
install_atomically "$staged_binary" "$installed_binary"

# Cargo-launched commands use an isolated development profile. The staged
# executable must refresh the real user installation instead.
unset TRACEDECAY_DATA_DIR TRACEDECAY_DISABLE_GLOBAL_DB

"$installed_binary" post-update
daemon_status=$("$installed_binary" daemon status)
printf '%s\n' "$daemon_status"
if [[ "$daemon_status" != *"(connectable)"* ]]; then
  "$installed_binary" daemon restart
  "$installed_binary" daemon status
fi
"$installed_binary" doctor
"$installed_binary" --version

printf 'Dogfood binary installed at %s\n' "$installed_binary"
printf 'Stable staged copy: %s\n' "$staged_binary"
