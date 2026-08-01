#!/usr/bin/env bash
# Live managed-daemon check: runs tests/live_daemon_suite.rs against the
# operator's *real, already-running* TraceDecay daemon, then a doctor pass, and
# prints a PASS/FAIL table.
#
# This is the one entry point that deliberately targets live state. Unlike
# scripts/mcp-conformance-smoke.sh it does NOT redirect HOME/XDG into a temp
# dir — the whole point is to observe the daemon the operator actually uses.
#
# For "did serving get slower", use scripts/perf-gate.sh instead: it indexes
# this repo into a throwaway profile, starts its own daemon, drives concurrent
# readers, and reports percentiles against explicit budgets.
#
# It is strictly read-only:
#   - the suite handshakes with allow_init=false, so it never creates a store;
#   - only read tools are dispatched (no fact_store, no edits, no init, no
#     ingest); tracedecay_memory_status is excluded by default because it
#     repairs derived vectors;
#   - the daemon is never started, stopped, restarted, or signalled. This
#     script must never call systemctl or `tracedecay daemon stop/restart`.
#
# Usage:
#   scripts/live-daemon-check.sh
#   TRACEDECAY_BIN=/usr/local/bin/tracedecay scripts/live-daemon-check.sh
#   TRACEDECAY_LIVE_DAEMON_PROJECT=/path/to/indexed/repo scripts/live-daemon-check.sh
#
# Environment:
#   TRACEDECAY_BIN                              installed binary to cross-check
#                                               (default: tracedecay on PATH)
#   TRACEDECAY_LIVE_DAEMON_PROJECT              indexed project to route to
#                                               (default: repo root)
#   TRACEDECAY_LIVE_DAEMON_SYMBOL               symbol for the search/callers probe
#   TRACEDECAY_LIVE_DAEMON_PATTERN              literal for the grep probe
#   TRACEDECAY_LIVE_DAEMON_ALLOW_MEMORY_STATUS  set to 1 to include the
#                                               repairing memory_status probe
#
# Exit status: 0 when every row passes, 1 otherwise.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRACEDECAY_BIN="${TRACEDECAY_BIN:-tracedecay}"
PROJECT="${TRACEDECAY_LIVE_DAEMON_PROJECT:-$REPO_ROOT}"

# Test names in tests/live_daemon_suite.rs, in the order they are reported.
TESTS=(
  live_daemon_socket_connects_and_serves_the_installed_version
  live_daemon_tools_list_exposes_the_full_catalog
  live_daemon_read_battery_returns_typed_payloads
  live_daemon_read_battery_respects_latency_bounds
  live_daemon_doctor_reports_no_issues
  live_daemon_stays_healthy_after_read_battery
)

ROW_NAMES=()
ROW_STATES=()
ROW_NOTES=()

record() {
  ROW_NAMES+=("$1")
  ROW_STATES+=("$2")
  ROW_NOTES+=("$3")
}

log() { printf '%s\n' "$*" >&2; }

preflight() {
  if ! command -v "$TRACEDECAY_BIN" >/dev/null 2>&1 && [ ! -x "$TRACEDECAY_BIN" ]; then
    log "error: tracedecay binary '$TRACEDECAY_BIN' not found; set TRACEDECAY_BIN"
    exit 1
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    log "error: cargo not found on PATH"
    exit 1
  fi
  if [ ! -d "$PROJECT" ]; then
    log "error: project '$PROJECT' does not exist; set TRACEDECAY_LIVE_DAEMON_PROJECT"
    exit 1
  fi
}

build_suite() {
  log "==> building tests/live_daemon_suite.rs"
  if ! cargo test --manifest-path "$REPO_ROOT/Cargo.toml" \
    --test live_daemon_suite --no-run >&2; then
    log "error: the live daemon suite failed to build"
    exit 1
  fi
}

run_case() {
  local name="$1"
  local output status

  log "==> $name"
  output="$(
    TRACEDECAY_LIVE_DAEMON_TESTS=1 \
    TRACEDECAY_BIN="$TRACEDECAY_BIN" \
    TRACEDECAY_LIVE_DAEMON_PROJECT="$PROJECT" \
      cargo test --manifest-path "$REPO_ROOT/Cargo.toml" \
        --test live_daemon_suite -- --exact --ignored --nocapture "$name" 2>&1
  )"
  status=$?
  printf '%s\n' "$output" >&2

  if [ "$status" -eq 0 ]; then
    # A green exit is not proof the test ran: `--exact` with a name that
    # matched nothing still exits 0, and a test whose env gate declined also
    # reports "ok". Require both the libtest pass line and the absence of the
    # gate's skip note, so a silently inert run is reported as FAIL.
    if ! printf '%s' "$output" | grep -q '1 passed'; then
      record "$name" FAIL "test did not execute (name matched nothing?)"
    elif printf '%s' "$output" | grep -q "set TRACEDECAY_LIVE_DAEMON_TESTS=1"; then
      record "$name" FAIL "env gate declined; the test never contacted the daemon"
    else
      record "$name" PASS ""
    fi
  else
    local note
    note="$(printf '%s' "$output" | grep -m1 -E "^(thread|error)" | cut -c1-72)"
    record "$name" FAIL "${note:-exit $status}"
  fi
}

run_doctor() {
  local output status note
  log "==> tracedecay doctor"
  output="$(cd "$PROJECT" && "$TRACEDECAY_BIN" doctor 2>&1)"
  status=$?
  printf '%s\n' "$output" >&2
  note="$(printf '%s' "$output" | grep -aoE '([0-9]+ issue\(s\), [0-9]+ warning\(s\)\.|[0-9]+ warning\(s\), no issues\.|All checks passed\.)' | tail -1)"
  if [ "$status" -eq 0 ]; then
    record "doctor" PASS "${note:-}"
  else
    record "doctor" FAIL "${note:-exit $status}"
  fi
}

print_table() {
  local failures=0 i state
  printf '\n'
  printf '%-62s  %-4s  %s\n' "CHECK" "RES" "NOTE"
  printf '%-62s  %-4s  %s\n' "$(printf '%.0s-' {1..62})" "----" "----"
  for i in "${!ROW_NAMES[@]}"; do
    state="${ROW_STATES[$i]}"
    [ "$state" = "FAIL" ] && failures=$((failures + 1))
    printf '%-62s  %-4s  %s\n' "${ROW_NAMES[$i]}" "$state" "${ROW_NOTES[$i]}"
  done
  printf '\n'
  if [ "$failures" -eq 0 ]; then
    printf 'live daemon check: PASS (%d checks)\n' "${#ROW_NAMES[@]}"
    return 0
  fi
  printf 'live daemon check: FAIL (%d of %d checks failed)\n' "$failures" "${#ROW_NAMES[@]}"
  return 1
}

main() {
  preflight
  log "binary:  $TRACEDECAY_BIN ($("$TRACEDECAY_BIN" --version 2>/dev/null || echo 'version unavailable'))"
  log "project: $PROJECT"
  build_suite
  for name in "${TESTS[@]}"; do
    run_case "$name"
  done
  run_doctor
  print_table
}

main "$@"
