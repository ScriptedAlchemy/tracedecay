#!/usr/bin/env bash
set -euo pipefail
umask 077

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
source_binary=${TRACEDECAY_DOGFOOD_SOURCE_BINARY:-"$target_dir/debug/tracedecay"}
build_identity_stamp="$target_dir/dogfood-build-identity.stamp"
dashboard_source_stamp="$target_dir/dogfood-dashboard-source.stamp"
stage_dir=${TRACEDECAY_DOGFOOD_STAGE_DIR:-"$HOME/.local/lib/tracedecay/dogfood"}
install_dir=${TRACEDECAY_DOGFOOD_INSTALL_DIR:-"$HOME/.local/bin"}
staged_binary="$stage_dir/tracedecay"
installed_binary="$install_dir/tracedecay"
profile_dir=${TRACEDECAY_DOGFOOD_PROFILE_DIR:-"$HOME/.tracedecay"}
verified_backup=${TRACEDECAY_DOGFOOD_BACKUP:-}
dogfood_started=$SECONDS

if [[ -n "${TRACEDECAY_SKIP_DASHBOARD_BUILD+x}" ]]; then
  printf '%s\n' \
    'dogfood refuses TRACEDECAY_SKIP_DASHBOARD_BUILD: dogfood must embed a fresh dashboard.' \
    'Unset TRACEDECAY_SKIP_DASHBOARD_BUILD and rerun cargo dogfood.' >&2
  exit 1
fi

report_stage() {
  local stage=$1
  local started=$2
  printf '[dogfood timing] stage=%s elapsed_s=%d\n' "$stage" "$((SECONDS - started))" >&2
}

refresh_build_identity_stamp() {
  local temporary

  mkdir -p "$target_dir" || return
  dogfood_build_identity_refresh="${SECONDS}-$$-$RANDOM"
  temporary=$(mktemp "${build_identity_stamp}.new.XXXXXX") || return
  if ! printf '%s\n' "$dogfood_build_identity_refresh" >"$temporary" ||
    ! mv -f -- "$temporary" "$build_identity_stamp"; then
    rm -f -- "$temporary"
    return 1
  fi
}

refresh_dashboard_source_stamp() {
  local temporary
  local writer

  mkdir -p "$target_dir" || return
  writer=$(mktemp "${target_dir}/dogfood-dashboard-stamp-writer.XXXXXX") || return
  temporary=$(mktemp "${dashboard_source_stamp}.new.XXXXXX") || {
    rm -f -- "$writer"
    return 1
  }
  if ! rustc --edition 2024 -O \
    "$repo_root/build-support/write_dashboard_stamp.rs" \
    -o "$writer" ||
    ! "$writer" "$repo_root" >"$temporary" ||
    [[ ! -s "$temporary" ]] ||
    ! mv -f -- "$temporary" "$dashboard_source_stamp"; then
    rm -f -- "$writer" "$temporary"
    return 1
  fi
  rm -f -- "$writer"
}

verify_dashboard_freshness() {
  local actual_stamp
  local expected_stamp

  if [[ ! -r "$dashboard_source_stamp" ]]; then
    printf 'dogfood dashboard freshness stamp is missing: %s\n' "$dashboard_source_stamp" >&2
    return 1
  fi
  if [[ ! -r "$repo_root/dashboard/app-dist/.source-stamp" ]] ||
    [[ ! -f "$repo_root/dashboard/app-dist/index.html" ]]; then
    printf 'dogfood dashboard bundle is missing its source stamp or entrypoint\n' >&2
    return 1
  fi
  expected_stamp=$(<"$dashboard_source_stamp")
  actual_stamp=$(<"$repo_root/dashboard/app-dist/.source-stamp")
  if [[ -z "$expected_stamp" || "$actual_stamp" != "$expected_stamp" ]]; then
    printf '%s\n' \
      'dogfood dashboard bundle does not match the freshly computed source stamp.' \
      'Rebuild the dashboard and dogfood binary, then rerun cargo dogfood.' >&2
    return 1
  fi
}

if [[ -L "$profile_dir" ]]; then
  printf 'dogfood profile directory must not be a symlink: %s\n' "$profile_dir" >&2
  exit 1
fi
mkdir -p "$profile_dir"
if [[ ! -d "$profile_dir" || ! -O "$profile_dir" ]]; then
  printf 'dogfood profile directory must be an owned directory: %s\n' "$profile_dir" >&2
  exit 1
fi
chmod 0700 "$profile_dir"

dogfood_lock="$profile_dir/dogfood.lock"
if [[ -L "$dogfood_lock" ]]; then
  printf 'dogfood lock must not be a symlink: %s\n' "$dogfood_lock" >&2
  exit 1
fi
exec {dogfood_lock_fd}>"$dogfood_lock"
chmod 0600 "$dogfood_lock"
flock -x "$dogfood_lock_fd"

cd "$repo_root"
if ! refresh_dashboard_source_stamp; then
  printf 'dogfood could not refresh dashboard source stamp: %s\n' \
    "$dashboard_source_stamp" >&2
  exit 1
fi
checkout_build_identity_early() {
  local sha status
  sha=$(git -C "$repo_root" rev-parse --short=12 HEAD) || return 1
  status=$(git -C "$repo_root" status --porcelain) || return 1
  if [[ -n "$status" ]]; then printf '%s.dirty' "$sha"; else printf '%s' "$sha"; fi
}

# Pin the checkout identity the build starts from. Concurrent workers commit
# to this checkout continuously, so the install-time checkout can differ from
# what the binary was faithfully built against; the verify step accepts either.
pinned_build_identity=$(checkout_build_identity_early || true)

stage_started=$SECONDS
if [[ -z "${TRACEDECAY_DOGFOOD_SOURCE_BINARY:-}" ]]; then
  # Default features only — never `--all-features` (enables test-transport).
  if ! refresh_build_identity_stamp; then
    printf 'dogfood could not refresh build identity stamp: %s\n' "$build_identity_stamp" >&2
    exit 1
  fi
  # Force build.rs to rerun and restamp the binary identity even when cargo's
  # env/stamp fingerprints consider the last build fresh (observed: a 0s no-op
  # build installed a binary stamped from a prior HEAD while concurrent workers
  # kept committing). build.rs watches this file via rerun-if-changed, so a
  # bumped mtime guarantees a restamp at the pinned checkout identity.
  touch "$repo_root/src/version/build_identity.rs"
  TRACEDECAY_DOGFOOD_BUILD_IDENTITY_STAMP="$build_identity_stamp" \
    TRACEDECAY_DOGFOOD_BUILD_IDENTITY_REFRESH="$dogfood_build_identity_refresh" \
    TRACEDECAY_DOGFOOD_DASHBOARD_STAMP_PATH="$dashboard_source_stamp" \
    cargo build --bin tracedecay
fi
report_stage dogfood-binary-build "$stage_started"

if [[ ! -x "$source_binary" ]]; then
  printf 'dogfood build did not produce %s\n' "$source_binary" >&2
  exit 1
fi

verify_dashboard_freshness

mkdir -p "$stage_dir" "$install_dir"

sync_path() {
  sync -f -- "$1"
}

checkout_build_identity() {
  local checkout_root
  local git_root
  local sha
  local status

  checkout_root=$(cd "$repo_root" && pwd -P) || {
    printf 'dogfood could not resolve checkout root: %s\n' "$repo_root" >&2
    return 1
  }
  git_root=$(git -C "$repo_root" rev-parse --show-toplevel) || {
    printf 'dogfood requires a Git worktree to verify the staged binary identity\n' >&2
    return 1
  }
  git_root=$(cd "$git_root" && pwd -P) || {
    printf 'dogfood could not resolve Git worktree root: %s\n' "$git_root" >&2
    return 1
  }
  if [[ "$git_root" != "$checkout_root" ]]; then
    printf 'dogfood checkout root is not the Git worktree root: %s\n' "$repo_root" >&2
    return 1
  fi
  sha=$(git -C "$repo_root" rev-parse --short=12 HEAD) || {
    printf 'dogfood could not resolve the checkout Git SHA\n' >&2
    return 1
  }
  status=$(git -C "$repo_root" status --porcelain) || {
    printf 'dogfood could not read the checkout dirty state\n' >&2
    return 1
  }
  if [[ -n "$status" ]]; then
    printf '%s.dirty' "$sha"
  else
    printf '%s' "$sha"
  fi
}

verify_binary_identity() {
  local binary=$1
  local expected_identity
  local reported_identity
  local reported_version

  expected_identity=$(checkout_build_identity) || return 1
  reported_version=$("$binary" --version) || {
    printf 'dogfood could not read candidate binary version: %s\n' "$binary" >&2
    return 1
  }
  case "$reported_version" in
    "tracedecay "*+*) reported_identity=${reported_version##*+} ;;
    *)
      printf 'dogfood candidate binary reported an invalid version: %s\n' "$reported_version" >&2
      return 1
      ;;
  esac
  # A dogfood build is an iteration build. The check that matters is the commit
  # SHA: a mismatch there means a stale binary is about to be installed. The
  # `.dirty` suffix must NOT fail the run — the build's own git probe can read a
  # transiently-dirty tree (a concurrent git index.lock, a build-time file
  # touch) even from a clean checkout, and an intentionally-dirty iteration
  # build is explicitly fine to dogfood. Concurrent workers also commit to this
  # checkout continuously, so the binary is accepted when its SHA matches
  # EITHER the identity pinned when the build started or the checkout at
  # install time — both mean the binary faithfully represents an identity this
  # dogfood invocation legitimately built.
  local reported_sha="${reported_identity%.dirty}"
  local expected_sha="${expected_identity%.dirty}"
  local pinned_sha="${pinned_build_identity%.dirty}"
  if [[ "$reported_sha" != "$expected_sha" && "$reported_sha" != "$pinned_sha" ]]; then
    printf '%s\n' \
      "dogfood candidate binary identity mismatch: expected $expected_identity (pinned $pinned_build_identity), got $reported_version." \
      'Force a fresh dogfood rebuild, then rerun cargo dogfood.' >&2
    return 1
  fi
  if [[ "$reported_identity" != "$expected_identity" ]]; then
    printf 'dogfood: installing %s (checkout now %s, pinned %s)\n' \
      "$reported_version" "$expected_identity" "$pinned_build_identity" >&2
  fi
}

install_atomically() {
  local source=$1
  local destination=$2
  local status
  local temporary
  temporary=$(mktemp "${destination}.new.XXXXXX")
  install -m 0755 "$source" "$temporary" || {
    status=$?
    rm -f -- "$temporary"
    return "$status"
  }
  sync_path "$temporary" || {
    status=$?
    rm -f -- "$temporary"
    return "$status"
  }
  mv -f "$temporary" "$destination" || {
    status=$?
    rm -f -- "$temporary"
    return "$status"
  }
  sync_path "$(dirname "$destination")"
}

preflight_started=$SECONDS

# Validate the exact tracked-integration refresh before daemon, store, or
# installed-binary mutation. Cargo-launched commands use an isolated
# development profile, but dogfood must inspect the real user integrations.
verify_binary_identity "$source_binary"
unset TRACEDECAY_DATA_DIR TRACEDECAY_DISABLE_GLOBAL_DB
integration_preflight_started=$SECONDS
if ! "$source_binary" reinstall --dry-run; then
  printf '%s\n' \
    'dogfood integration refresh preflight failed before installing the new binary.' \
    'No daemon, store, or installed binary was changed.' >&2
  exit 1
fi
report_stage integration-refresh-preflight "$integration_preflight_started"

# Owner-authorized fast path (2026-07-31): a plain `cp -a` profile copy with
# no manifest or rehearsal. The checksummed backup-profile path re-reads and
# re-writes the full profile twice; at current profile size that outlasts the
# maintenance window, so the owner accepted a plain copy as the recovery
# artifact. The copy must at least LOOK like a profile before we proceed.
if [[ -z "$verified_backup" ]]; then
  # Owner directive (2026-08-01): dogfood no longer requires a profile backup.
  # The forward-only boundary recovers forward on its own (retain the new
  # binary and rerun); a profile snapshot is optional insurance, not a gate.
  # When TRACEDECAY_DOGFOOD_BACKUP is set it is still validated and rehearsed.
  printf 'dogfood: no TRACEDECAY_DOGFOOD_BACKUP set; proceeding without a profile backup\n' >&2
elif [[ "${TRACEDECAY_DOGFOOD_BACKUP_PLAIN:-}" == 1 ]]; then
  if [[ ! -d "$verified_backup/profile" || ! -f "$verified_backup/profile/global.db" ]]; then
    printf '%s\n' \
      'TRACEDECAY_DOGFOOD_BACKUP_PLAIN=1 requires TRACEDECAY_DOGFOOD_BACKUP to' \
      'name a directory holding a plain profile copy at <backup>/profile.' >&2
    exit 1
  fi
elif [[ ! -d "$verified_backup" || ! -f "$verified_backup/backup-manifest.json" ]]; then
  printf '%s\n' \
    'TRACEDECAY_DOGFOOD_BACKUP names an incomplete backup (no backup-manifest.json).' \
    'Create one with tracedecay migrate backup-profile, or unset it to skip.' >&2
  exit 1
fi

candidate=$(mktemp "$stage_dir/tracedecay.candidate.XXXXXX")
previous_installed=
previous_staged=
had_installed=0
had_staged=0
replacement_active=0
boundary_reached=0
committed=0
active_child=
backup_rehearsal=

restore_path() {
  local backup=$1
  local had_previous=$2
  local destination=$3

  if ((had_previous)); then
    mv -f -- "$backup" "$destination"
  else
    rm -f -- "$destination"
  fi
}

terminate_active_child() {
  if [[ -n "$active_child" ]] && kill -0 "$active_child" 2>/dev/null; then
    kill -TERM "$active_child" 2>/dev/null || true
    wait "$active_child" 2>/dev/null || true
  fi
  active_child=
}

handle_signal() {
  local status=$1
  trap - HUP INT TERM
  terminate_active_child
  exit "$status"
}

run_new_binary() {
  local status

  "$installed_binary" "$@" &
  active_child=$!
  set +e
  wait "$active_child"
  status=$?
  set -e
  active_child=
  return "$status"
}

cleanup_install() {
  local status=$?
  local cleanup_binary=
  local cleanup_status=0

  trap - EXIT HUP INT TERM
  set +e
  terminate_active_child

  if ((replacement_active && ! committed)); then
    if ((boundary_reached)); then
      if [[ -x "$installed_binary" ]]; then
        cleanup_binary=$installed_binary
      elif [[ -x "$staged_binary" ]]; then
        cleanup_binary=$staged_binary
      elif [[ -x "$candidate" ]]; then
        cleanup_binary=$candidate
      fi

      if [[ -n "$cleanup_binary" ]]; then
        "$cleanup_binary" post-update --mode dogfood-recover-inactive ||
          cleanup_status=$?
      else
        cleanup_status=1
      fi
      printf '%s\n' \
        'Dogfood crossed the forward-only binary boundary and failed.' \
        'The previous binary was not restored or executed.' >&2
      if ((cleanup_status == 0)); then
        printf '%s\n' \
          'The managed daemon is disabled and inactive under the retained new service unit.' >&2
      else
        printf '%s\n' \
          'Automatic daemon stop failed; keep the managed service stopped before recovery.' >&2
      fi
      printf '%s\n' \
        'Recover forward: fix or build a newer binary, then rerun cargo dogfood.' \
        'Do not execute any prior TraceDecay binary against the live stores.' >&2
      printf 'New installed binary retained at %q\n' "$installed_binary" >&2
      printf 'New staged binary retained at %q\n' "$staged_binary" >&2
    else
      restore_path "$previous_installed" "$had_installed" "$installed_binary" ||
        cleanup_status=$?
      restore_path "$previous_staged" "$had_staged" "$staged_binary" ||
        cleanup_status=$?
      printf 'Dogfood failed before installing the new binary; restored prior binaries\n' >&2
    fi
  fi

  rm -f -- "$candidate"
  rm -f -- "$previous_installed" "$previous_staged"
  if [[ -n "$backup_rehearsal" ]]; then
    rm -rf -- "$backup_rehearsal"
  fi
  if ((cleanup_status != 0)); then
    printf 'Dogfood recovery action also failed with status %d (original status %d)\n' \
      "$cleanup_status" "$status" >&2
  fi
  exit "$status"
}
trap cleanup_install EXIT
trap 'handle_signal 129' HUP
trap 'handle_signal 130' INT
trap 'handle_signal 143' TERM

# A plain owner-authorized copy has no manifest to rehearse against; the
# checksummed path keeps its full restore-and-verify rehearsal.
if [[ -n "$verified_backup" && "${TRACEDECAY_DOGFOOD_BACKUP_PLAIN:-}" != 1 ]]; then
  backup_rehearsal=$(mktemp -d "$stage_dir/dogfood-backup-rehearsal.XXXXXX")
  rmdir -- "$backup_rehearsal"
  "$source_binary" migrate rehearse-profile-backup \
    --backup "$verified_backup" \
    --restore "$backup_rehearsal"
  rm -rf -- "$backup_rehearsal"
  backup_rehearsal=
fi

report_stage forward-boundary-preflight "$preflight_started"
install_started=$SECONDS
install -m 0755 "$source_binary" "$candidate"
if [[ -e "$installed_binary" || -L "$installed_binary" ]]; then
  previous_installed=$(mktemp "$install_dir/tracedecay.previous.XXXXXX")
  rm -f -- "$previous_installed"
  cp -a -- "$installed_binary" "$previous_installed"
  had_installed=1
fi
if [[ -e "$staged_binary" || -L "$staged_binary" ]]; then
  previous_staged=$(mktemp "$stage_dir/tracedecay.previous.XXXXXX")
  rm -f -- "$previous_staged"
  cp -a -- "$staged_binary" "$previous_staged"
  had_staged=1
fi
replacement_active=1
install_atomically "$candidate" "$staged_binary"
install_atomically "$candidate" "$installed_binary"
report_stage staged-binary-atomic-install "$install_started"

boundary_reached=1
post_update_args=(post-update --strict --mode dogfood-forward-only)
if [[ "${TRACEDECAY_DOGFOOD_NO_HEAL:-0}" == 1 ]]; then
  post_update_args+=(--no-heal)
fi
if [[ "${TRACEDECAY_DOGFOOD_NO_REINSTALL:-0}" == 1 ]]; then
  post_update_args+=(--no-reinstall)
fi
post_update_started=$SECONDS
run_new_binary "${post_update_args[@]}"
report_stage post-update "$post_update_started"

committed=1

printf 'Dogfood binary installed at %s\n' "$installed_binary"
printf 'Stable staged copy: %s\n' "$staged_binary"
report_stage total-installer "$dogfood_started"
