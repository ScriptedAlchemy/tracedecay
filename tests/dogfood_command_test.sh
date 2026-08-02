#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dogfood_script="$repo_root/scripts/dogfood.sh"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fail() {
  printf 'dogfood contract failure: %s\n' "$*" >&2
  exit 1
}

write_fake_binary() {
  local path=$1
  local binary_id=$2
  mkdir -p "$(dirname "$path")"
  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n'
    printf 'binary_id=%q\n' "$binary_id"
    printf 'checkout_root=%q\n' "$repo_root"
    cat <<'EOF'
if [[ "${1:-}" == "--version" ]]; then
  sha=$(git -C "$checkout_root" rev-parse --short=12 HEAD)
  status=$(git -C "$checkout_root" status --porcelain)
  dirty=
  if [[ -n "$status" ]]; then
    dirty=.dirty
  fi
  reported_version=${TRACEDECAY_DOGFOOD_TEST_REPORTED_VERSION:-"0.0.0+$sha$dirty"}
  printf 'tracedecay %s\n' "$reported_version"
  exit 0
fi

command_text=$*
if [[ "$command_text" == "reinstall --dry-run" ]]; then
  if [[ -n "${TRACEDECAY_DOGFOOD_TEST_PREFLIGHT_MARKER:-}" ]]; then
    : >"$TRACEDECAY_DOGFOOD_TEST_PREFLIGHT_MARKER"
  fi
  if [[ "${TRACEDECAY_DOGFOOD_TEST_FAIL_PREFLIGHT:-0}" == 1 ]]; then
    printf 'cline: registration config is corrupt\n' >&2
    exit 42
  fi
  exit 0
fi

if [[ "$command_text" == migrate\ rehearse-profile-backup* ]]; then
  exit 0
fi

printf '%s:%s\n' "$binary_id" "$command_text" \
  >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"

if [[ "$binary_id" == "${TRACEDECAY_DOGFOOD_TEST_OPEN_BINARY:-}" \
  && "$command_text" == "${TRACEDECAY_DOGFOOD_TEST_OPEN_COMMAND:-}" ]]; then
  : >"${TRACEDECAY_DOGFOOD_TEST_SCHEMA_MARKER:?}"
  : >"${TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER:?}"
fi

if [[ "$command_text" == "post-update --strict --mode dogfood-forward-only" \
  && -n "${TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER:-}" ]]; then
  : >"$TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER"
fi

if [[ "$binary_id" == "${TRACEDECAY_DOGFOOD_TEST_HOLD_BINARY:-}" \
  && "$command_text" == "${TRACEDECAY_DOGFOOD_TEST_HOLD_COMMAND:-}" ]]; then
  on_signal() {
    printf '%s:interrupted\n' "$binary_id" \
      >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"
    if [[ -n "${TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER:-}" ]]; then
      rm -f "$TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER"
    fi
    exit 143
  }
  trap on_signal HUP INT TERM
  : >"${TRACEDECAY_DOGFOOD_TEST_HOLD_MARKER:?}"
  while [[ ! -e "${TRACEDECAY_DOGFOOD_TEST_HOLD_RELEASE:?}" ]]; do
    sleep 0.02
  done
fi

# Fail injections run before inactive recovery clears the daemon marker so a
# failed recover-inactive leaves the simulated service active.
if [[ "$binary_id" == "${TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY:-}" \
  && "$command_text" == "${TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND:-}" ]]; then
  if [[ "$command_text" == "post-update --strict --mode dogfood-forward-only" \
    && -n "${TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER:-}" ]]; then
    rm -f "$TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER"
  fi
  if [[ -n "${TRACEDECAY_DOGFOOD_TEST_FAIL_MESSAGE:-}" ]]; then
    printf '%s\n' "$TRACEDECAY_DOGFOOD_TEST_FAIL_MESSAGE" >&2
  fi
  exit 42
fi

if [[ "$command_text" == "post-update --mode dogfood-recover-inactive" \
  && -n "${TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER:-}" ]]; then
  rm -f "$TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER"
fi
EOF
  } >"$path"
  chmod +x "$path"
}

fake_bin="$fixture/fake bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 97
EOF
cat >"$fake_bin/rustc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=
while (($#)); do
  if [[ "$1" == -o ]]; then
    output=${2:?}
    shift 2
    continue
  fi
  shift
done
test -n "$output"
cat >"$output" <<'WRITER'
#!/usr/bin/env bash
set -euo pipefail
printf '%s' "${TRACEDECAY_DOGFOOD_TEST_DASHBOARD_SOURCE_STAMP:?}"
WRITER
chmod +x "$output"
EOF
cat >"$fake_bin/install" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
destination=${!#}
if [[ -n "${TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_PREFIX:-}" \
  && "$destination" == "$TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_PREFIX"* \
  && ! -e "${TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_ONCE:?}" ]]; then
  : >"$TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_ONCE"
  exit 73
fi
exec /usr/bin/install "$@"
EOF
cat >"$fake_bin/sync" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ -n "${TRACEDECAY_DOGFOOD_TEST_FAIL_SYNC_PATH:-}" \
  && "${*: -1}" == "$TRACEDECAY_DOGFOOD_TEST_FAIL_SYNC_PATH"* ]]; then
  exit 74
fi
/usr/bin/sync "$@"

if [[ "${TRACEDECAY_DOGFOOD_TEST_CRASH_AFTER:-}" == marker-fsync \
  && "${*: -1}" == "${TRACEDECAY_DOGFOOD_TEST_MARKER_TEMP_PREFIX:-}"* ]]; then
  marker_fsync_count=0
  if [[ -e "${TRACEDECAY_DOGFOOD_TEST_MARKER_FSYNC_COUNT:?}" ]]; then
    read -r marker_fsync_count \
      <"${TRACEDECAY_DOGFOOD_TEST_MARKER_FSYNC_COUNT:?}" || true
  fi
  marker_fsync_count=$((marker_fsync_count + 1))
  printf '%s\n' "$marker_fsync_count" \
    >"${TRACEDECAY_DOGFOOD_TEST_MARKER_FSYNC_COUNT:?}"
  if ((marker_fsync_count == 2)); then
    : >"${TRACEDECAY_DOGFOOD_TEST_CRASH_REACHED:?}"
    kill -KILL "$PPID"
    exit 137
  fi
fi
EOF
cat >"$fake_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${TRACEDECAY_DOGFOOD_TEST_MANAGER_LOG:?}"

crash_after() {
  local stage=$1

  if [[ "${TRACEDECAY_DOGFOOD_TEST_MANAGER_PHASE:-}" == forward \
    && "${TRACEDECAY_DOGFOOD_TEST_CRASH_AFTER:-}" == "$stage" ]]; then
    : >"${TRACEDECAY_DOGFOOD_TEST_CRASH_REACHED:?}"
    kill -KILL "${TRACEDECAY_DOGFOOD_TEST_CRASH_TARGET:?}"
    exit 137
  fi
}

case "${2:-}" in
  daemon-reload)
    : >"${TRACEDECAY_DOGFOOD_TEST_MANAGER_RELOADED:?}"
    crash_after manager-reload
    ;;
  stop)
    /usr/bin/rm -f \
      "${TRACEDECAY_DOGFOOD_TEST_OLD_SERVICE_RUNNING:?}" \
      "${TRACEDECAY_DOGFOOD_TEST_STALE_SOCKET:?}"
    crash_after old-stop
    ;;
  start)
    : >"${TRACEDECAY_DOGFOOD_TEST_NEW_SERVICE_RUNNING:?}"
    crash_after new-start
    ;;
  disable)
    /usr/bin/rm -f \
      "${TRACEDECAY_DOGFOOD_TEST_OLD_SERVICE_RUNNING:?}" \
      "${TRACEDECAY_DOGFOOD_TEST_NEW_SERVICE_RUNNING:?}" \
      "${TRACEDECAY_DOGFOOD_TEST_STALE_SOCKET:?}"
    ;;
  is-active)
    test -e "${TRACEDECAY_DOGFOOD_TEST_OLD_SERVICE_RUNNING:?}" ||
      test -e "${TRACEDECAY_DOGFOOD_TEST_NEW_SERVICE_RUNNING:?}"
    ;;
  is-enabled)
    printf 'enabled\n'
    ;;
esac
EOF
chmod +x \
  "$fake_bin/cargo" \
  "$fake_bin/install" \
  "$fake_bin/rustc" \
  "$fake_bin/sync" \
  "$fake_bin/systemctl"
clean_path="$fake_bin:/usr/bin:/bin"

setup_case() {
  local name=$1
  case_root="$fixture/$name"
  case_home="$case_root/home with spaces"
  case_stage="$case_root/stage with spaces"
  case_install="$case_root/install with spaces"
  case_profile="$case_root/profile with spaces"
  case_backup="$case_root/verified backup with spaces"
  case_source="$case_root/build output/tracedecay candidate"
  case_target="$case_root/target"
  case_dashboard_stamp="$case_target/dogfood-dashboard-source.stamp"
  case_log="$case_root/actions.log"
  case_output="$case_root/output.log"
  case_state="$case_profile/dogfood-migration-boundary.state"
  case_manager_dir="$case_root/service manager with spaces"
  case_unit="$case_manager_dir/tracedecay.service"
  case_old_binary="$case_root/old absolute binary/tracedecay"
  case_manager_log="$case_root/service-manager.log"
  case_old_running="$case_root/old-service-running"
  case_new_running="$case_root/new-service-running"
  case_stale_socket="$case_root/old-daemon.sock"
  case_manager_reloaded="$case_root/manager-reloaded"
  case_crash_reached="$case_root/crash-reached"
  case_crash_status="$case_root/crash-status"
  case_marker_fsync_count="$case_root/marker-fsync-count"
  installed="$case_install/tracedecay"
  staged="$case_stage/tracedecay"
  mkdir -p "$case_home" "$case_stage" "$case_install" "$case_profile" "$case_backup" \
    "$(dirname "$case_source")" "$case_manager_dir" "$case_target"
  printf '{}\n' >"$case_backup/backup-manifest.json"
  cp "$repo_root/dashboard/app-dist/.source-stamp" "$case_dashboard_stamp"
  : >"$case_log"
}

install_old_pair() {
  write_fake_binary "$installed" old-installed
  write_fake_binary "$staged" old-staged
}

# CI exports TRACEDECAY_SKIP_DASHBOARD_BUILD=1 for every job, and dogfood
# refuses to run with it set. Cases opt into that refusal explicitly (see the
# skip-dashboard-build case), so the ambient value must not reach the script.
run_case() {
  env \
    --unset=TRACEDECAY_SKIP_DASHBOARD_BUILD \
    PATH="$clean_path" \
    HOME="$case_home" \
    CARGO_TARGET_DIR="$case_target" \
    TRACEDECAY_DOGFOOD_SOURCE_BINARY="$case_source" \
    TRACEDECAY_DOGFOOD_TEST_DASHBOARD_SOURCE_STAMP="$(<"$repo_root/dashboard/app-dist/.source-stamp")" \
    TRACEDECAY_DOGFOOD_STAGE_DIR="$case_stage" \
    TRACEDECAY_DOGFOOD_INSTALL_DIR="$case_install" \
    TRACEDECAY_DOGFOOD_PROFILE_DIR="$case_profile" \
    TRACEDECAY_DOGFOOD_BACKUP="$case_backup" \
    TRACEDECAY_DOGFOOD_TEST_LOG="$case_log" \
    "$@" \
    "$dogfood_script"
}

run_case_background() {
  exec env \
    --unset=TRACEDECAY_SKIP_DASHBOARD_BUILD \
    PATH="$clean_path" \
    HOME="$case_home" \
    CARGO_TARGET_DIR="$case_target" \
    TRACEDECAY_DOGFOOD_SOURCE_BINARY="$case_source" \
    TRACEDECAY_DOGFOOD_TEST_DASHBOARD_SOURCE_STAMP="$(<"$repo_root/dashboard/app-dist/.source-stamp")" \
    TRACEDECAY_DOGFOOD_STAGE_DIR="$case_stage" \
    TRACEDECAY_DOGFOOD_INSTALL_DIR="$case_install" \
    TRACEDECAY_DOGFOOD_PROFILE_DIR="$case_profile" \
    TRACEDECAY_DOGFOOD_BACKUP="$case_backup" \
    TRACEDECAY_DOGFOOD_TEST_LOG="$case_log" \
    "$@" \
    "$dogfood_script"
}

assert_boundary() {
  local key=$1
  local expected=$2
  grep -Fxq "$key=$expected" "$case_state" ||
    fail "boundary state missing $key=$expected"
}

marker_checksum() {
  local payload=$1
  local output
  output=$(printf '%s' "$payload" | sha256sum)
  printf '%s' "${output%% *}"
}

binary_checksum() {
  local output
  output=$(sha256sum -- "$1")
  printf '%s' "${output%% *}"
}

write_test_marker() {
  local path=$1
  local attempt_id=$2
  local outcome=$3
  local boundary=$4
  local policy=$5
  local daemon=$6
  local payload
  printf -v payload \
    'format=2\nattempt_id=%s\noutcome=%s\nattempt_boundary=%s\nold_binary_policy=%s\nmanaged_daemon=%s\n' \
    "$attempt_id" "$outcome" "$boundary" "$policy" "$daemon"
  printf '%schecksum=%s\n' "$payload" "$(marker_checksum "$payload")" >"$path"
  chmod 0600 "$path"
}

write_test_marker_v3() {
  local path=$1
  local attempt_id=$2
  local outcome=$3
  local boundary=$4
  local policy=$5
  local daemon=$6
  local retained_binary_sha256=$7
  local payload
  printf -v payload \
    'format=3\nattempt_id=%s\noutcome=%s\nattempt_boundary=%s\nold_binary_policy=%s\nmanaged_daemon=%s\nretained_binary_sha256=%s\n' \
    "$attempt_id" "$outcome" "$boundary" "$policy" "$daemon" "$retained_binary_sha256"
  printf '%schecksum=%s\n' "$payload" "$(marker_checksum "$payload")" >"$path"
  chmod 0600 "$path"
}

assert_v3_retained_binding() {
  local expected=none
  if [[ -e "$installed" || -L "$installed" ]]; then
    expected=$(binary_checksum "$installed")
  fi
  grep -Fxq format=3 "$case_state" ||
    fail 'new boundary marker did not migrate to format 3'
  assert_boundary retained_binary_sha256 "$expected"
}

assert_no_temporary_install_files() {
  local artifacts=()
  shopt -s nullglob
  artifacts=(
    "$case_install"/tracedecay.new.*
    "$case_install"/tracedecay.previous.*
    "$case_stage"/tracedecay.new.*
    "$case_stage"/tracedecay.previous.*
    "$case_stage"/tracedecay.candidate.*
    "$case_profile"/dogfood-migration-boundary.state.new.*
  )
  shopt -u nullglob
  ((${#artifacts[@]} == 0)) ||
    fail "temporary install files remain: ${artifacts[*]}"
}

assert_forward_stage_failure() {
  local name=$1
  local stage=$2

  setup_case "$name"
  write_fake_binary "$case_source" new
  install_old_pair
  daemon_marker="$case_root/daemon-running"
  if run_case \
    TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
    TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=new \
    TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --strict --mode dogfood-forward-only' \
    TRACEDECAY_DOGFOOD_TEST_FAIL_STAGE="$stage" \
    >"$case_output" 2>&1; then
    fail "$stage failure unexpectedly succeeded"
  fi
  test ! -e "$daemon_marker" || fail "$stage failure left daemon running"
  cmp "$case_source" "$installed"
  cmp "$case_source" "$staged"
  test "$(cat "$case_log")" = \
    $'new:post-update --strict --mode dogfood-forward-only\nnew:post-update --mode dogfood-recover-inactive' ||
    fail "$stage recovery escaped the new typed lifecycle"
  if grep -q '^old-' "$case_log"; then
    fail "$stage recovery invoked an old binary"
  fi
  assert_boundary outcome forward-recovery-required
  assert_boundary managed_daemon inactive
}

write_crashable_binary() {
  local path=$1
  local binary_id=$2
  mkdir -p "$(dirname "$path")"
  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n'
    printf 'binary_id=%q\n' "$binary_id"
    printf 'checkout_root=%q\n' "$repo_root"
    cat <<'EOF'
command_text=$*
printf '%s:%s\n' "$binary_id" "$command_text" \
  >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"

crash_now() {
  local stage=$1

  if [[ "${TRACEDECAY_DOGFOOD_TEST_CRASH_AFTER:-}" == "$stage" ]]; then
    : >"${TRACEDECAY_DOGFOOD_TEST_CRASH_REACHED:?}"
    kill -KILL "$PPID"
    exit 137
  fi
}

case "$command_text" in
  'post-update --strict --mode dogfood-forward-only')
    unit=${TRACEDECAY_DOGFOOD_TEST_SERVICE_UNIT:?}
    temporary=$(mktemp "${unit}.new.XXXXXX")
    printf '[Service]\nExecStart=%s daemon run\n' "$0" >"$temporary"
    chmod 0644 "$temporary"
    /usr/bin/sync -f "$temporary"
    mv -f -- "$temporary" "$unit"
    /usr/bin/sync -f "$(dirname "$unit")"
    crash_now unit-rename-dir-fsync

    export TRACEDECAY_DOGFOOD_TEST_MANAGER_PHASE=forward
    export TRACEDECAY_DOGFOOD_TEST_CRASH_TARGET=$PPID
    systemctl --user daemon-reload
    systemctl --user stop tracedecay.service
    systemctl --user start tracedecay.service

    : >"${TRACEDECAY_DOGFOOD_TEST_READINESS_REACHED:?}"
    crash_now readiness
    "$0" --version >/dev/null
    crash_now version
    : >"${TRACEDECAY_DOGFOOD_TEST_DOCTOR_REACHED:?}"
    crash_now doctor
    ;;
  'post-update --mode dogfood-recover-inactive')
    export TRACEDECAY_DOGFOOD_TEST_MANAGER_PHASE=recovery
    systemctl --user daemon-reload
    systemctl --user disable --now tracedecay.service
    ;;
  --version)
    sha=$(git -C "$checkout_root" rev-parse --short=12 HEAD)
    status=$(git -C "$checkout_root" status --porcelain)
    dirty=
    if [[ -n "$status" ]]; then
      dirty=.dirty
    fi
    printf 'tracedecay 0.0.0+%s%s\n' "$sha" "$dirty"
    ;;
esac
EOF
  } >"$path"
  chmod +x "$path"
}

create_unreachable_socket() {
  python3 - "$1" <<'PY'
import socket
import sys

socket_path = sys.argv[1]
socket_handle = socket.socket(socket.AF_UNIX)
socket_handle.bind(socket_path)
socket_handle.close()
PY
}

assert_unreachable_stale_socket() {
  if [[ -e "$case_stale_socket" ]]; then
    test -S "$case_stale_socket" ||
      fail "stale daemon socket is not a socket: $case_stale_socket"
    if python3 - "$case_stale_socket" <<'PY'
import socket
import sys

socket_handle = socket.socket(socket.AF_UNIX)
try:
    socket_handle.connect(sys.argv[1])
except OSError:
    raise SystemExit(1)
raise SystemExit(0)
PY
    then
      fail "stale daemon socket remained reachable: $case_stale_socket"
    fi
  fi
}

assert_valid_boundary_marker() {
  local checksum_index
  local marker_mode
  local payload
  local retained_binary_sha256
  local -a marker_lines

  test -f "$case_state" || fail "boundary marker missing after crash"
  test ! -L "$case_state" || fail "boundary marker became a symlink"
  if marker_mode=$(stat -c '%a' -- "$case_state" 2>/dev/null); then
    :
  else
    marker_mode=$(stat -f '%Lp' -- "$case_state")
  fi
  test "$marker_mode" = 600 ||
    fail "boundary marker mode is $marker_mode instead of 600"
  mapfile -t marker_lines <"$case_state"
  case "${marker_lines[0]:-}" in
    format=2)
      ((${#marker_lines[@]} == 7)) ||
        fail "format-2 boundary marker has ${#marker_lines[@]} lines instead of seven"
      checksum_index=6
      ;;
    format=3)
      ((${#marker_lines[@]} == 8)) ||
        fail "format-3 boundary marker has ${#marker_lines[@]} lines instead of eight"
      [[ "${marker_lines[6]}" == retained_binary_sha256=* ]] ||
        fail "format-3 boundary marker has no retained binary binding"
      retained_binary_sha256=${marker_lines[6]#retained_binary_sha256=}
      [[ "$retained_binary_sha256" == none ||
        "$retained_binary_sha256" =~ ^[0-9a-f]{64}$ ]] ||
        fail "format-3 boundary marker has invalid retained binary binding"
      checksum_index=7
      ;;
    *)
      fail "boundary marker has unsupported format: ${marker_lines[0]:-missing}"
      ;;
  esac
  [[ "${marker_lines[1]}" == attempt_id=* && -n "${marker_lines[1]#attempt_id=}" ]] ||
    fail "boundary marker has no attempt id"
  [[ "${marker_lines[2]}" == outcome=* ]] || fail "boundary marker has no outcome"
  [[ "${marker_lines[3]}" == attempt_boundary=* ]] ||
    fail "boundary marker has no attempt boundary"
  [[ "${marker_lines[4]}" == old_binary_policy=* ]] ||
    fail "boundary marker has no binary policy"
  [[ "${marker_lines[5]}" == managed_daemon=* ]] ||
    fail "boundary marker has no daemon state"
  printf -v payload '%s\n' "${marker_lines[@]:0:checksum_index}"
  test "${marker_lines[$checksum_index]}" = "checksum=$(marker_checksum "$payload")" ||
    fail "boundary marker checksum is invalid"

  case "${marker_lines[2]#outcome=}:${marker_lines[3]#attempt_boundary=}:${marker_lines[4]#old_binary_policy=}:${marker_lines[5]#managed_daemon=}" in
    preparing:not-reached:allowed:unchanged | \
      preparing:not-reached:forbidden:unchanged | \
      safe-rollback-complete:not-reached:allowed:unchanged | \
      safe-rollback-complete:not-reached:forbidden:unchanged | \
      post-update-starting:reached:forbidden:inactivity-pending | \
      forward-recovery-required:reached:forbidden:inactive | \
      forward-recovery-required:reached:forbidden:inactivity-unproven | \
      validated:reached:forbidden:verified-new-version)
      ;;
    *)
      fail "boundary marker has an invalid transition: ${marker_lines[*]}"
      ;;
  esac
}

run_crash_case() {
  run_case \
    TRACEDECAY_DOGFOOD_TEST_MANAGER_LOG="$case_manager_log" \
    TRACEDECAY_DOGFOOD_TEST_MANAGER_RELOADED="$case_manager_reloaded" \
    TRACEDECAY_DOGFOOD_TEST_SERVICE_UNIT="$case_unit" \
    TRACEDECAY_DOGFOOD_TEST_OLD_SERVICE_RUNNING="$case_old_running" \
    TRACEDECAY_DOGFOOD_TEST_NEW_SERVICE_RUNNING="$case_new_running" \
    TRACEDECAY_DOGFOOD_TEST_STALE_SOCKET="$case_stale_socket" \
    TRACEDECAY_DOGFOOD_TEST_READINESS_REACHED="$case_root/readiness-reached" \
    TRACEDECAY_DOGFOOD_TEST_DOCTOR_REACHED="$case_root/doctor-reached" \
    "$@"
}

run_crash_case_background() {
  local status

  set +e
  run_crash_case "$@"
  status=$?
  set -e
  printf '%s\n' "$status" >"$case_crash_status"
}

assert_crash_service_state() {
  local stage=$1

  case "$stage" in
    marker-fsync)
      grep -Fxq "ExecStart=$case_old_binary daemon run" "$case_unit" ||
        fail 'marker fsync crash rewrote the unit before post-update began'
      test -e "$case_old_running" ||
        fail 'marker fsync crash did not retain the old running service'
      test ! -e "$case_new_running" ||
        fail 'marker fsync crash started a new service'
      ;;
    unit-rename-dir-fsync | manager-reload)
      assert_new_unit
      test -e "$case_old_running" ||
        fail "$stage crash stopped the old service before quiescing"
      test ! -e "$case_new_running" ||
        fail "$stage crash started a new service before quiescing"
      ;;
    old-stop)
      assert_new_unit
      test ! -e "$case_old_running" ||
        fail 'old-stop crash left the old service running'
      test ! -e "$case_new_running" ||
        fail 'old-stop crash started a new service'
      ;;
    new-start | readiness | version | doctor)
      assert_new_unit
      test ! -e "$case_old_running" ||
        fail "$stage crash left the old service selected"
      test -e "$case_new_running" ||
        fail "$stage crash did not reach the new service start"
      ;;
  esac
}

assert_new_unit() {
  test -f "$case_unit" || fail "new service unit is not regular: $case_unit"
  test ! -L "$case_unit" || fail "new service unit is a symlink: $case_unit"
  grep -Fxq "ExecStart=$installed daemon run" "$case_unit" ||
    fail "new service unit does not select $installed"
  if grep -Fq "$case_old_binary" "$case_unit"; then
    fail 'new service unit still selects the old absolute binary'
  fi
}

assert_crash_recovery() {
  local stage=$1
  local crash_status
  local dogfood_pid

  command -v python3 >/dev/null || fail 'crash harness requires python3'
  setup_case "sigkill-$stage"
  write_crashable_binary "$case_source" new
  install_old_pair
  write_fake_binary "$case_old_binary" old-service
  printf '[Service]\nExecStart=%s daemon run\n' "$case_old_binary" >"$case_unit"
  chmod 0644 "$case_unit"
  : >"$case_old_running"
  create_unreachable_socket "$case_stale_socket"

  run_crash_case_background \
    TRACEDECAY_DOGFOOD_TEST_CRASH_AFTER="$stage" \
    TRACEDECAY_DOGFOOD_TEST_CRASH_REACHED="$case_crash_reached" \
    TRACEDECAY_DOGFOOD_TEST_MARKER_TEMP_PREFIX="$case_state.new." \
    TRACEDECAY_DOGFOOD_TEST_MARKER_FSYNC_COUNT="$case_marker_fsync_count" \
    >"$case_output" 2>&1 &
  dogfood_pid=$!
  for _ in $(seq 1 200); do
    [[ -e "$case_crash_reached" ]] && break
    kill -0 "$dogfood_pid" 2>/dev/null || break
    sleep 0.02
  done
  if [[ ! -e "$case_crash_reached" ]]; then
    kill -KILL "$dogfood_pid" 2>/dev/null || true
    set +e
    wait "$dogfood_pid"
    set -e
    fail "$stage crash hook was not reached"
  fi
  set +e
  wait "$dogfood_pid"
  set -e
  test -f "$case_crash_status" ||
    fail "$stage crash did not report an exit status"
  crash_status=$(<"$case_crash_status")
  test "$crash_status" -eq 137 ||
    fail "$stage crash exited $crash_status instead of SIGKILL status 137"

  case "$stage" in
    marker-fsync)
      test "$(<"$case_marker_fsync_count")" = 2 ||
        fail 'marker fsync crash did not reach the post-update marker'
      ;;
    manager-reload)
      test -e "$case_manager_reloaded" ||
        fail 'manager reload crash did not reload the new unit'
      ;;
    readiness)
      test -e "$case_root/readiness-reached" ||
        fail 'readiness crash did not reach the readiness probe'
      ;;
    version)
      grep -Fxq 'new:--version' "$case_log" ||
        fail 'version crash did not execute the retained new binary'
      ;;
    doctor)
      test -e "$case_root/doctor-reached" ||
        fail 'Doctor crash did not reach the Doctor boundary'
      ;;
  esac

  assert_valid_boundary_marker
  if [[ "$stage" == marker-fsync ]]; then
    assert_boundary outcome preparing
    assert_boundary attempt_boundary not-reached
    assert_boundary old_binary_policy allowed
    assert_boundary managed_daemon unchanged
  else
    assert_boundary outcome post-update-starting
    assert_boundary attempt_boundary reached
    assert_boundary old_binary_policy forbidden
    assert_boundary managed_daemon inactivity-pending
  fi
  assert_crash_service_state "$stage"
  assert_unreachable_stale_socket
  if grep -q '^old-' "$case_log"; then
    fail "$stage crash executed an old binary"
  fi

  : >"$case_log"
  write_crashable_binary "$case_source" newer
  run_crash_case >"$case_output" 2>&1
  cmp "$case_source" "$installed"
  cmp "$case_source" "$staged"
  assert_valid_boundary_marker
  assert_boundary outcome validated
  assert_boundary attempt_boundary reached
  assert_boundary old_binary_policy forbidden
  assert_boundary managed_daemon verified-new-version
  assert_new_unit
  test ! -e "$case_old_running" ||
    fail "$stage retry did not retire the old running service"
  test -e "$case_new_running" ||
    fail "$stage retry did not start the newer service"
  assert_unreachable_stale_socket
  if grep -q '^old-' "$case_log"; then
    fail "$stage retry executed an old binary"
  fi
}

# A profile backup is optional insurance, not a gate: with none named, dogfood
# proceeds and says so on stderr so the operator knows the rehearsal was
# skipped. Naming an incomplete one is still a configuration error (next case).
setup_case absent-profile-backup
write_fake_binary "$case_source" new
run_case TRACEDECAY_DOGFOOD_BACKUP= >"$case_output" 2>&1
grep -Fq 'proceeding without a profile backup' "$case_output" ||
  fail "absent backup did not announce that it proceeded without one"
test -e "$case_state" ||
  fail "absent backup stopped short of the migration marker boundary"

# Dogfood must stop before installation or marker publication when a backup is
# named but is not a complete verified backup fit for restored-copy rehearsal.
setup_case incomplete-profile-backup
write_fake_binary "$case_source" new
rm -f "$case_backup/backup-manifest.json"
if run_case >"$case_output" 2>&1; then
  fail "dogfood succeeded with an incomplete profile backup"
fi
grep -Fq 'names an incomplete backup' "$case_output" ||
  fail "incomplete backup refusal was not actionable"
test ! -e "$case_state" ||
  fail "incomplete backup refusal crossed the migration marker boundary"

# Crossing into post-update is irreversible. Even when it fails after opening
# a store, the new binary remains selected and is the only binary allowed to
# stop the managed daemon.
setup_case post-update-failure
write_fake_binary "$case_source" new
write_fake_binary "$case_root/old target" old-installed
ln -s "$case_root/old target" "$installed"
write_fake_binary "$staged" old-staged
schema_marker="$case_root/schema-opened"
daemon_marker="$case_root/daemon-running"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_OPEN_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_OPEN_COMMAND='post-update --strict --mode dogfood-forward-only' \
  TRACEDECAY_DOGFOOD_TEST_SCHEMA_MARKER="$schema_marker" \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --strict --mode dogfood-forward-only' \
  TRACEDECAY_DOGFOOD_TEST_FAIL_MESSAGE='terminal daemon startup health failure: fts5: corruption found reading blob 412316860480 from table "nodes_fts"' \
  >"$case_output" 2>&1; then
  fail 'post-update failure unexpectedly succeeded'
fi
test -e "$schema_marker" || fail 'post-update did not simulate a schema open'
test ! -e "$daemon_marker" || fail 'managed daemon remained running'
test ! -L "$installed" || fail 'installed path still selects the old symlink'
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'new:post-update --strict --mode dogfood-forward-only\nnew:post-update --mode dogfood-recover-inactive' ||
  fail 'an old binary ran after the migration boundary'
assert_boundary outcome forward-recovery-required
assert_boundary attempt_boundary reached
assert_boundary old_binary_policy forbidden
assert_boundary managed_daemon inactive
grep -Fq 'previous binary was not restored or executed' "$case_output" ||
  fail 'forward-only diagnostic omitted old-binary warning'
grep -Fq 'rerun cargo dogfood' "$case_output" ||
  fail 'forward-recovery instruction was not explicit'
grep -Fq 'terminal daemon startup health failure: fts5: corruption found reading blob 412316860480 from table "nodes_fts"' "$case_output" ||
  fail 'terminal health failure swallowed the underlying corruption'
assert_no_temporary_install_files

# A new invocation after a boundary failure must prove inactive recovery with
# the current source build before overwriting preparing and moving forward.
: >"$case_log"
write_fake_binary "$case_source" newer
run_case >"$case_output" 2>&1
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'newer:post-update --mode dogfood-recover-inactive\nnewer:post-update --strict --mode dogfood-forward-only' ||
  fail 'repeated invocation skipped source-candidate inactive recovery'
assert_boundary outcome validated
assert_boundary old_binary_policy forbidden
assert_no_temporary_install_files

# Retry with a still-active daemon must use the current source candidate's typed
# inactive-recovery command. Failure leaves the pending marker untouched and
# must not invoke the unbound installed binary.
setup_case retry-active-old-daemon
write_fake_binary "$case_source" newer
write_fake_binary "$installed" new-installed
write_fake_binary "$staged" new-staged
write_fake_binary "$case_root/old leftover" old-installed
cp "$installed" "$case_root/expected installed"
daemon_marker="$case_root/daemon-running"
: >"$daemon_marker"
write_test_marker \
  "$case_state" previous-attempt-retry01 forward-recovery-required reached forbidden \
  inactivity-unproven
cp "$case_state" "$case_root/marker.before"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=newer \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --mode dogfood-recover-inactive' \
  >"$case_output" 2>&1; then
  fail 'retry with active daemon and failed recovery unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_source" "$staged"
cmp "$case_root/marker.before" "$case_state" ||
  fail 'failed inactive recovery overwrote the pending marker'
test -e "$daemon_marker" || fail 'failed recovery cleared the active daemon marker'
test "$(cat "$case_log")" = \
  $'newer:post-update --mode dogfood-recover-inactive' ||
  fail 'active-daemon retry escaped the source-candidate recover-inactive path'
if grep -q '^old-' "$case_log"; then
  fail 'active-daemon retry invoked an old binary'
fi
if grep -q 'dogfood-forward-only' "$case_log"; then
  fail 'active-daemon retry reached forward-only before inactivity was proven'
fi
grep -Fq 'inactive recovery failed' "$case_output" ||
  fail 'active-daemon recovery failure was not actionable'
assert_boundary outcome forward-recovery-required
assert_boundary managed_daemon inactivity-unproven
assert_no_temporary_install_files

# Successful inactive recovery must run against the current source candidate
# and clear any still-active daemon before preparing/forward-only may proceed.
setup_case retry-recovery-success
write_fake_binary "$case_source" newer
write_fake_binary "$installed" new-installed
write_fake_binary "$staged" new-staged
daemon_marker="$case_root/daemon-running"
: >"$daemon_marker"
write_test_marker \
  "$case_state" previous-attempt-retry02 post-update-starting reached forbidden \
  inactivity-pending
run_case \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  >"$case_output" 2>&1
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'newer:post-update --mode dogfood-recover-inactive\nnewer:post-update --strict --mode dogfood-forward-only' ||
  fail 'recovery success did not prove inactivity before forward-only'
# Forward-only may start the new unit afterward; recovery is proven by the
# source-candidate recover-inactive preceding any forward-only work.
if grep -q '^old-' "$case_log"; then
  fail 'recovery success invoked an old binary'
fi
assert_boundary outcome validated
assert_boundary old_binary_policy forbidden
assert_no_temporary_install_files

# A v2 pending marker does not bind the installed path. Recovery must therefore
# use the newly built source candidate, even when the installed path was
# replaced by a broken old binary after the marker was written.
setup_case retry-recovery-replaced-installed
write_fake_binary "$case_source" newer
write_fake_binary "$installed" old-replaced
write_fake_binary "$staged" new-staged
write_test_marker \
  "$case_state" previous-attempt-retry02b forward-recovery-required reached forbidden \
  inactive
if ! run_case \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=old-replaced \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --mode dogfood-recover-inactive' \
  >"$case_output" 2>&1; then
  fail 'v2 retry could not recover with the newer source candidate'
fi
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'newer:post-update --mode dogfood-recover-inactive\nnewer:post-update --strict --mode dogfood-forward-only' ||
  fail 'v2 retry executed an unbound installed binary'
assert_boundary outcome validated
assert_boundary old_binary_policy forbidden
assert_valid_boundary_marker
assert_v3_retained_binding
assert_no_temporary_install_files

# A valid v3 marker binds the retained installed bytes. If that path is later
# replaced, the mismatch must be detected and the unbound path must never run;
# the current source candidate remains the forward-recovery authority.
setup_case retry-recovery-v3-binding-mismatch
write_fake_binary "$case_source" newer
write_fake_binary "$installed" retained-new
write_fake_binary "$staged" retained-staged
retained_digest=$(binary_checksum "$installed")
write_test_marker_v3 \
  "$case_state" previous-attempt-retry02c forward-recovery-required reached forbidden \
  inactive "$retained_digest"
write_fake_binary "$installed" old-replaced
if ! run_case \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=old-replaced \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --mode dogfood-recover-inactive' \
  >"$case_output" 2>&1; then
  fail 'v3 mismatch could not recover with the newer source candidate'
fi
test "$(cat "$case_log")" = \
  $'newer:post-update --mode dogfood-recover-inactive\nnewer:post-update --strict --mode dogfood-forward-only' ||
  fail 'v3 mismatch executed the unbound installed binary'
assert_boundary outcome validated
assert_boundary old_binary_policy forbidden
assert_valid_boundary_marker
assert_v3_retained_binding
assert_no_temporary_install_files

# A replaced retained path may no longer be hashable at all. That is an
# untrusted mismatch, not permission to block the trusted source recovery path.
setup_case retry-recovery-v3-unhashable-installed
write_fake_binary "$case_source" newer
write_fake_binary "$installed" retained-new
write_fake_binary "$staged" retained-staged
retained_digest=$(binary_checksum "$installed")
write_test_marker_v3 \
  "$case_state" previous-attempt-retry02d forward-recovery-required reached forbidden \
  inactive "$retained_digest"
rm -f -- "$installed"
ln -s "$case_root/missing-retained-binary" "$installed"
if ! run_case >"$case_output" 2>&1; then
  fail 'v3 unhashable retained path blocked trusted source recovery'
fi
test "$(cat "$case_log")" = \
  $'newer:post-update --mode dogfood-recover-inactive\nnewer:post-update --strict --mode dogfood-forward-only' ||
  fail 'v3 unhashable mismatch executed an unexpected binary'
cmp "$case_source" "$installed"
assert_boundary outcome validated
assert_valid_boundary_marker
assert_v3_retained_binding
assert_no_temporary_install_files

# If no usable retained binary exists, an interrupted retry must durably encode
# `none`; it must never publish an empty or malformed digest binding.
setup_case retry-recovery-v3-missing-retained-binding
write_fake_binary "$case_source" newer
write_fake_binary "$installed" retained-new
write_fake_binary "$staged" retained-staged
retained_digest=$(binary_checksum "$installed")
write_test_marker_v3 \
  "$case_state" previous-attempt-retry02e forward-recovery-required reached forbidden \
  inactive "$retained_digest"
rm -f -- "$installed"
ln -s "$case_root/missing-retained-binary" "$installed"
fail_once="$case_root/candidate-install-failed"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_PREFIX="$case_stage/tracedecay.candidate." \
  TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_ONCE="$fail_once" \
  >"$case_output" 2>&1; then
  fail 'missing-retained candidate failure unexpectedly succeeded'
fi
assert_boundary outcome preparing
assert_boundary old_binary_policy forbidden
assert_boundary retained_binary_sha256 none
assert_valid_boundary_marker

# Recovery failure is fail-closed across repeated retries: the authoritative
# pending marker bytes must remain identical.
setup_case retry-recovery-failure-preserves-marker
write_fake_binary "$case_source" newer
write_fake_binary "$installed" new-installed
write_fake_binary "$staged" new-staged
cp "$installed" "$case_root/expected installed"
daemon_marker="$case_root/daemon-running"
: >"$daemon_marker"
write_test_marker \
  "$case_state" previous-attempt-retry03 forward-recovery-required reached forbidden \
  inactive
cp "$case_state" "$case_root/marker.before"
for attempt in 1 2; do
  : >"$case_log"
  if run_case \
    TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
    TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=newer \
    TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --mode dogfood-recover-inactive' \
    >"$case_output" 2>&1; then
    fail "repeated recovery failure attempt $attempt unexpectedly succeeded"
  fi
  cmp "$case_root/marker.before" "$case_state" ||
    fail "repeated recovery failure attempt $attempt mutated the marker"
  cmp "$case_root/expected installed" "$installed"
  cmp "$case_source" "$staged"
  test -e "$daemon_marker" ||
    fail "repeated recovery failure attempt $attempt cleared the daemon"
  test "$(cat "$case_log")" = \
    $'newer:post-update --mode dogfood-recover-inactive' ||
    fail "repeated recovery failure attempt $attempt invoked unexpected commands"
done
assert_boundary outcome forward-recovery-required
assert_boundary attempt_boundary reached
assert_boundary old_binary_policy forbidden
assert_boundary managed_daemon inactive
assert_no_temporary_install_files

# Before post-update begins, no live writable schema can have opened. A staged
# atomic-install failure therefore restores both prior paths and leaves the
# existing daemon untouched.
setup_case pre-boundary-failure
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
daemon_marker="$case_root/daemon-running"
: >"$daemon_marker"
fail_once="$case_root/install-failed"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_PREFIX="$installed.new." \
  TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_ONCE="$fail_once" \
  >"$case_output" 2>&1; then
  fail 'pre-boundary install failure unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test -e "$daemon_marker" || fail 'pre-boundary rollback stopped the old daemon'
test ! -s "$case_log" || fail 'a binary ran before the migration boundary'
assert_boundary outcome safe-rollback-complete
assert_boundary attempt_boundary not-reached
assert_boundary old_binary_policy allowed
assert_boundary managed_daemon unchanged
assert_no_temporary_install_files

# Registration refresh failures must surface before the migration marker,
# binary replacement, or any daemon lifecycle command.
setup_case integration-preflight-failure
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
preflight_marker="$case_root/integration-preflight-ran"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_PREFLIGHT_MARKER="$preflight_marker" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_PREFLIGHT=1 \
  >"$case_output" 2>&1; then
  fail 'integration preflight failure unexpectedly succeeded'
fi
test -e "$preflight_marker" || fail 'integration preflight did not run'
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -e "$case_state" || fail 'integration preflight failure recorded a boundary marker'
test ! -s "$case_log" || fail 'integration preflight failure ran a lifecycle command'
grep -Fq 'cline: registration config is corrupt' "$case_output" ||
  fail 'integration preflight omitted the per-integration cause'
grep -Fq 'dogfood integration refresh preflight failed before the migration boundary' \
  "$case_output" ||
  fail 'integration preflight failure did not identify the safe boundary'
assert_no_temporary_install_files

# A candidate binary must attest to this checkout's fresh SHA and dirty state
# before dogfood mutates binary paths or records a migration-boundary marker.
setup_case staged-identity-mismatch
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
daemon_marker="$case_root/daemon-running"
: >"$daemon_marker"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_REPORTED_VERSION='0.0.0+000000000000' \
  >"$case_output" 2>&1; then
  fail 'staged identity mismatch unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test -e "$daemon_marker" || fail 'identity mismatch stopped the old daemon'
test ! -s "$case_log" || fail 'identity mismatch executed a lifecycle command'
grep -Fq 'dogfood candidate binary identity mismatch' "$case_output" ||
  fail 'identity mismatch was not explicit'
test ! -e "$case_state" || fail 'identity mismatch recorded a boundary marker'
assert_no_temporary_install_files

# The dashboard stamp written by build.rs is the positive freshness proof.
# A mismatch fails before dogfood mutates binary paths or records a boundary.
setup_case dashboard-freshness-mismatch
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
printf 'stale-dashboard-stamp\n' >"$case_dashboard_stamp"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_DASHBOARD_SOURCE_STAMP=stale-dashboard-stamp \
  >"$case_output" 2>&1; then
  fail 'stale dashboard bundle unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -s "$case_log" || fail 'stale dashboard bundle executed a lifecycle command'
grep -Fq 'dogfood dashboard bundle does not match' "$case_output" ||
  fail 'stale dashboard rejection was not explicit'
test ! -e "$case_state" || fail 'stale dashboard recorded a boundary marker'
assert_no_temporary_install_files

# Skipping the dashboard build would embed a stale bundle, so the mere presence
# of the escape hatch is refused before anything is inspected or mutated.
setup_case skip-dashboard-build
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
if run_case TRACEDECAY_SKIP_DASHBOARD_BUILD=1 >"$case_output" 2>&1; then
  fail 'skipped dashboard build unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -s "$case_log" || fail 'skipped dashboard build executed a lifecycle command'
grep -Fq 'dogfood refuses TRACEDECAY_SKIP_DASHBOARD_BUILD' "$case_output" ||
  fail 'skipped dashboard build rejection was not explicit'
test ! -e "$case_state" || fail 'skipped dashboard build recorded a boundary marker'
assert_no_temporary_install_files

# A later dogfood attempt may fail before its own boundary after an earlier
# attempt permanently forbade old binaries. Its rollback marker must remain a
# valid v2 state and preserve that forbidden policy.
setup_case pre-boundary-failure-after-reached-marker
write_fake_binary "$case_source" newer
write_fake_binary "$installed" retained-new-installed
write_fake_binary "$staged" retained-new-staged
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
write_test_marker \
  "$case_state" previous-attempt-retry04 validated reached forbidden verified-new-version
fail_once="$case_root/install-failed"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_PREFIX="$installed.new." \
  TRACEDECAY_DOGFOOD_TEST_FAIL_INSTALL_ONCE="$fail_once" \
  >"$case_output" 2>&1; then
  fail 'post-reached pre-boundary install failure unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -s "$case_log" || fail 'post-reached pre-boundary failure executed a binary'
assert_boundary outcome safe-rollback-complete
assert_boundary attempt_boundary not-reached
assert_boundary old_binary_policy forbidden
assert_boundary managed_daemon unchanged
assert_valid_boundary_marker
assert_no_temporary_install_files

# Recovery markers are parsed as a strict, checksummed state machine. A
# modified marker fails closed before either binary path changes.
setup_case tampered-marker
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
write_test_marker \
  "$case_state" previous-attempt-0001 validated reached forbidden verified-new-version
printf 'tampered=true\n' >>"$case_state"
if run_case >"$case_output" 2>&1; then
  fail 'tampered marker unexpectedly validated'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -s "$case_log" || fail 'tampered marker allowed a binary to execute'
grep -Fq 'invalid dogfood migration marker' "$case_output" ||
  fail 'tampered marker rejection was not actionable'

# The retained-binary binding is inside the v3 checksum envelope. Editing only
# that field must fail before any binary path is executed or replaced.
setup_case tampered-v3-retained-binding
write_fake_binary "$case_source" new
write_fake_binary "$installed" retained-new
write_fake_binary "$staged" retained-staged
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
write_test_marker_v3 \
  "$case_state" previous-attempt-0001b forward-recovery-required reached forbidden \
  inactive "$(binary_checksum "$installed")"
sed -i \
  's/^retained_binary_sha256=.*/retained_binary_sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' \
  "$case_state"
if run_case >"$case_output" 2>&1; then
  fail 'tampered v3 retained binding unexpectedly validated'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -s "$case_log" || fail 'tampered v3 binding allowed a binary to execute'
grep -Fq 'invalid dogfood migration marker checksum' "$case_output" ||
  fail 'tampered v3 binding rejection did not identify its invalid checksum'

# Even a correctly checksummed marker cannot encode an impossible/stale
# transition.
setup_case stale-marker-transition
write_fake_binary "$case_source" new
install_old_pair
write_test_marker \
  "$case_state" previous-attempt-0002 safe-rollback-complete reached forbidden inactive
if run_case >"$case_output" 2>&1; then
  fail 'stale marker transition unexpectedly validated'
fi
test ! -s "$case_log" || fail 'stale marker allowed a binary to execute'
grep -Fq 'invalid dogfood migration marker transition' "$case_output" ||
  fail 'stale marker transition rejection was not explicit'

# Marker permissions and path identity are part of validation.
setup_case marker-permissions
write_fake_binary "$case_source" new
install_old_pair
write_test_marker \
  "$case_state" previous-attempt-0003 validated reached forbidden verified-new-version
chmod 0644 "$case_state"
if run_case >"$case_output" 2>&1; then
  fail 'over-permissive marker unexpectedly validated'
fi
test ! -s "$case_log" || fail 'over-permissive marker allowed a binary to execute'
grep -Fq 'must have mode 0600' "$case_output" ||
  fail 'marker permission rejection was not explicit'

setup_case marker-symlink
write_fake_binary "$case_source" new
install_old_pair
external_state="$case_root/external-marker"
write_test_marker \
  "$external_state" previous-attempt-0004 validated reached forbidden verified-new-version
ln -s "$external_state" "$case_state"
if run_case >"$case_output" 2>&1; then
  fail 'marker symlink unexpectedly validated'
fi
test ! -s "$case_log" || fail 'marker symlink allowed a binary to execute'
grep -Fq 'must not be a symlink' "$case_output" ||
  fail 'marker symlink rejection was not explicit'

# A state fsync failure occurs before the boundary and therefore restores old
# paths and never invokes either binary.
setup_case marker-write-failure
write_fake_binary "$case_source" new
install_old_pair
cp "$installed" "$case_root/expected installed"
cp "$staged" "$case_root/expected staged"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_FAIL_SYNC_PATH="$case_state.new." \
  >"$case_output" 2>&1; then
  fail 'marker fsync failure unexpectedly succeeded'
fi
cmp "$case_root/expected installed" "$installed"
cmp "$case_root/expected staged" "$staged"
test ! -s "$case_log" || fail 'state write failure allowed a binary to execute'

# A fully formed but unrenamed temporary marker is never authoritative and is
# removed before the next attempt.
setup_case interrupted-marker-write
write_fake_binary "$case_source" new
write_test_marker \
  "$case_state.new.interrupted" abandoned-attempt-0005 validated reached forbidden verified-new-version
run_case >"$case_output" 2>&1
test "$(cat "$case_log")" = \
  $'new:post-update --strict --mode dogfood-forward-only' ||
  fail 'interrupted marker write changed the authoritative transition'
assert_boundary outcome validated
assert_no_temporary_install_files

# The same forward-only sink covers service acquisition/start and the internal
# post-update health pass before Doctor/version validation.
assert_forward_stage_failure acquire-failure acquire
assert_forward_stage_failure start-failure start
assert_forward_stage_failure health-pass-failure health-pass

# Doctor failure is also post-boundary: the typed post-update mode owns
# deactivation, so the shell never guesses at a daemon subcommand.
setup_case doctor-failure
write_fake_binary "$case_source" new
install_old_pair
daemon_marker="$case_root/daemon-running"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --strict --mode dogfood-forward-only' \
  TRACEDECAY_DOGFOOD_TEST_FAIL_STAGE=doctor \
  >"$case_output" 2>&1; then
  fail 'doctor failure unexpectedly succeeded'
fi
test ! -e "$daemon_marker" || fail 'doctor failure left daemon running'
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'new:post-update --strict --mode dogfood-forward-only\nnew:post-update --mode dogfood-recover-inactive' ||
  fail 'doctor recovery invoked an old binary'
assert_boundary outcome forward-recovery-required
assert_boundary managed_daemon inactive

# The final health probe is still on the forward-only side of the boundary.
setup_case health-failure
write_fake_binary "$case_source" new
install_old_pair
daemon_marker="$case_root/daemon-running"
if run_case \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --strict --mode dogfood-forward-only' \
  TRACEDECAY_DOGFOOD_TEST_FAIL_STAGE=version \
  >"$case_output" 2>&1; then
  fail 'health failure unexpectedly succeeded'
fi
test ! -e "$daemon_marker" || fail 'health failure left daemon running'
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'new:post-update --strict --mode dogfood-forward-only\nnew:post-update --mode dogfood-recover-inactive' ||
  fail 'health recovery invoked an old binary'
assert_boundary outcome forward-recovery-required
assert_boundary managed_daemon inactive

# Interruption during post-update has the same irreversible semantics. The
# active new process is terminated before the new binary stops the daemon.
setup_case interrupted-post-update
write_fake_binary "$case_source" new
install_old_pair
schema_marker="$case_root/schema-opened"
daemon_marker="$case_root/daemon-running"
hold_marker="$case_root/post-update-entered"
hold_release="$case_root/never-release"
run_case_background \
  TRACEDECAY_DOGFOOD_TEST_OPEN_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_OPEN_COMMAND='post-update --strict --mode dogfood-forward-only' \
  TRACEDECAY_DOGFOOD_TEST_SCHEMA_MARKER="$schema_marker" \
  TRACEDECAY_DOGFOOD_TEST_DAEMON_MARKER="$daemon_marker" \
  TRACEDECAY_DOGFOOD_TEST_HOLD_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_HOLD_COMMAND='post-update --strict --mode dogfood-forward-only' \
  TRACEDECAY_DOGFOOD_TEST_HOLD_MARKER="$hold_marker" \
  TRACEDECAY_DOGFOOD_TEST_HOLD_RELEASE="$hold_release" \
  >"$case_output" 2>&1 &
dogfood_pid=$!
for _ in $(seq 1 200); do
  [[ -e "$hold_marker" ]] && break
  sleep 0.02
done
test -e "$hold_marker" || fail 'post-update hold point was not reached'
if flock -n "$case_profile/dogfood.lock" true; then
  kill -TERM "$dogfood_pid" || true
  fail 'dogfood profile lock was released during validation'
fi
kill -TERM "$dogfood_pid"
set +e
wait "$dogfood_pid"
interrupted_status=$?
set -e
test "$interrupted_status" -eq 143 ||
  fail "interrupted dogfood exited $interrupted_status instead of 143"
test ! -e "$daemon_marker" || fail 'interruption left daemon running'
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'new:post-update --strict --mode dogfood-forward-only\nnew:interrupted\nnew:post-update --mode dogfood-recover-inactive' ||
  fail 'interruption escaped the typed post-update lifecycle'
if grep -q '^old-' "$case_log"; then
  fail 'interruption invoked an old binary'
fi
assert_boundary outcome forward-recovery-required
assert_boundary managed_daemon inactive
assert_no_temporary_install_files

# The durable marker and service-manager boundaries are independently
# killable. A hard crash after start/readiness/version/Doctor leaves the
# manager-owned new service running; the retry must quiesce it before using a
# newer binary. Earlier stages retain the old service until quiescing begins.
for crash_stage in \
  marker-fsync \
  unit-rename-dir-fsync \
  manager-reload \
  old-stop \
  new-start \
  readiness \
  version \
  doctor; do
  assert_crash_recovery "$crash_stage"
done

# Successful installation uses the caller-provided build (including paths with
# spaces), never invokes cargo/release upgrade, and commits both atomic copies.
setup_case success
write_fake_binary "$case_source" new
run_case >"$case_output" 2>&1
cmp "$case_source" "$installed"
cmp "$case_source" "$staged"
test "$(cat "$case_log")" = \
  $'new:post-update --strict --mode dogfood-forward-only' ||
  fail 'successful lifecycle commands changed'
if grep -Eq '(^|:)upgrade([[:space:]]|$)' "$case_log"; then
  fail 'dogfood invoked release upgrade'
fi
assert_boundary outcome validated
assert_boundary attempt_boundary reached
assert_boundary old_binary_policy forbidden
assert_no_temporary_install_files

grep -Fq \
  'dogfood = "run --quiet --bin tracedecay -- dogfood"' \
  "$repo_root/.cargo/config.toml"
if grep -Fq 'dogfood-release' "$repo_root/.cargo/config.toml"; then
  fail 'dogfood retained a separate release-build alias'
fi

echo 'dogfood command contract passed'
