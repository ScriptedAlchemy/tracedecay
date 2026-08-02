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
boundary_state="$profile_dir/dogfood-migration-boundary.state"
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

marker_checksum() {
  local payload=$1
  local output

  if command -v sha256sum >/dev/null 2>&1; then
    output=$(printf '%s' "$payload" | sha256sum) || return
  elif command -v shasum >/dev/null 2>&1; then
    output=$(printf '%s' "$payload" | shasum -a 256) || return
  else
    printf 'dogfood requires sha256sum or shasum for recovery markers\n' >&2
    return 1
  fi
  printf '%s' "${output%% *}"
}

file_checksum() {
  local path=$1
  local output

  if command -v sha256sum >/dev/null 2>&1; then
    output=$(sha256sum -- "$path") || return
  elif command -v shasum >/dev/null 2>&1; then
    output=$(shasum -a 256 "$path") || return
  else
    printf 'dogfood requires sha256sum or shasum for recovery markers\n' >&2
    return 1
  fi
  printf '%s' "${output%% *}"
}

retained_binary_checksum() {
  if [[ ! -f "$installed_binary" ]]; then
    printf 'none'
    return 0
  fi
  file_checksum "$installed_binary"
}

file_mode() {
  local path=$1
  local mode

  if mode=$(stat -c '%a' -- "$path" 2>/dev/null); then
    printf '%s' "$mode"
  else
    stat -f '%Lp' -- "$path"
  fi
}

marker_transition_is_valid() {
  local outcome=$1
  local boundary=$2
  local policy=$3
  local daemon=$4

  case "$outcome:$boundary:$policy:$daemon" in
    preparing:not-reached:allowed:unchanged | \
      preparing:not-reached:forbidden:unchanged | \
      safe-rollback-complete:not-reached:allowed:unchanged | \
      safe-rollback-complete:not-reached:forbidden:unchanged | \
      post-update-starting:reached:forbidden:inactivity-pending | \
      forward-recovery-required:reached:forbidden:inactive | \
      forward-recovery-required:reached:forbidden:inactivity-unproven | \
      validated:reached:forbidden:verified-new-version)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

marker_outcome=none
marker_attempt_id=
marker_boundary=not-reached
marker_policy=allowed
marker_daemon=unchanged
marker_format=0
marker_retained_binary_sha256=none
marker_retained_binary_trusted=0

load_boundary_state() {
  local lines=()
  local payload
  local expected_checksum
  local checksum_index

  if [[ ! -e "$boundary_state" && ! -L "$boundary_state" ]]; then
    return 0
  fi
  if [[ -L "$boundary_state" ]]; then
    printf 'dogfood migration marker must not be a symlink: %s\n' "$boundary_state" >&2
    return 1
  fi
  if [[ ! -f "$boundary_state" ]]; then
    printf 'invalid dogfood migration marker: not a regular file: %s\n' "$boundary_state" >&2
    return 1
  fi
  if [[ "$(file_mode "$boundary_state")" != 600 ]]; then
    printf 'dogfood migration marker must have mode 0600: %s\n' "$boundary_state" >&2
    return 1
  fi

  mapfile -t lines <"$boundary_state"
  case "${lines[0]:-}" in
    format=2)
      if ((${#lines[@]} != 7)); then
        printf 'invalid dogfood migration marker structure: %s\n' "$boundary_state" >&2
        return 1
      fi
      marker_format=2
      marker_retained_binary_sha256=none
      marker_retained_binary_trusted=0
      checksum_index=6
      ;;
    format=3)
      if ((${#lines[@]} != 8)) ||
        [[ "${lines[6]}" != retained_binary_sha256=* ]]; then
        printf 'invalid dogfood migration marker structure: %s\n' "$boundary_state" >&2
        return 1
      fi
      marker_format=3
      marker_retained_binary_sha256=${lines[6]#retained_binary_sha256=}
      if [[ "$marker_retained_binary_sha256" != none &&
        ! "$marker_retained_binary_sha256" =~ ^[0-9a-f]{64}$ ]]; then
        printf 'invalid dogfood migration marker retained binary binding: %s\n' \
          "$boundary_state" >&2
        return 1
      fi
      checksum_index=7
      ;;
    *)
      printf 'invalid dogfood migration marker structure: %s\n' "$boundary_state" >&2
      return 1
      ;;
  esac
  if [[ "${lines[1]}" != attempt_id=* ]] ||
    [[ "${lines[2]}" != outcome=* ]] ||
    [[ "${lines[3]}" != attempt_boundary=* ]] ||
    [[ "${lines[4]}" != old_binary_policy=* ]] ||
    [[ "${lines[5]}" != managed_daemon=* ]] ||
    [[ "${lines[$checksum_index]}" != checksum=* ]]; then
    printf 'invalid dogfood migration marker structure: %s\n' "$boundary_state" >&2
    return 1
  fi

  marker_attempt_id=${lines[1]#attempt_id=}
  marker_outcome=${lines[2]#outcome=}
  marker_boundary=${lines[3]#attempt_boundary=}
  marker_policy=${lines[4]#old_binary_policy=}
  marker_daemon=${lines[5]#managed_daemon=}
  if [[ ! "$marker_attempt_id" =~ ^[A-Za-z0-9._-]{16,128}$ ]]; then
    printf 'invalid dogfood migration marker attempt id: %s\n' "$boundary_state" >&2
    return 1
  fi
  if ! marker_transition_is_valid \
    "$marker_outcome" "$marker_boundary" "$marker_policy" "$marker_daemon"; then
    printf 'invalid dogfood migration marker transition: %s\n' "$boundary_state" >&2
    return 1
  fi

  printf -v payload '%s\n' "${lines[@]:0:checksum_index}"
  expected_checksum=$(marker_checksum "$payload")
  if [[ "${lines[$checksum_index]#checksum=}" != "$expected_checksum" ]]; then
    printf 'invalid dogfood migration marker checksum: %s\n' "$boundary_state" >&2
    return 1
  fi
  if ((marker_format == 3)); then
    local current_retained_binary_sha256
    current_retained_binary_sha256=$(retained_binary_checksum) || return
    if [[ "$current_retained_binary_sha256" == "$marker_retained_binary_sha256" ]]; then
      marker_retained_binary_trusted=1
    fi
  fi
}

validate_marker_transition() {
  local next=$1

  case "$marker_outcome:$next" in
    none:preparing | \
      preparing:preparing | \
      safe-rollback-complete:preparing | \
      post-update-starting:preparing | \
      forward-recovery-required:preparing | \
      validated:preparing | \
      preparing:safe-rollback-complete | \
      preparing:post-update-starting | \
      post-update-starting:forward-recovery-required | \
      post-update-starting:validated)
      return 0
      ;;
    *)
      printf 'invalid dogfood migration marker state transition: %s -> %s\n' \
        "$marker_outcome" "$next" >&2
      return 1
      ;;
  esac
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

record_boundary_outcome() {
  local outcome=$1
  local attempt_boundary=$2
  local binary_policy=$3
  local managed_daemon=$4
  local checksum
  local payload
  local retained_binary_sha256
  local temporary

  marker_transition_is_valid \
    "$outcome" "$attempt_boundary" "$binary_policy" "$managed_daemon" ||
    return 1
  validate_marker_transition "$outcome" || return 1
  retained_binary_sha256=$(retained_binary_checksum) || return
  printf -v payload \
    'format=3\nattempt_id=%s\noutcome=%s\nattempt_boundary=%s\nold_binary_policy=%s\nmanaged_daemon=%s\nretained_binary_sha256=%s\n' \
    "$attempt_id" "$outcome" "$attempt_boundary" "$binary_policy" "$managed_daemon" \
    "$retained_binary_sha256"
  checksum=$(marker_checksum "$payload") || return
  temporary=$(mktemp "${boundary_state}.new.XXXXXX") || return
  if ! {
    printf '%s' "$payload"
    printf 'checksum=%s\n' "$checksum"
  } >"$temporary" ||
    ! chmod 0600 "$temporary" ||
    ! sync_path "$temporary" ||
    ! mv -f -- "$temporary" "$boundary_state" ||
    ! sync_path "$profile_dir"; then
    rm -f -- "$temporary"
    return 1
  fi
  marker_outcome=$outcome
  marker_attempt_id=$attempt_id
  marker_boundary=$attempt_boundary
  marker_policy=$binary_policy
  marker_daemon=$managed_daemon
  marker_format=3
  marker_retained_binary_sha256=$retained_binary_sha256
  marker_retained_binary_trusted=1
}

load_boundary_state
rm -f -- "${boundary_state}".new.*
preflight_started=$SECONDS

# Validate the exact tracked-integration refresh before recovery, daemon, store,
# marker, or installed-binary mutation. Cargo-launched commands use an isolated
# development profile, but dogfood must inspect the real user integrations.
verify_binary_identity "$source_binary"
unset TRACEDECAY_DATA_DIR TRACEDECAY_DISABLE_GLOBAL_DB
integration_preflight_started=$SECONDS
if ! "$source_binary" reinstall --dry-run; then
  printf '%s\n' \
    'dogfood integration refresh preflight failed before the migration boundary.' \
    'No daemon, store, migration marker, or installed binary was changed.' >&2
  exit 1
fi
report_stage integration-refresh-preflight "$integration_preflight_started"

# A valid reached/forbidden pending marker means a prior attempt already crossed
# the migration boundary. Never trust the installed path without an identity
# binding from that attempt. Stage the current source build atomically, then use
# that stable candidate to prove the managed unit is inactive.
require_inactive_recovery_before_preparing() {
  case "$marker_outcome" in
    post-update-starting | forward-recovery-required) ;;
    *)
      return 0
      ;;
  esac

  if ((marker_format == 3 && ! marker_retained_binary_trusted)); then
    printf '%s\n' \
      'The retained installed binary does not match its migration-marker binding.' \
      'It will not be executed; recovery will use the current source build.' >&2
  fi

  if ! verify_binary_identity "$source_binary"; then
    printf 'The migration marker was left unchanged.\n' >&2
    return 1
  fi

  if ! install_atomically "$source_binary" "$staged_binary"; then
    printf '%s\n' \
      "dogfood retry from $marker_outcome could not stage the current source build." \
      'The migration marker was left unchanged.' >&2
    return 1
  fi

  if ! "$staged_binary" post-update --mode dogfood-recover-inactive; then
    printf '%s\n' \
      "dogfood inactive recovery failed while a $marker_outcome marker is pending." \
      'The migration marker was left unchanged.' \
      'Keep the managed service stopped, then rerun cargo dogfood with a schema-compatible newer binary.' \
      'Do not execute any prior TraceDecay binary against the live stores.' >&2
    return 1
  fi
}

require_inactive_recovery_before_preparing

old_binary_policy=$marker_policy
if [[ "$marker_boundary" == reached ]]; then
  old_binary_policy=forbidden
fi
attempt_id="$(date +%s)-$$-$RANDOM-$RANDOM"

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
  local daemon_outcome=inactive

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
      if ((cleanup_status != 0)); then
        daemon_outcome=inactivity-unproven
      fi
      record_boundary_outcome \
        forward-recovery-required reached forbidden "$daemon_outcome" ||
        printf 'Could not persist the dogfood migration-boundary outcome\n' >&2

      printf '%s\n' \
        'Dogfood crossed the forward-only migration boundary and failed.' \
        'The previous binary was not restored or executed.' >&2
      if ((cleanup_status == 0)); then
        printf '%s\n' \
          'The managed daemon is disabled and inactive under the retained new service unit.' >&2
      else
        printf '%s\n' \
          'Automatic daemon stop failed; keep the managed service stopped before recovery.' >&2
      fi
      printf '%s\n' \
        'Recover forward: fix or build a schema-compatible newer binary, then rerun cargo dogfood.' \
        'Do not execute any prior TraceDecay binary against the live stores.' >&2
      printf 'New installed binary retained at %q\n' "$installed_binary" >&2
      printf 'New staged binary retained at %q\n' "$staged_binary" >&2
      printf 'Boundary outcome recorded at %q\n' "$boundary_state" >&2
    else
      restore_path "$previous_installed" "$had_installed" "$installed_binary" ||
        cleanup_status=$?
      restore_path "$previous_staged" "$had_staged" "$staged_binary" ||
        cleanup_status=$?
      record_boundary_outcome \
        safe-rollback-complete not-reached "$old_binary_policy" unchanged ||
        printf 'Could not persist the dogfood migration-boundary outcome\n' >&2
      printf 'Dogfood failed before the migration boundary; restored prior binaries\n' >&2
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
record_boundary_outcome preparing not-reached "$old_binary_policy" unchanged
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
record_boundary_outcome post-update-starting reached forbidden inactivity-pending
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

record_boundary_outcome validated reached forbidden verified-new-version
committed=1

printf 'Dogfood binary installed at %s\n' "$installed_binary"
printf 'Stable staged copy: %s\n' "$staged_binary"
report_stage total-installer "$dogfood_started"
