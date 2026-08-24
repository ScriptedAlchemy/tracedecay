# Shared preamble for the shell gates that test a checked-in guard script.
#
# Source it, do not execute it:
#
#     . "$(dirname "${BASH_SOURCE[0]}")/../scripts/lib/gate-test.sh"
#
# It defines GATE_REPO_ROOT, creates a self-cleaning scratch directory in
# GATE_SCRATCH, and replaces the `set +e` / command-substitution dance every
# one of these gates was open-coding. That pattern is easy to get subtly wrong
# — a forgotten `set -e` leaves the rest of the file unguarded — so it lives
# here once.
#
# Usage:
#
#     gate_run some-guard --flag        # never aborts; records status+output
#     gate_expect_status "label" 1      # or gate_expect_success "label"
#     gate_output_contains "label" "the reason the guard rejected"

# shellcheck shell=bash

GATE_REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
readonly GATE_REPO_ROOT

GATE_SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-gate.XXXXXX")
readonly GATE_SCRATCH
# shellcheck disable=SC2064 # expand GATE_SCRATCH now, not at trap time
trap "rm -rf -- '$GATE_SCRATCH'" EXIT

# Combined stdout+stderr and exit status of the most recent `gate_run`.
GATE_OUTPUT=""
GATE_STATUS=0

# Fail the gate with a message on stderr.
gate_fail() {
  echo "$*" >&2
  exit 1
}

# Run a command without aborting the gate, recording its combined output and
# exit status for the assertions below.
gate_run() {
  set +e
  GATE_OUTPUT=$("$@" 2>&1)
  GATE_STATUS=$?
  set -e
}

# Require the last `gate_run` to have exited with exactly `status`.
gate_expect_status() {
  local label=$1
  local status=$2
  if [[ $GATE_STATUS -ne $status ]]; then
    echo "$GATE_OUTPUT" >&2
    gate_fail "$label: expected exit status $status, got $GATE_STATUS"
  fi
}

# Require the last `gate_run` to have succeeded.
gate_expect_success() {
  gate_expect_status "$1" 0
}

# Require the last `gate_run` to have failed. A guard that rejects for the
# wrong reason is not a guard, so pair this with `gate_output_contains`.
gate_expect_failure() {
  local label=$1
  if [[ $GATE_STATUS -eq 0 ]]; then
    echo "$GATE_OUTPUT" >&2
    gate_fail "$label: expected failure, but the command succeeded"
  fi
}

# Require the last `gate_run` output to mention `substring`.
gate_output_contains() {
  local label=$1
  local substring=$2
  if [[ $GATE_OUTPUT != *"$substring"* ]]; then
    echo "$GATE_OUTPUT" >&2
    gate_fail "$label: expected output containing '$substring'"
  fi
}
