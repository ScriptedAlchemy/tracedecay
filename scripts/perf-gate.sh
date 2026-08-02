#!/usr/bin/env bash
# Serving-path performance gate.
#
# Enforces the invariant in docs/SERVING-PATH-PERFORMANCE.md — "a serving-path
# operation performs O(result) work, never O(store)" — end to end, against
# TraceDecay's own codebase:
#
#   PHASE BUILD   build (or accept) a tracedecay binary
#   PHASE INDEX   `tracedecay init` over THIS repo, timed, into a throwaway profile
#   PHASE SERVE   start a daemon that serves that store
#   PHASE LOAD    K concurrent workers hammer search/grep/callers/context for N s
#   PHASE VERDICT metrics JSON + markdown table, checked against the budgets below
#
# The regression class this catches is the one profiled on 2026-08-01: a read
# that went from milliseconds to minutes because per-request work scaled with
# store size. The budgets are therefore deliberately loose — they are tripwires
# for order-of-magnitude regressions, not a microbenchmark. A run that is 3x
# slower than yesterday still passes; a run that is 100x slower does not.
#
# Isolation: the run NEVER touches the operator's real profile. HOME, XDG, and
# every TRACEDECAY_* storage variable are redirected into one throwaway
# directory that is removed on exit, and the daemon is a private foreground
# process on a private socket — no user service is installed, started, stopped,
# or signalled.
#
# Usage:
#   scripts/perf-gate.sh
#   TRACEDECAY_PERF_BIN=/path/to/tracedecay scripts/perf-gate.sh   # skip the build
#   PERF_WORKERS=12 PERF_DURATION_SECONDS=120 scripts/perf-gate.sh
#   PERF_TARGET_REPO=/other/repo PERF_SEED_SYMBOLS="Foo bar" scripts/perf-gate.sh
#
# Exit status: 0 when every budget holds, 1 when any budget is exceeded, 2 on a
# harness/preflight error (which is never a performance verdict).

set -uo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# BUDGETS — the entire pass/fail contract of this gate lives in this block.
#
# Sized for a 2-4 core GitHub runner building in release mode. Raise one only
# with a recorded reason; a budget that has to grow to stay green is usually
# reporting a real regression rather than runner noise.
# ─────────────────────────────────────────────────────────────────────────────
PERF_BUDGET_INDEX_SECONDS="${PERF_BUDGET_INDEX_SECONDS:-900}"           # full index of this repo
PERF_BUDGET_WARM_P95_SECONDS="${PERF_BUDGET_WARM_P95_SECONDS:-10}"      # p95 of any read tool under load
PERF_BUDGET_MAX_CALL_SECONDS="${PERF_BUDGET_MAX_CALL_SECONDS:-60}"      # slowest single call in the run
PERF_BUDGET_DAEMON_RSS_MB="${PERF_BUDGET_DAEMON_RSS_MB:-6144}"          # peak daemon process-group RSS
PERF_BUDGET_MAX_ERROR_RATE="${PERF_BUDGET_MAX_ERROR_RATE:-0.05}"        # failed calls / total calls
PERF_BUDGET_MIN_NODE_COUNT="${PERF_BUDGET_MIN_NODE_COUNT:-10000}"       # proves we indexed the real repo
PERF_BUDGET_MIN_THROUGHPUT_RPS="${PERF_BUDGET_MIN_THROUGHPUT_RPS:-0.5}" # calls/s across all workers
# ─────────────────────────────────────────────────────────────────────────────

# Reindex-under-load. When > 0, that many private clones of the target repo are
# mounted into the SAME daemon during the load window, so the read battery is
# measured while the daemon is running full cold code-index builds — the
# "agent worktrees reindexing while a live tool battery runs" shape. This is
# the probe for docs/SERVING-PATH-PERFORMANCE.md Principle 2: indexing races to
# idle at machine width, and interactive reads stay fast because of the
# reserved core slice, not because indexing was slowed down.
PERF_REINDEX_WORKTREES="${PERF_REINDEX_WORKTREES:-0}"

# Load shape. Overridable so a laptop can run a shorter pass than CI.
PERF_WORKERS="${PERF_WORKERS:-6}"
PERF_DURATION_SECONDS="${PERF_DURATION_SECONDS:-60}"
PERF_CARGO_PROFILE="${PERF_CARGO_PROFILE:-release}"
PERF_DAEMON_READY_TIMEOUT="${PERF_DAEMON_READY_TIMEOUT:-300}"
PERF_INDEX_TIMEOUT="${PERF_INDEX_TIMEOUT:-$((PERF_BUDGET_INDEX_SECONDS + 120))}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The repo under test. Defaults to this checkout — indexing TraceDecay with
# TraceDecay is the point of the gate. Overridable so the harness itself can be
# smoke-tested against a tiny fixture in seconds, and so a bigger corpus can be
# substituted without editing the script.
PERF_TARGET_REPO="${PERF_TARGET_REPO:-$REPO_ROOT}"
PERF_TARGET_REPO="$(cd "$PERF_TARGET_REPO" 2>/dev/null && pwd)" ||
  { printf 'perf-gate: PERF_TARGET_REPO does not exist\n' >&2; exit 2; }
# Symbols/patterns the load phase queries. They only need to exist in the
# target repo; the node-id probe walks the list until one resolves.
IFS=' ' read -r -a PERF_SEED_SYMBOLS <<<"${PERF_SEED_SYMBOLS:-DaemonHandshake TraceDecay call_default_tool default_socket_path run_read_battery}"
PERF_OUTPUT_DIR="${PERF_OUTPUT_DIR:-$REPO_ROOT/target/perf-gate}"
METRICS_JSON="$PERF_OUTPUT_DIR/perf-gate-metrics.json"

log() { printf '%s\n' "$*" >&2; }
die() {
  log "perf-gate: $*"
  exit 2
}

# ── preflight ────────────────────────────────────────────────────────────────

for required in python3 setsid ps timeout awk; do
  command -v "$required" >/dev/null 2>&1 || die "$required is required"
done
[[ -n "${EPOCHREALTIME:-}" ]] || die "bash 5+ is required (EPOCHREALTIME is unset)"
[[ -f "$REPO_ROOT/Cargo.toml" ]] || die "cannot locate the repo root from ${BASH_SOURCE[0]}"

# ── throwaway profile ────────────────────────────────────────────────────────
#
# Kept short on purpose: the daemon socket lives under this directory and a
# Unix `sun_path` is capped at ~108 bytes, so a long TMPDIR silently turns
# every connect() into a confusing "stale socket" error.
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tdperf.XXXXXX")" || die "could not create a run directory"
DAEMON_SOCKET="$RUN_DIR/d.sock"
if ((${#DAEMON_SOCKET} > 100)); then
  rm -rf "$RUN_DIR"
  die "socket path '$DAEMON_SOCKET' is too long for a Unix socket; set TMPDIR to something shorter"
fi

DAEMON_PID=""
SAMPLER_PID=""
DAEMON_LOG="$RUN_DIR/daemon.log"
RSS_SAMPLES="$RUN_DIR/rss.samples"
CALLS_DIR="$RUN_DIR/calls"

WORKER_PIDS=()

# Liveness is always asked of the PID, never of the process group: `setsid`
# only becomes a group leader once its exec completes, so a group probe
# immediately after `&` reports "dead" for a process that is very much alive.
process_alive() { [[ -n "${1:-}" ]] && kill -0 "$1" 2>/dev/null; }

# Signal both the process group (to catch children) and the PID itself (in case
# the group does not exist yet, or the process never became a leader).
signal_tree() {
  kill -"$2" -- "-$1" 2>/dev/null || true
  kill -"$2" "$1" 2>/dev/null || true
}

# Stop a process and everything it spawned. Never blocks on `wait` for a
# process that is still running — that is how a teardown turns into a hang.
stop_tree() {
  local pid="$1" label="$2" deadline
  [[ -n "$pid" ]] || return 0
  if process_alive "$pid"; then
    signal_tree "$pid" TERM
    deadline=$((SECONDS + 10))
    while process_alive "$pid" && ((SECONDS < deadline)); do sleep 0.2; done
    if process_alive "$pid"; then
      log "perf-gate: $label did not exit on TERM; sending KILL"
      signal_tree "$pid" KILL
      deadline=$((SECONDS + 5))
      while process_alive "$pid" && ((SECONDS < deadline)); do sleep 0.2; done
    fi
  fi
  if process_alive "$pid"; then
    log "perf-gate: WARNING $label (pid $pid) survived KILL"
  else
    wait "$pid" 2>/dev/null || true
  fi
  return 0
}

# Unconditional teardown. Workers go first so they stop issuing new calls, then
# the sampler, then the daemon; whatever the exit path, nothing this script
# started is left behind.
teardown() {
  local status=$? pid
  trap - EXIT INT TERM
  for pid in ${WORKER_PIDS[@]+"${WORKER_PIDS[@]}"}; do
    stop_tree "$pid" "load worker"
  done
  stop_tree "${REINDEX_PID:-}" "reindex driver"
  stop_tree "$SAMPLER_PID" "rss sampler"
  stop_tree "$DAEMON_PID" "daemon"
  if ((status != 0)) && [[ -s "$DAEMON_LOG" ]]; then
    log "----- daemon log (last 40 lines) -----"
    tail -40 "$DAEMON_LOG" >&2 || true
  fi
  rm -rf "$RUN_DIR"
  [[ -e "$RUN_DIR" ]] && log "perf-gate: WARNING run directory $RUN_DIR survived teardown"
  exit "$status"
}
trap teardown EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$RUN_DIR/home/.local/share" "$RUN_DIR/home/.config" "$RUN_DIR/profile" "$CALLS_DIR"
mkdir -p "$PERF_OUTPUT_DIR"

# ── PHASE BUILD ──────────────────────────────────────────────────────────────
# Runs before the environment is redirected so cargo keeps the real HOME, and
# with it the real ~/.cargo registry and toolchain.

if [[ -n "${TRACEDECAY_PERF_BIN:-}" ]]; then
  [[ -x "$TRACEDECAY_PERF_BIN" ]] || die "TRACEDECAY_PERF_BIN '$TRACEDECAY_PERF_BIN' is not executable"
  BIN="$(cd "$(dirname "$TRACEDECAY_PERF_BIN")" && pwd)/$(basename "$TRACEDECAY_PERF_BIN")"
  log "==> PHASE BUILD: using prebuilt binary $BIN"
else
  command -v cargo >/dev/null 2>&1 || die "cargo is required unless TRACEDECAY_PERF_BIN is set"
  log "==> PHASE BUILD: cargo build --profile $PERF_CARGO_PROFILE --bin tracedecay"
  # No CARGO_TARGET_DIR override: .cargo/config.toml already pins a repo-local
  # target dir, which is exactly what the CI cache restores.
  (cd "$REPO_ROOT" && cargo build --locked --profile "$PERF_CARGO_PROFILE" --bin tracedecay) >&2 ||
    die "the tracedecay binary failed to build"
  BUILD_DIR="$PERF_CARGO_PROFILE"
  [[ "$BUILD_DIR" == "dev" ]] && BUILD_DIR="debug"
  BIN="$REPO_ROOT/target/$BUILD_DIR/tracedecay"
  [[ -x "$BIN" ]] || die "expected a binary at $BIN after the build"
fi
BUILD_VERSION="$("$BIN" --version 2>/dev/null | tr -d '\n' || echo unknown)"
log "    binary: $BIN ($BUILD_VERSION)"

# ── isolation ────────────────────────────────────────────────────────────────
# Everything below this line runs against the throwaway profile only.

unset TRACEDECAY_HOME TRACEDECAY_PROFILE TRACEDECAY_PROFILE_DIR
export HOME="$RUN_DIR/home"
export XDG_DATA_HOME="$RUN_DIR/home/.local/share"
export XDG_CONFIG_HOME="$RUN_DIR/home/.config"
export TRACEDECAY_DATA_DIR="$RUN_DIR/profile"
export TRACEDECAY_DAEMON_SOCKET="$DAEMON_SOCKET"
export TRACEDECAY_GLOBAL_DB="$RUN_DIR/profile/global.db"
export TRACEDECAY_DISABLE_GLOBAL_DB=1

case "$TRACEDECAY_DATA_DIR" in
  "$RUN_DIR"/*) ;;
  *) die "refusing to run: TRACEDECAY_DATA_DIR '$TRACEDECAY_DATA_DIR' escaped the run directory" ;;
esac

elapsed_since() { awk -v a="$1" -v b="$EPOCHREALTIME" 'BEGIN{printf "%.3f", b - a}'; }

# ── PHASE INDEX ──────────────────────────────────────────────────────────────

log "==> PHASE INDEX: indexing $PERF_TARGET_REPO into $TRACEDECAY_DATA_DIR"
index_start="$EPOCHREALTIME"
if ! (cd "$PERF_TARGET_REPO" && timeout "$PERF_INDEX_TIMEOUT" "$BIN" init) >"$RUN_DIR/init.log" 2>&1; then
  log "----- init log (last 40 lines) -----"
  tail -40 "$RUN_DIR/init.log" >&2 || true
  die "\`tracedecay init\` failed or exceeded ${PERF_INDEX_TIMEOUT}s"
fi
INDEX_SECONDS="$(elapsed_since "$index_start")"
log "    indexed in ${INDEX_SECONDS}s"

# ── PHASE SERVE ──────────────────────────────────────────────────────────────

log "==> PHASE SERVE: starting a private daemon on $DAEMON_SOCKET"
setsid "$BIN" daemon run --socket "$DAEMON_SOCKET" >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

ready_deadline=$((SECONDS + PERF_DAEMON_READY_TIMEOUT))
until [[ -S "$DAEMON_SOCKET" ]]; do
  process_alive "$DAEMON_PID" || die "the daemon exited before binding its socket"
  ((SECONDS < ready_deadline)) || die "the daemon did not bind its socket within ${PERF_DAEMON_READY_TIMEOUT}s"
  sleep 0.2
done

# A bound socket only proves the listener exists; one read tool that answers is
# the real readiness signal. This first call also absorbs cold-open cost, which
# docs/SERVING-PATH-PERFORMANCE.md sanctions as the one slow path.
td() {
  local tool="$1" args="$2"
  "$BIN" tool "$tool" --args "$args" --project "$PERF_TARGET_REPO" --json
}

cold_start="$EPOCHREALTIME"
until td status '{"format":"json"}' >"$RUN_DIR/status.json" 2>"$RUN_DIR/status.err"; do
  process_alive "$DAEMON_PID" || die "the daemon exited before answering tracedecay_status"
  if ((SECONDS >= ready_deadline)); then
    log "----- last status error -----"
    tail -10 "$RUN_DIR/status.err" >&2 || true
    die "the daemon did not answer tracedecay_status within ${PERF_DAEMON_READY_TIMEOUT}s"
  fi
  sleep 1
done
COLD_STATUS_SECONDS="$(elapsed_since "$cold_start")"
log "    daemon ready; first tracedecay_status answered in ${COLD_STATUS_SECONDS}s"

# The CLI wraps every payload in an MCP content envelope, so the graph counts
# live in content[0].text as an embedded JSON document.
python3 - "$RUN_DIR/status.json" >"$RUN_DIR/counts.env" <<'PY' || die "could not parse tracedecay_status"
import json, sys

envelope = json.load(open(sys.argv[1]))
payload = json.loads(envelope["content"][0]["text"])
for name, key in (
    ("NODE_COUNT", "node_count"),
    ("EDGE_COUNT", "edge_count"),
    ("FILE_COUNT", "file_count"),
    ("DB_SIZE_BYTES", "db_size_bytes"),
):
    print(f"{name}={int(payload.get(key, 0))}")
PY
# shellcheck disable=SC1090
source "$RUN_DIR/counts.env"
log "    graph: ${NODE_COUNT} nodes / ${EDGE_COUNT} edges / ${FILE_COUNT} files / ${DB_SIZE_BYTES} bytes"

# Resolve one real node id so the callers probe traverses the graph instead of
# erroring on a made-up id. Walks the seed list so one renamed symbol cannot
# turn the gate into a harness error.
NODE_ID=""
for seed in "${PERF_SEED_SYMBOLS[@]}"; do
  td search "$(printf '{"query":"%s","limit":10,"format":"json"}' "$seed")" \
    >"$RUN_DIR/search.json" 2>/dev/null || continue
  NODE_ID="$(python3 - "$RUN_DIR/search.json" <<'PY'
import json, sys

try:
    envelope = json.load(open(sys.argv[1]))
    payload = json.loads(envelope["content"][0]["text"])
    for hit in payload.get("results", []):
        node_id = hit.get("node_id") or hit.get("id")
        if node_id:
            print(node_id)
            break
except Exception:
    pass
PY
  )"
  [[ -n "$NODE_ID" ]] && break
done
[[ -n "$NODE_ID" ]] ||
  die "none of the seed symbols (${PERF_SEED_SYMBOLS[*]}) resolved to a node; set PERF_SEED_SYMBOLS"
log "    callers probe node: $NODE_ID"

# ── PHASE LOAD ───────────────────────────────────────────────────────────────

SEARCH_QUERIES=("${PERF_SEED_SYMBOLS[@]}")
GREP_PATTERNS=("${PERF_SEED_SYMBOLS[@]}")
CONTEXT_TASKS=(
  "how does the daemon serve tool calls"
  "where is the code index generation validated"
  "how are search results ranked"
  "how does storage retention work"
)

# One call. Records `tool,milliseconds,ok|err` — the raw sample stream the
# verdict phase aggregates.
timed_call() {
  local record="$1" tool="$2" args="$3" start status
  start="$EPOCHREALTIME"
  if "$BIN" tool "$tool" --args "$args" --project "$PERF_TARGET_REPO" --json >/dev/null 2>&1; then
    status=ok
  else
    status=err
  fi
  printf '%s,%s,%s\n' "$tool" "$(awk -v a="$start" -v b="$EPOCHREALTIME" 'BEGIN{printf "%.1f", (b-a)*1000}')" \
    "$status" >>"$record"
}

# A worker is a plain process issuing sequential CLI calls, which is exactly how
# agents drive the daemon in production (every hook and tool invocation is its
# own process). K of them at once reproduce the "agents saturate the daemon with
# hundreds of concurrent calls" shape from docs/SERVING-PATH-PERFORMANCE.md.
# Per-call process startup is a fixed tens-of-milliseconds constant and cannot
# hide the order-of-magnitude regressions these budgets exist to catch.
worker() {
  local id="$1" deadline="$2" record="$CALLS_DIR/worker-$1.csv"
  local i=0 slot q g c
  : >"$record"
  while (($(date +%s) < deadline)); do
    slot=$((i + id))
    q="${SEARCH_QUERIES[$((slot % ${#SEARCH_QUERIES[@]}))]}"
    g="${GREP_PATTERNS[$((slot % ${#GREP_PATTERNS[@]}))]}"
    c="${CONTEXT_TASKS[$((slot % ${#CONTEXT_TASKS[@]}))]}"
    case $((slot % 4)) in
      0) timed_call "$record" search "$(printf '{"query":"%s","limit":10,"format":"json"}' "$q")" ;;
      1) timed_call "$record" grep "$(printf '{"pattern":"%s","fixed_strings":true,"max_results":20,"format":"json"}' "$g")" ;;
      2) timed_call "$record" callers "$(printf '{"node_id":"%s","max_depth":2,"format":"json"}' "$NODE_ID")" ;;
      3) timed_call "$record" context "$(printf '{"task":"%s","max_nodes":20,"format":"json"}' "$c")" ;;
    esac
    i=$((i + 1))
  done
}

# Warm-up: one round of every tool, excluded from the reported window so the
# gate measures steady-state serving rather than first-touch lazy loading.
log "==> PHASE LOAD: warming up"
WARMUP_RECORD="$RUN_DIR/warmup.csv"
WARMUP_SEED="${PERF_SEED_SYMBOLS[0]}"
: >"$WARMUP_RECORD"
timed_call "$WARMUP_RECORD" search "$(printf '{"query":"%s","limit":10,"format":"json"}' "$WARMUP_SEED")"
timed_call "$WARMUP_RECORD" grep "$(printf '{"pattern":"%s","fixed_strings":true,"max_results":20,"format":"json"}' "$WARMUP_SEED")"
timed_call "$WARMUP_RECORD" callers "$(printf '{"node_id":"%s","max_depth":2,"format":"json"}' "$NODE_ID")"
timed_call "$WARMUP_RECORD" context '{"task":"how does the daemon serve tool calls","max_nodes":20,"format":"json"}'

log "==> PHASE LOAD: $PERF_WORKERS workers x ${PERF_DURATION_SECONDS}s"

# Peak RSS of the whole daemon process group, sampled once a second while the
# load runs.
setsid bash -c '
  while :; do
    ps -o rss= -g "$1" 2>/dev/null | awk "{sum += \$1} END {if (sum > 0) print sum}"
    sleep 1
  done
' _ "$DAEMON_PID" >"$RSS_SAMPLES" 2>/dev/null &
SAMPLER_PID=$!

# Reindex driver: keeps the daemon building fresh code-index generations for
# the whole load window. Each `status` call against an unmounted project root
# mounts that worktree and triggers its cold reconcile, so cycling over N
# private clones holds the indexing pipeline busy while the workers read.
REINDEX_PID=""
if ((PERF_REINDEX_WORKTREES > 0)); then
  log "==> PHASE LOAD: preparing $PERF_REINDEX_WORKTREES reindex clone(s)"
  REINDEX_ROOTS=()
  for ((r = 0; r < PERF_REINDEX_WORKTREES; r++)); do
    clone="$RUN_DIR/reindex-$r"
    if git clone --local --quiet "$PERF_TARGET_REPO" "$clone" >/dev/null 2>&1; then
      REINDEX_ROOTS+=("$clone")
    else
      log "    WARNING could not clone $PERF_TARGET_REPO into $clone"
    fi
  done
  ((${#REINDEX_ROOTS[@]} > 0)) || die "no reindex clone could be created"
  reindex_driver() {
    local deadline="$1" root
    while (($(date +%s) < deadline)); do
      for root in "${REINDEX_ROOTS[@]}"; do
        (($(date +%s) < deadline)) || break
        # Drop the mount so the next touch is a cold full build again.
        rm -rf "$root/.tracedecay" 2>/dev/null || true
        "$BIN" tool status "{\"format\":\"json\"}" --project "$root" --json >/dev/null 2>&1 || true
        "$BIN" tool search '{"query":"TraceDecay","limit":10,"format":"json"}' \
          --project "$root" --json >/dev/null 2>&1 || true
      done
    done
  }
fi

WORKER_PIDS=()
load_deadline=$(($(date +%s) + PERF_DURATION_SECONDS))
load_start="$EPOCHREALTIME"
if ((PERF_REINDEX_WORKTREES > 0)); then
  reindex_driver "$load_deadline" &
  REINDEX_PID=$!
  log "    reindex driver running over ${#REINDEX_ROOTS[@]} clone(s)"
fi
for ((w = 0; w < PERF_WORKERS; w++)); do
  worker "$w" "$load_deadline" &
  WORKER_PIDS+=("$!")
done
for pid in "${WORKER_PIDS[@]}"; do
  wait "$pid" 2>/dev/null || true
done
LOAD_SECONDS="$(elapsed_since "$load_start")"

stop_tree "$REINDEX_PID" "reindex driver"
REINDEX_PID=""
stop_tree "$SAMPLER_PID" "rss sampler"
SAMPLER_PID=""

# The daemon must have survived its own load test.
if process_alive "$DAEMON_PID"; then DAEMON_SURVIVED=true; else DAEMON_SURVIVED=false; fi
log "    load window ${LOAD_SECONDS}s; daemon survived: $DAEMON_SURVIVED"

# ── PHASE VERDICT ────────────────────────────────────────────────────────────

log "==> PHASE VERDICT"
PERF_CALLS_DIR="$CALLS_DIR" \
  PERF_RSS_SAMPLES="$RSS_SAMPLES" \
  PERF_METRICS_JSON="$METRICS_JSON" \
  PERF_SUMMARY_MD="$RUN_DIR/summary.md" \
  PERF_INDEX_SECONDS="$INDEX_SECONDS" \
  PERF_LOAD_SECONDS="$LOAD_SECONDS" \
  PERF_COLD_STATUS_SECONDS="$COLD_STATUS_SECONDS" \
  PERF_NODE_COUNT="$NODE_COUNT" \
  PERF_EDGE_COUNT="$EDGE_COUNT" \
  PERF_FILE_COUNT="$FILE_COUNT" \
  PERF_DB_SIZE_BYTES="$DB_SIZE_BYTES" \
  PERF_DAEMON_SURVIVED="$DAEMON_SURVIVED" \
  PERF_BINARY_VERSION="$BUILD_VERSION" \
  PERF_CARGO_PROFILE="$PERF_CARGO_PROFILE" \
  PERF_WORKERS="$PERF_WORKERS" \
  PERF_REINDEX_WORKTREES="$PERF_REINDEX_WORKTREES" \
  PERF_BUDGET_INDEX_SECONDS="$PERF_BUDGET_INDEX_SECONDS" \
  PERF_BUDGET_WARM_P95_SECONDS="$PERF_BUDGET_WARM_P95_SECONDS" \
  PERF_BUDGET_MAX_CALL_SECONDS="$PERF_BUDGET_MAX_CALL_SECONDS" \
  PERF_BUDGET_DAEMON_RSS_MB="$PERF_BUDGET_DAEMON_RSS_MB" \
  PERF_BUDGET_MAX_ERROR_RATE="$PERF_BUDGET_MAX_ERROR_RATE" \
  PERF_BUDGET_MIN_NODE_COUNT="$PERF_BUDGET_MIN_NODE_COUNT" \
  PERF_BUDGET_MIN_THROUGHPUT_RPS="$PERF_BUDGET_MIN_THROUGHPUT_RPS" \
  python3 - <<'PY'
import glob
import json
import math
import os

env = os.environ
calls_dir = env["PERF_CALLS_DIR"]
metrics_path = env["PERF_METRICS_JSON"]
summary_path = env["PERF_SUMMARY_MD"]

budgets = {
    "index_seconds": float(env["PERF_BUDGET_INDEX_SECONDS"]),
    "warm_p95_seconds": float(env["PERF_BUDGET_WARM_P95_SECONDS"]),
    "max_call_seconds": float(env["PERF_BUDGET_MAX_CALL_SECONDS"]),
    "daemon_rss_mb": float(env["PERF_BUDGET_DAEMON_RSS_MB"]),
    "max_error_rate": float(env["PERF_BUDGET_MAX_ERROR_RATE"]),
    "min_node_count": float(env["PERF_BUDGET_MIN_NODE_COUNT"]),
    "min_throughput_rps": float(env["PERF_BUDGET_MIN_THROUGHPUT_RPS"]),
}

samples: dict[str, list[float]] = {}
errors: dict[str, int] = {}
for path in sorted(glob.glob(os.path.join(calls_dir, "*.csv"))):
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            tool, milliseconds, status = line.split(",")
            samples.setdefault(tool, []).append(float(milliseconds) / 1000.0)
            if status != "ok":
                errors[tool] = errors.get(tool, 0) + 1


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(quantile * len(ordered)) - 1))
    return ordered[index]


per_tool = {
    tool: {
        "calls": len(values),
        "errors": errors.get(tool, 0),
        "p50_seconds": round(percentile(values, 0.50), 3),
        "p95_seconds": round(percentile(values, 0.95), 3),
        "max_seconds": round(max(values), 3),
    }
    for tool, values in sorted(samples.items())
}

total_calls = sum(stats["calls"] for stats in per_tool.values())
total_errors = sum(stats["errors"] for stats in per_tool.values())
load_seconds = float(env["PERF_LOAD_SECONDS"])
throughput = round(total_calls / load_seconds, 3) if load_seconds > 0 else 0.0
error_rate = round(total_errors / total_calls, 4) if total_calls else 1.0
worst_p95_tool = max(per_tool, key=lambda tool: per_tool[tool]["p95_seconds"]) if per_tool else "none"
worst_p95 = per_tool[worst_p95_tool]["p95_seconds"] if per_tool else 0.0
worst_max = max((stats["max_seconds"] for stats in per_tool.values()), default=0.0)

rss_path = env["PERF_RSS_SAMPLES"]
rss_kb = []
if os.path.exists(rss_path):
    with open(rss_path) as handle:
        rss_kb = [int(token) for token in handle.read().split() if token.isdigit()]
peak_rss_mb = round(max(rss_kb) / 1024.0, 1) if rss_kb else 0.0

index_seconds = float(env["PERF_INDEX_SECONDS"])
node_count = int(env["PERF_NODE_COUNT"])
daemon_survived = env["PERF_DAEMON_SURVIVED"] == "true"

# (label, measured, comparison, budget, unit)
checks = [
    ("index duration", round(index_seconds, 1), "<=", budgets["index_seconds"], "s"),
    ("graph size (indexed the real repo)", node_count, ">=", budgets["min_node_count"], "nodes"),
    (f"worst warm p95 ({worst_p95_tool})", worst_p95, "<=", budgets["warm_p95_seconds"], "s"),
    ("slowest single call", worst_max, "<=", budgets["max_call_seconds"], "s"),
    ("daemon peak RSS", peak_rss_mb, "<=", budgets["daemon_rss_mb"], "MB"),
    ("error rate", error_rate, "<=", budgets["max_error_rate"], ""),
    ("throughput", throughput, ">=", budgets["min_throughput_rps"], "calls/s"),
]

rows = []
failed = []
for label, measured, comparison, budget, unit in checks:
    ok = measured <= budget if comparison == "<=" else measured >= budget
    rows.append((label, f"{measured}{' ' + unit if unit else ''}", comparison,
                 f"{budget}{' ' + unit if unit else ''}", ok))
    if not ok:
        failed.append(f"{label}: {measured} {comparison} {budget} violated")

rows.append(("daemon survived the load", "yes" if daemon_survived else "no", "==", "yes", daemon_survived))
if not daemon_survived:
    failed.append("the daemon died during the load phase")

metrics = {
    "schema": "tracedecay.perf-gate/v1",
    "binary_version": env["PERF_BINARY_VERSION"].strip(),
    "cargo_profile": env["PERF_CARGO_PROFILE"],
    "workers": int(env["PERF_WORKERS"]),
    "reindex_worktrees_under_load": int(env["PERF_REINDEX_WORKTREES"]),
    "load_seconds": load_seconds,
    "index": {
        "seconds": round(index_seconds, 3),
        "node_count": node_count,
        "edge_count": int(env["PERF_EDGE_COUNT"]),
        "file_count": int(env["PERF_FILE_COUNT"]),
        "db_size_bytes": int(env["PERF_DB_SIZE_BYTES"]),
    },
    "serve": {
        "cold_status_seconds": float(env["PERF_COLD_STATUS_SECONDS"]),
        "peak_rss_mb": peak_rss_mb,
        "daemon_survived": daemon_survived,
    },
    "load": {
        "total_calls": total_calls,
        "total_errors": total_errors,
        "error_rate": error_rate,
        "throughput_calls_per_second": throughput,
        "per_tool": per_tool,
    },
    "budgets": budgets,
    "failed_budgets": failed,
    "verdict": "PASS" if not failed else "FAIL",
}

os.makedirs(os.path.dirname(metrics_path), exist_ok=True)
with open(metrics_path, "w") as handle:
    json.dump(metrics, handle, indent=2, sort_keys=True)
    handle.write("\n")

lines = [
    f"## Serving-path perf gate: {metrics['verdict']}",
    "",
    f"`{metrics['binary_version']}` — {metrics['cargo_profile']} profile, "
    f"{metrics['workers']} workers x {load_seconds:.0f}s",
    "",
    "### Index",
    "",
    "| metric | value |",
    "| --- | ---: |",
    f"| duration | {index_seconds:.1f} s |",
    f"| nodes | {node_count} |",
    f"| edges | {metrics['index']['edge_count']} |",
    f"| files | {metrics['index']['file_count']} |",
    f"| store size | {metrics['index']['db_size_bytes'] / 1048576:.1f} MiB |",
    "",
    "### Serving latency under load",
    "",
    "| tool | calls | errors | p50 (s) | p95 (s) | max (s) |",
    "| --- | ---: | ---: | ---: | ---: | ---: |",
]
for tool, stats in per_tool.items():
    lines.append(
        f"| {tool} | {stats['calls']} | {stats['errors']} | {stats['p50_seconds']:.3f} | "
        f"{stats['p95_seconds']:.3f} | {stats['max_seconds']:.3f} |"
    )
lines += [
    f"| **total** | **{total_calls}** | **{total_errors}** | | | |",
    "",
    f"Throughput {throughput} calls/s · daemon peak RSS {peak_rss_mb} MB · "
    f"cold first status {metrics['serve']['cold_status_seconds']:.2f} s",
    "",
    "### Budgets",
    "",
    "| check | measured | | budget | result |",
    "| --- | ---: | :---: | ---: | --- |",
]
for label, measured, comparison, budget, ok in rows:
    lines.append(f"| {label} | {measured} | {comparison} | {budget} | {'PASS' if ok else '**FAIL**'} |")
lines.append("")

report = "\n".join(lines)
with open(summary_path, "w") as handle:
    handle.write(report + "\n")

step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
if step_summary:
    with open(step_summary, "a") as handle:
        handle.write(report + "\n")
else:
    print(report)

raise SystemExit(1 if failed else 0)
PY
VERDICT_STATUS=$?

cp -f "$RUN_DIR/summary.md" "$PERF_OUTPUT_DIR/perf-gate-summary.md" 2>/dev/null || true
log ""
log "perf-gate: metrics written to $METRICS_JSON"
if ((VERDICT_STATUS == 0)); then
  log "perf-gate: PASS"
else
  log "perf-gate: FAIL — see the budget table above"
fi
exit "$VERDICT_STATUS"
