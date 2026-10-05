#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -P -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -P -- "$SCRIPT_DIR/.." && pwd)
BIN=
SAMPLES=
REPORT=

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bin)
            [ "$#" -ge 2 ] || { echo "--bin requires an executable" >&2; exit 2; }
            BIN=$2
            shift 2
            ;;
        --samples)
            [ "$#" -ge 2 ] || { echo "--samples requires a positive integer" >&2; exit 2; }
            SAMPLES=$2
            shift 2
            ;;
        --report)
            [ "$#" -ge 2 ] || { echo "--report requires a path" >&2; exit 2; }
            REPORT=$2
            shift 2
            ;;
        --)
            shift
            break
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

[ -n "$BIN" ] || { echo "--bin is required" >&2; exit 2; }
[ -x "$BIN" ] || { echo "benchmark executable is not executable: $BIN" >&2; exit 2; }
[ -n "$SAMPLES" ] || { echo "--samples is required" >&2; exit 2; }
case "$SAMPLES" in
    ''|*[!0-9]*|0) echo "--samples must be a positive integer" >&2; exit 2 ;;
esac
[ -n "$REPORT" ] || { echo "--report is required" >&2; exit 2; }
case "$BIN" in
    /*) ;;
    *) BIN=$PWD/$BIN ;;
esac
cd -- "$REPO_ROOT"
RUSTUP_HOME=$(rustup show home)
BENCH_RUSTUP_TOOLCHAIN=$(rustup show active-toolchain)
RUSTUP_TOOLCHAIN=${BENCH_RUSTUP_TOOLCHAIN%% *}
export RUSTUP_HOME RUSTUP_TOOLCHAIN

case "$REPORT" in
    /*) REPORT_ABS=$REPORT ;;
    *) REPORT_ABS=$REPO_ROOT/$REPORT ;;
esac

PROCESS_HELPER=$REPO_ROOT/scripts/lib/portable_process.py
BENCH_PID=
BENCH_WAITED=0
STAGING_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-tool-performance.XXXXXX")
cleanup() {
    CLEANUP_STATUS=$?
    trap - EXIT
    trap '' HUP INT TERM
    if [ -n "$BENCH_PID" ]; then
        if [ "$BENCH_WAITED" -eq 0 ]; then
            kill -TERM "$BENCH_PID" 2>/dev/null || :
        fi
        set +e
        python3 "$PROCESS_HELPER" stop-group --pid "$BENCH_PID" --grace 1
        STOP_STATUS=$?
        wait "$BENCH_PID" 2>/dev/null
        python3 "$PROCESS_HELPER" group-alive --pid "$BENCH_PID"
        ALIVE_STATUS=$?
        set -e
        if { [ "$STOP_STATUS" -ne 0 ] && [ "$STOP_STATUS" -ne 2 ]; } || [ "$ALIVE_STATUS" -ne 1 ]; then
            echo "benchmark process cleanup failed; staging remains at $STAGING_ROOT" >&2
            exit 1
        fi
    fi
    rm -rf -- "$STAGING_ROOT" || CLEANUP_STATUS=1
    exit "$CLEANUP_STATUS"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

export HOME=$STAGING_ROOT
export USERPROFILE=$HOME
export XDG_CONFIG_HOME=$STAGING_ROOT/xdg-config
export XDG_DATA_HOME=$STAGING_ROOT/xdg-data
export XDG_CACHE_HOME=$STAGING_ROOT/xdg-cache
export CODEX_HOME=$HOME/.codex
export CARGO_HOME=$STAGING_ROOT/cargo-home
export CARGO_TARGET_DIR=$STAGING_ROOT/fixture-target
unset TRACEDECAY_HOME TRACEDECAY_PROFILE TRACEDECAY_PROFILE_DIR TRACEDECAY_DATA_DIR
unset TRACEDECAY_GLOBAL_DB TRACEDECAY_DAEMON_SOCKET
mkdir -p -- "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_NOSYSTEM=1
unset GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
unset GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES

FIXTURE=$STAGING_ROOT/runtime-fixture
mkdir -p -- "$FIXTURE"
cp -R -- "$REPO_ROOT/benchmark_data/runtime/fixtures/project/." "$FIXTURE/"
git -C "$FIXTURE" init -q -b bench
git -C "$FIXTURE" config user.name tracedecay-benchmark
git -C "$FIXTURE" config user.email tracedecay-benchmark@example.invalid
git -C "$FIXTURE" add -- .
git -C "$FIXTURE" commit -q -m 'fixture baseline'
printf '\n' >>"$FIXTURE/README.md"
git -C "$FIXTURE" add -- README.md
git -C "$FIXTURE" commit -q -m 'fixture history point'

export TRACEDECAY_BENCH_REPOS_DIR=$STAGING_ROOT
export TRACEDECAY_BENCH_SMALL_FIXTURE=1
export TRACEDECAY_BENCH_AUDIT=1
export TRACEDECAY_BENCH_SAMPLES=$SAMPLES
export TRACEDECAY_BENCH_REPORT=$REPORT_ABS
if command -v sha256sum >/dev/null 2>&1; then
    export TRACEDECAY_BENCH_BINARY_SHA256=$(sha256sum -- "$BIN" | awk '{print $1}')
else
    export TRACEDECAY_BENCH_BINARY_SHA256=$(shasum -a 256 -- "$BIN" | awk '{print $1}')
fi

set +e
python3 "$PROCESS_HELPER" exec-session --parent-pid "$$" -- "$BIN" "$@" &
BENCH_PID=$!
wait "$BENCH_PID"
STATUS=$?
BENCH_WAITED=1
set -e
exit "$STATUS"
