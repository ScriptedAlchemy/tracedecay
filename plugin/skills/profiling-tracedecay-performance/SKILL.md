---
name: profiling-tracedecay-performance
description: 'Use when a TraceDecay daemon is slow, CPU- or memory-heavy, blocked on I/O or locks, or when an optimization needs reproducible before/after performance evidence.'
---

# Profiling TraceDecay Performance

Profile the smallest reproducible production journey in a disposable sandbox.
Use Hotpath to identify slow TraceDecay operations, Linux sampling to attribute
physical cycles, and the efficiency scorecard to prove the product-level win.

Announce: "Using tracedecay:profiling-tracedecay-performance."

## Non-negotiable isolation

Never profile the operator's daemon, home, data directory, socket, profile,
project registry, or stores. Do not attach `perf`, `strace`, or `fatrace` to a
live TraceDecay process. A fresh index of a copied target repository is safe.

For manual captures, create one disposable authority root and pass every
process the same environment:

```bash
set -euo pipefail
: "${TRACEDECAY_BIN:?select a built binary before creating a manual sandbox}"
: "${PROFILE_WORKLOAD:?set the exact sandboxed production command}"
PROFILE_ID="$(date -u +%Y%m%dT%H%M%SZ)-${USER}-$$"
REPORT_ROOT="$PWD/target/profiles/$PROFILE_ID"
RUN_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-profile.XXXXXX")"
mkdir -p "$REPORT_ROOT" "$RUN_ROOT"/{home,data,tmp,project}
cp -a /path/to/target-repo/. "$RUN_ROOT/project/"

export HOME="$RUN_ROOT/home"
export XDG_CONFIG_HOME="$RUN_ROOT/home/.config"
export XDG_DATA_HOME="$RUN_ROOT/home/.local/share"
export TMPDIR="$RUN_ROOT/tmp"
export TRACEDECAY_DATA_DIR="$RUN_ROOT/data"
export TRACEDECAY_GLOBAL_DB="$RUN_ROOT/data/global.db"
export TRACEDECAY_DAEMON_SOCKET="$RUN_ROOT/daemon.sock"

cleanup_profile() {
  rc=$?
  trap - EXIT INT TERM
  for child in "${PERF_PID:-}" "${STAT_PID:-}" "${TRACE_PID:-}"; do
    if test -n "$child" && kill -0 "$child" 2>/dev/null; then
      kill -INT "$child"
      wait "$child" || true
    fi
  done
  if test -n "${DAEMON_PID:-}" && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID"
    wait "$DAEMON_PID" || true
  fi
  rm -rf "$RUN_ROOT"
  exit "$rc"
}
trap cleanup_profile EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
```

Fail closed if a command cannot accept these authorities. Start only the
profiled binary, from the copied repository. For a Hotpath capture, set the
Hotpath report environment below before running this launch block.

```bash
cd "$RUN_ROOT/project"
"$TRACEDECAY_BIN" daemon run --socket "$TRACEDECAY_DAEMON_SOCKET" \
  >"$REPORT_ROOT/daemon.log" 2>&1 &
DAEMON_PID=$!
deadline=$((SECONDS + 30))
until test -S "$TRACEDECAY_DAEMON_SOCKET"; do
  if ! kill -0 "$DAEMON_PID"; then
    wait "$DAEMON_PID"
    exit 1
  fi
  if (( SECONDS >= deadline )); then
    kill -TERM "$DAEMON_PID"
    wait "$DAEMON_PID"
    exit 1
  fi
  sleep 0.05
done
```

Record the binary hash, commit, exact workload, fixture digest, kernel, CPU,
and load average with every capture. Stop and wait for this PID before deleting
the sandbox. Never reuse a sandbox between baseline and candidate runs.

```bash
sha256sum "$TRACEDECAY_BIN" >"$REPORT_ROOT/binary.sha256"
git -C "$RUN_ROOT/project" rev-parse HEAD >"$REPORT_ROOT/fixture-commit.txt"
git -C "$RUN_ROOT/project" status --short >"$REPORT_ROOT/fixture-status.txt"
uname -a >"$REPORT_ROOT/kernel.txt"
lscpu >"$REPORT_ROOT/cpu.txt"
uptime >"$REPORT_ROOT/load.txt"
printf '%s\n' "$PROFILE_WORKLOAD" >"$REPORT_ROOT/workload.txt"
```

`PROFILE_WORKLOAD` must be one exact, repeatable production command that points
to `TRACEDECAY_DAEMON_SOCKET`. Store reports outside `RUN_ROOT`; cleanup may
then remove the disposable authorities without deleting evidence.

## Choose the evidence layer

- **Which TraceDecay operation is slow?** Start with Hotpath timing spans.
- **Where do CPU cycles or waits physically go?** Add `perf`, `strace`, and
  thread/I/O observation after Hotpath narrows the journey.
- **Did the optimization improve the product?** Run the efficiency scorecard
  before and after against the same pinned fixture.

Do not infer physical cost from spans alone. Serialization, SHA-256, allocator,
SQLite, futex, and kernel I/O frames can dominate beneath one measured
operation.

## Prove the baseline first

Use `scripts/efficiency-scorecard.py` before changing production code. It owns
the pinned fixture, isolated HOME/XDG/data/global DB/socket, readiness polling,
and JSON schema. It measures cold index, incremental sync, seal to activation,
tool-call p50/p95, startup/restart, store size, and peak RSS.

Serialize only the build; do not hold the Cargo lease during measurements:

```bash
: "${CARGO_TARGET_DIR:?export the per-worktree target dir printed by scripts/agent-worktree.sh}"
export CARGO_TARGET_DIR
PROFILE_ID="${PROFILE_ID:-$(date -u +%Y%m%dT%H%M%SZ)-${USER}-$$}"
flock -w 7200 /tmp/tracedecay-cargo-heavy.lock bash -lc \
  'CARGO_BUILD_JOBS=4 kache cargo -- build --locked --release -p tracedecay-cli --bin tracedecay'

scripts/efficiency-scorecard.py \
  --binary "$CARGO_TARGET_DIR/release/tracedecay" \
  --label baseline \
  --output "target/efficiency-scorecard-$PROFILE_ID-baseline"
```

After the optimization, rebuild and run the same command with `--label
candidate` and a different output directory. Compare only scorecards with the
same fixture digest and similar host load. Use `--quick` only as a smoke test;
the default repeated run is the evidence.

## Hotpath timing spans

Hotpath is the first resort for "which of our operations is slow." Default
builds remain byte-identical no-ops because the attributes are gated by crate
features.

Build the shipped binary with timing spans; add `hotpath-mcp` only when live
Hotpath MCP inspection is needed:

```bash
flock -w 7200 /tmp/tracedecay-cargo-heavy.lock bash -lc \
  'CARGO_BUILD_JOBS=4 kache cargo -- build --profile perf -p tracedecay-cli --bin tracedecay --features hotpath'
export TRACEDECAY_BIN="$CARGO_TARGET_DIR/perf/tracedecay"
```

Use `--features hotpath,hotpath-mcp` instead only for live Hotpath MCP
inspection.

Run the sandboxed daemon and exact workload with:

```bash
export HOTPATH_OUTPUT_PATH="$REPORT_ROOT/hotpath.json"
export HOTPATH_OUTPUT_FORMAT=json-pretty
export HOTPATH_REPORT='functions-timing,futures,rw_locks,mutexes,io,threads,debug'
export RUST_LOG='warn,tracedecay_graph_db=debug,tracedecay_code_index_runtime=debug'
```

Keep workload boundaries explicit: fresh start, deterministic input, exact
request count, readiness condition, and clean shutdown. Rank spans by inclusive
time and call count, then inspect child spans before instrumenting more code.
High total time can mean one slow call or an N+1 pattern; preserve that
distinction.

### Instrumentation convention

- Declare `hotpath = { version = "0.24", ... }` once in the workspace and use
  `hotpath.workspace = true` in crates.
- Give each crate a `hotpath` feature that enables `hotpath/hotpath` and
  cascades to instrumented dependency crates.
- Prefer `#[hotpath::measure(label = "...", future = true)]` on the exact async
  operation under investigation.
- Use `#[hotpath::measure_all]` on an impl only when every method is valid to
  instrument.

`measure_all` must exclude `const fn`: wrapping a const method causes
E0015/E0493 in all-target builds. Broad impl instrumentation can also trigger
query-depth overflow when `test-helpers` expands the workspace spine. Narrow
the annotations; do not raise compiler limits or weaken the all-target check.

Keep instrumentation changes in separate mechanical commits per crate. This
makes profiling-only diffs reviewable and lets an optimization stand on its
own evidence.

## System-level cycle and wait attribution

Use the sandbox PID. First identify sustained hot threads:

```bash
top -H -b -d 1 -n 30 -p "$DAEMON_PID" \
  >"$REPORT_ROOT/top-threads.txt"
pidstat -t -u -d -p "$DAEMON_PID" 1 30 \
  >"$REPORT_ROOT/pidstat-threads.txt"
ps -L -p "$DAEMON_PID" -o pid,tid,psr,pcpu,stat,wchan:32,comm \
  >"$REPORT_ROOT/thread-snapshot.txt"
```

Choose the repeatably hot TID or comma-separated TIDs, reproduce the same
workload, and capture DWARF stacks. Bound the capture by operation completion,
not a fixed sleep:

```bash
HOT_TIDS=1234,1235
perf record -F 99 -e cycles:u --call-graph dwarf,16384 \
  -t "$HOT_TIDS" -o "$REPORT_ROOT/perf.data" &
PERF_PID=$!
sleep 1
kill -0 "$PERF_PID"
bash -lc "$PROFILE_WORKLOAD"
WORKLOAD_STATUS=$?
kill -INT "$PERF_PID"
wait "$PERF_PID"
PERF_PID=
test "$WORKLOAD_STATUS" -eq 0
perf report --stdio --children -i "$REPORT_ROOT/perf.data" \
  >"$REPORT_ROOT/perf-report.txt"
```

Render a flamegraph when FlameGraph tools are installed:

```bash
perf script -i "$REPORT_ROOT/perf.data" \
  | stackcollapse-perf.pl >"$REPORT_ROOT/perf.folded"
flamegraph.pl --countname cycles "$REPORT_ROOT/perf.folded" \
  >"$REPORT_ROOT/perf.svg"
```

If wall time is high but CPU samples are sparse, capture waiting and I/O in
separate runs because tracing perturbs latency:

```bash
strace -f -c -S time -p "$DAEMON_PID" \
  -o "$REPORT_ROOT/strace-summary.txt" &
TRACE_PID=$!
sleep 1
kill -0 "$TRACE_PID"
bash -lc "$PROFILE_WORKLOAD"
WORKLOAD_STATUS=$?
kill -INT "$TRACE_PID"
wait "$TRACE_PID"
TRACE_PID=
test "$WORKLOAD_STATUS" -eq 0

fatrace -p "$DAEMON_PID" \
  >"$REPORT_ROOT/fatrace.txt" &
TRACE_PID=$!
sleep 1
kill -0 "$TRACE_PID"
bash -lc "$PROFILE_WORKLOAD"
WORKLOAD_STATUS=$?
kill -INT "$TRACE_PID"
wait "$TRACE_PID"
TRACE_PID=
test "$WORKLOAD_STATUS" -eq 0
```

Run these as separate matched captures. `fatrace` may require elevated
privileges and system support; if unavailable, retain `pidstat -d` plus a
targeted `strace` of
`read,write,pread64,pwrite64,openat,fsync,fdatasync`.
If `perf_event_paranoid` or ptrace policy blocks the sandbox PID, report that
host limitation; never work around it by attaching to a live service. Treat
unresolved or truncated stacks as inconclusive and rebuild with usable debug
symbols before interpreting them.

Use a separate matched run for total counters. Set `COMPLETED_OPS` to the exact
successful operation count produced by the workload; do not count errors or
cancellations:

```bash
COMPLETED_OPS=1
case "$COMPLETED_OPS" in
  ''|*[!0-9]*) echo "COMPLETED_OPS must be a positive integer" >&2; exit 1 ;;
esac
test "$COMPLETED_OPS" -gt 0
perf stat -x, -e task-clock,cycles,instructions,cache-misses,context-switches \
  -t "$HOT_TIDS" -o "$REPORT_ROOT/perf-stat.csv" &
STAT_PID=$!
sleep 1
kill -0 "$STAT_PID"
bash -lc "$PROFILE_WORKLOAD"
WORKLOAD_STATUS=$?
kill -INT "$STAT_PID"
wait "$STAT_PID"
STAT_PID=
test "$WORKLOAD_STATUS" -eq 0
awk -F, -v n="$COMPLETED_OPS" \
  '$3 == "cycles" {
     gsub(/ /, "", $1)
     if ($1 !~ /^[0-9]+([.][0-9]+)?$/) exit 2
     print $1 / n
     found = 1
   }
   END {if (!found) exit 3}' \
  "$REPORT_ROOT/perf-stat.csv" >"$REPORT_ROOT/cycles-per-operation.txt"
```

Read stacks from the measured TraceDecay operation downward:

- Wide `malloc`, `free`, Rust allocation, jemalloc, or mimalloc frames mean
  allocator traffic; allocator-adjacent futex frames indicate arena/allocator
  lock contention.
- Wide SHA-256 and canonical JSON/serde frames mean identity material is being
  repeatedly reserialized or rehashed.
- Futex call count alone is not contention. Require accumulated wait time or
  wide off-CPU stacks and identify the owning caller.
- Many small reads/opens indicate N+1 or metadata-heavy I/O; slow
  `fsync`/`fdatasync` indicates durability latency.

Hotpath once isolated session restore as the slow operation. `perf` then
showed about 25% allocator traffic plus SHA-256 over canonical JSON inside that
span. Removing the redundant work produced a measured 114x improvement. This
is why span and cycle evidence are complementary.

## Evidence and stopping rule

Repeat matched baseline and candidate runs. Report p50/p95 wall time, normalized
cycles, peak RSS, I/O, lock waits, completed work, and correctness. A valid win
reduces both the scorecard metric and the implicated span/stack without moving
cost into startup, activation, errors, cancellation, or incomplete output.

Use the scorecard's default three scenario runs and 25 tool samples; do not
reduce them for evidence. For system captures, take at least three matched
windows and normalize total cycles, CPU seconds, I/O bytes, and accumulated
wait time by the number of successfully completed target operations.

Shut down the exact sandbox PID and retain `REPORT_ROOT`:

```bash
kill -TERM "$DAEMON_PID"
wait "$DAEMON_PID"
DAEMON_PID=
rm -rf "$RUN_ROOT"
RUN_ROOT=
```

Stop instrumenting once the dominant cost is attributable and the scorecard
falsifies or confirms the fix. Remove exploratory instrumentation that did not
earn permanent diagnostic value.

## Common mistakes

- Profiling the live daemon or registry instead of a disposable authority root.
- Raising deadlines, compiler recursion limits, or memory budgets instead of
  fixing measured cost.
- Comparing different fixture digests, readiness conditions, or host load.
- Treating `--quick`, one run, span duration, or syscall count as proof.
- Recording process-wide `perf` before identifying hot threads.
- Holding the shared Cargo lease while benchmarking.
- Sharing a target directory or report path with another agent.
- Adding a second sandbox/scorecard wrapper instead of using the maintained
  `scripts/efficiency-scorecard.py`.
