#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
source_binary=${TRACEDECAY_DOGFOOD_SOURCE_BINARY:-"$target_dir/release/tracedecay"}
stage_dir=${TRACEDECAY_DOGFOOD_STAGE_DIR:-"$HOME/.local/lib/tracedecay/dogfood"}
install_dir=${TRACEDECAY_DOGFOOD_INSTALL_DIR:-"$HOME/.local/bin"}
staged_binary="$stage_dir/tracedecay"
installed_binary="$install_dir/tracedecay"
profile_dir=${TRACEDECAY_DOGFOOD_PROFILE_DIR:-"$HOME/.tracedecay"}

mkdir -p "$profile_dir"
exec {dogfood_lock_fd}>"$profile_dir/dogfood.lock"
flock -x "$dogfood_lock_fd"

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

candidate=$(mktemp "$stage_dir/tracedecay.candidate.XXXXXX")
previous_runtime=$(command -v tracedecay || true)
previous_installed=
previous_staged=
had_installed=0
had_staged=0
replacement_active=0
post_update_started=0
committed=0

restore_path() {
  local backup=$1
  local had_previous=$2
  local destination=$3

  if ((had_previous)); then
    mv -f "$backup" "$destination"
  else
    rm -f "$destination"
  fi
}

cleanup_install() {
  local status=$?
  local rollback_binary=
  local rollback_status=0

  trap - EXIT HUP INT TERM
  set +e

  if ((replacement_active && ! committed)); then
    if ((had_installed || had_staged)) || [[ -n "$previous_runtime" ]]; then
      restore_path "$previous_installed" "$had_installed" "$installed_binary" || rollback_status=$?
      restore_path "$previous_staged" "$had_staged" "$staged_binary" || rollback_status=$?
      if ((had_installed)); then
        rollback_binary=$installed_binary
      elif ((had_staged)); then
        rollback_binary=$staged_binary
      elif [[ -x "$previous_runtime" ]]; then
        rollback_binary=$previous_runtime
      fi
      if ((post_update_started)) && [[ -n "$rollback_binary" ]]; then
        PATH="$(dirname "$rollback_binary"):$PATH" \
          "$rollback_binary" post-update --strict || rollback_status=$?
      fi
      printf 'Dogfood validation failed; restored previous installation\n' >&2
    else
      if ((post_update_started)); then
        "$candidate" uninstall || rollback_status=$?
        "$candidate" daemon uninstall-service || rollback_status=$?
      fi
      restore_path "$previous_installed" 0 "$installed_binary" || rollback_status=$?
      restore_path "$previous_staged" 0 "$staged_binary" || rollback_status=$?
      printf 'Dogfood validation failed; removed first-time installation\n' >&2
    fi
  fi

  rm -f "$candidate"
  rm -f "$previous_installed" "$previous_staged"
  if ((rollback_status != 0)); then
    printf 'Dogfood rollback also failed with status %d (original status %d)\n' \
      "$rollback_status" "$status" >&2
  fi
  exit "$status"
}
trap cleanup_install EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

install -m 0755 "$source_binary" "$candidate"
if [[ -e "$installed_binary" || -L "$installed_binary" ]]; then
  previous_installed=$(mktemp "$install_dir/tracedecay.previous.XXXXXX")
  cp -p "$installed_binary" "$previous_installed"
  had_installed=1
fi
if [[ -e "$staged_binary" || -L "$staged_binary" ]]; then
  previous_staged=$(mktemp "$stage_dir/tracedecay.previous.XXXXXX")
  cp -p "$staged_binary" "$previous_staged"
  had_staged=1
fi
replacement_active=1
install_atomically "$candidate" "$installed_binary"

# Cargo-launched commands use an isolated development profile. The staged
# executable must refresh the real user installation instead.
unset TRACEDECAY_DATA_DIR TRACEDECAY_DISABLE_GLOBAL_DB

post_update_started=1
"$installed_binary" post-update --strict
"$installed_binary" doctor
"$installed_binary" --version

install_atomically "$candidate" "$staged_binary"
committed=1

printf 'Dogfood binary installed at %s\n' "$installed_binary"
printf 'Stable staged copy: %s\n' "$staged_binary"
