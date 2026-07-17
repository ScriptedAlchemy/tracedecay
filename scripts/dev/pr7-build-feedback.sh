#!/usr/bin/env bash
# PR7 developer-feedback build evidence (00-plan-set-index rule, PR7+):
# same-host baseline vs candidate — warm incremental no-op check plus a
# representative touched test target; wall time, rebuilt units, CPU/memory,
# visible variance. Absolute timings are diagnostic, not portable gates.
#
# Usage: scripts/dev/pr7-build-feedback.sh <baseline-commit> <candidate-commit>
# Runs each measurement N times from a clean checkout of the commit in a
# temporary clone (never disturbs the invoking checkout). Requires a quiet
# machine for meaningful variance.
set -euo pipefail

BASELINE="${1:?baseline commit}"
CANDIDATE="${2:?candidate commit}"
RUNS="${RUNS:-3}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

measure() {
    local label="$1"
    local commit="$2"
    local dir="$WORK/$label"
    git clone --quiet --shared --no-checkout "$REPO_ROOT" "$dir"
    git -C "$dir" checkout --quiet "$commit"
    pushd "$dir" >/dev/null

    echo "== $label ($commit) =="
    # Cold build once to warm the target dir (excluded from measurements).
    cargo check --all-features >/dev/null 2>&1

    echo "-- warm no-op check x$RUNS --"
    for i in $(seq 1 "$RUNS"); do
        /usr/bin/time -f "wall=%es maxrss=%MKB cpu=%P" \
            cargo check --all-features 2>&1 | grep -E "^wall|Finished" | tail -1
    done

    echo "-- touched-unit incremental check x$RUNS (touch store crate) --"
    for i in $(seq 1 "$RUNS"); do
        touch crates/tracedecay-store/src/lib.rs
        /usr/bin/time -f "wall=%es maxrss=%MKB cpu=%P" \
            cargo check --all-features 2>&1 | grep -cE "^\s+Checking" | \
            xargs -I{} echo "rebuilt_units={}"
        /usr/bin/time -f "wall=%es maxrss=%MKB cpu=%P" true 2>/dev/null || true
    done

    echo "-- representative test target build x$RUNS (session_suite) --"
    for i in $(seq 1 "$RUNS"); do
        touch tests/session_suite/observation_store.rs
        /usr/bin/time -f "wall=%es maxrss=%MKB cpu=%P" \
            cargo nextest run --all-features --no-run \
            -E 'binary(session_suite)' 2>&1 | tail -1
    done

    popd >/dev/null
}

measure baseline "$BASELINE"
measure candidate "$CANDIDATE"
echo "done; report wall/maxrss/cpu per phase with min/max spread across runs"
