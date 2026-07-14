#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage:
  with-isolated-tracedecay-daemon.sh --bin PATH [options] -- COMMAND [ARG...]
  with-isolated-tracedecay-daemon.sh --cargo DIR [options] -- COMMAND [ARG...]

Options:
  --ready-timeout SECONDS  Daemon readiness deadline (default: 60)
  --stop-timeout SECONDS   TERM grace period before KILL (default: 5)
  --lifecycle-label LABEL  Print start/stop messages using LABEL

The harness creates an isolated TraceDecay profile and Unix socket, exports
TRACEDECAY_DATA_DIR and TRACEDECAY_DAEMON_SOCKET to COMMAND, and removes the
profile after the command and daemon have stopped.
EOF
  exit 2
}

ready_timeout=60
stop_timeout=5
lifecycle_label=""
daemon_mode=""
daemon_value=""

while (($# > 0)); do
  case "$1" in
    --bin | --cargo)
      (($# >= 2)) || usage
      [[ -z "$daemon_mode" ]] || usage
      daemon_mode="${1#--}"
      daemon_value="$2"
      shift 2
      ;;
    --ready-timeout)
      (($# >= 2)) || usage
      ready_timeout="$2"
      shift 2
      ;;
    --stop-timeout)
      (($# >= 2)) || usage
      stop_timeout="$2"
      shift 2
      ;;
    --lifecycle-label)
      (($# >= 2)) || usage
      lifecycle_label="$2"
      shift 2
      ;;
    --)
      shift
      break
      ;;
    *)
      usage
      ;;
  esac
done

[[ -n "$daemon_mode" && $# -gt 0 ]] || usage
for value in "$ready_timeout" "$stop_timeout"; do
  [[ "$value" =~ ^[1-9][0-9]*$ ]] || usage
done

case "$daemon_mode" in
  bin)
    [[ -x "$daemon_value" ]] || {
      echo "error: tracedecay binary is not executable: $daemon_value" >&2
      exit 2
    }
    daemon_value="$(cd "$(dirname "$daemon_value")" && pwd)/$(basename "$daemon_value")"
    ;;
  cargo)
    [[ -f "$daemon_value/Cargo.toml" ]] || {
      echo "error: Cargo.toml not found under: $daemon_value" >&2
      exit 2
    }
    daemon_value="$(cd "$daemon_value" && pwd)"
    ;;
esac

command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required for the bounded daemon socket probe" >&2
  exit 2
}
command -v setsid >/dev/null 2>&1 || {
  echo "error: setsid is required for bounded daemon process-group cleanup" >&2
  exit 2
}

run_dir="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-daemon.XXXXXX")"
export TRACEDECAY_DATA_DIR="$run_dir/profile"
export TRACEDECAY_DAEMON_SOCKET="$run_dir/daemon.sock"
export TRACEDECAY_DAEMON_HARNESS_ACTIVE=1
# Keep explicit caller overrides inside the elected daemon's isolated profile.
# A database outside this profile correctly fails the sole-writer authority check.
export TRACEDECAY_GLOBAL_DB="$TRACEDECAY_DATA_DIR/global.db"
daemon_log="$run_dir/daemon.log"
daemon_pid=""
mkdir -p "$TRACEDECAY_DATA_DIR"

print_daemon_log() {
  echo "----- tracedecay daemon log -----" >&2
  if [[ -s "$daemon_log" ]]; then
    cat "$daemon_log" >&2 || true
  else
    echo "(no daemon output captured)" >&2
  fi
}

daemon_group_alive() {
  [[ -n "$daemon_pid" ]] && kill -0 -- "-$daemon_pid" 2>/dev/null
}

stop_daemon() {
  local deadline

  [[ -n "$daemon_pid" ]] || return 0
  if daemon_group_alive; then
    [[ -z "$lifecycle_label" ]] || echo "== stopping $lifecycle_label" >&2
    kill -TERM -- "-$daemon_pid" 2>/dev/null || true
    deadline=$((SECONDS + stop_timeout))
    while daemon_group_alive && ((SECONDS < deadline)); do
      sleep 0.1
    done
    if daemon_group_alive; then
      [[ -z "$lifecycle_label" ]] || echo "== force stopping $lifecycle_label" >&2
      kill -KILL -- "-$daemon_pid" 2>/dev/null || true
    fi
  fi
  wait "$daemon_pid" 2>/dev/null || true
}

cleanup() {
  local status=$?

  trap - EXIT INT TERM
  stop_daemon
  if ((status != 0)); then
    print_daemon_log
  fi
  rm -rf "$run_dir"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[[ -z "$lifecycle_label" ]] || echo "== starting $lifecycle_label"
if [[ "$daemon_mode" == "bin" ]]; then
  setsid "$daemon_value" daemon run --socket "$TRACEDECAY_DAEMON_SOCKET" \
    >"$daemon_log" 2>&1 &
else
  (
    cd "$daemon_value"
    exec setsid cargo run -- daemon run --socket "$TRACEDECAY_DAEMON_SOCKET"
  ) >"$daemon_log" 2>&1 &
fi
daemon_pid=$!

if ! python3 - "$TRACEDECAY_DAEMON_SOCKET" "$daemon_pid" "$ready_timeout" <<'PY'
import os
import socket
import sys
import time

socket_path, daemon_pid, timeout = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
deadline = time.monotonic() + timeout
while time.monotonic() < deadline:
    try:
        os.kill(daemon_pid, 0)
    except ProcessLookupError:
        print("error: tracedecay daemon exited before becoming ready", file=sys.stderr)
        raise SystemExit(1)

    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(min(0.25, max(0.01, deadline - time.monotonic())))
    try:
        client.connect(socket_path)
    except (FileNotFoundError, ConnectionRefusedError, socket.timeout, OSError):
        time.sleep(min(0.1, max(0.0, deadline - time.monotonic())))
    else:
        raise SystemExit(0)
    finally:
        client.close()

print(f"error: tracedecay daemon did not become ready within {timeout} seconds", file=sys.stderr)
raise SystemExit(1)
PY
then
  exit 1
fi

set +e
"$@"
status=$?
set -e

if ((status == 0)) && ! daemon_group_alive; then
  echo "error: tracedecay daemon exited while the smoke command was running" >&2
  status=1
fi
exit "$status"
