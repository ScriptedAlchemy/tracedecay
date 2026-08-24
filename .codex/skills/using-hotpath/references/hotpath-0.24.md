# Hotpath 0.24 reference for TraceDecay

TraceDecay pins `hotpath`, `hotpath-macros`, and `hotpath-meta` 0.24.0. Prefer the pinned crate source over stale tagged prose for exact flags and MCP schemas.

## Build lanes

Use a distinct process and target directory for each lane. Keep product/default builds free of profiling features.

```bash
CARGO_TARGET_DIR=target/hotpath-off \
  cargo build --locked --profile perf -p tracedecay-cli --bin tracedecay \
  --no-default-features --features production

RUSTFLAGS='--cfg tokio_unstable' CARGO_TARGET_DIR=target/hotpath-timing \
  cargo build --locked --profile perf -p tracedecay-cli --bin tracedecay \
  --no-default-features --features production,hotpath,hotpath-mcp

RUSTFLAGS='--cfg tokio_unstable' CARGO_TARGET_DIR=target/hotpath-alloc \
  cargo build --locked --profile perf -p tracedecay-cli --bin tracedecay \
  --no-default-features --features production,hotpath-alloc,hotpath-mcp

RUSTFLAGS='--cfg tokio_unstable' CARGO_TARGET_DIR=target/hotpath-cpu \
  cargo build --locked --profile perf -p tracedecay-cli --bin tracedecay \
  --no-default-features --features production,hotpath-cpu,hotpath-mcp

RUSTFLAGS='--cfg tokio_unstable' CARGO_TARGET_DIR=target/hotpath-all \
  cargo build --locked --profile perf -p tracedecay-cli --bin tracedecay \
  --no-default-features \
  --features production,hotpath,hotpath-alloc,hotpath-cpu,hotpath-mcp
```

`tokio_unstable` adds blocking-pool, local-queue, steal/poll, remote-schedule, and I/O-driver runtime metrics. Basic Tokio metrics work without it.

CPU profiling is Linux/macOS-only and additionally needs `hotpath-samply`, a `samply` executable, symbols, and OS sampling permissions.

## Exit reports

Exact `HOTPATH_REPORT` sections:

```text
functions-timing, functions-alloc, functions-cpu,
channels, streams, futures, rw_locks, mutexes,
sql, http, server, io, threads, debug
```

Use `all`, `auto`, an exact comma-separated list, or exclusions such as `auto,-threads`. Output formats are `table`, `json`, `json-pretty`, and `none`.

```bash
HOTPATH_OUTPUT_FORMAT=json-pretty \
HOTPATH_OUTPUT_PATH=/tmp/tracedecay-hotpath.json \
HOTPATH_REPORT='functions-timing,channels,futures,rw_locks,mutexes,http,server,io,threads,debug' \
HOTPATH_REPORT_LABEL=timing \
HOTPATH_ENTRIES_LIMIT=256 \
HOTPATH_LOGS_LIMIT=50 \
HOTPATH_TIME_SAMPLING_RATE=0.1 \
HOTPATH_METRICS_PORT=6770 \
HOTPATH_MCP_PORT=6771 \
target/hotpath-timing/perf/tracedecay daemon run
```

The Cargo feature set is the profiling activation authority. A feature-enabled
binary starts Hotpath collection for every invocation; use a feature-off
production binary when profiling must be absent. Without either
`HOTPATH_OUTPUT_PATH` or an explicit `HOTPATH_OUTPUT_FORMAT`, TraceDecay defaults
the format to `none`: live metrics and MCP stay available, but no exit table is
appended to a CLI stream. Hook commands never write reports to protocol stdout;
set `HOTPATH_OUTPUT_PATH` to collect a hook exit report.
The report is emitted when the single process-boundary `HotpathGuard` drops;
TraceDecay's CLI and hook dispatch return an `ExitCode` so the guard always gets
that shutdown boundary.

Hotpath 0.24 keys async timing by static source location. TraceDecay therefore
uses one static `mcp.tool_call` lifetime plus static dispatch-family spans, and
records the exact canonical tool as the bounded `mcp.tool.name` value. A
dynamic future label at one callsite is not per-tool timing: the first observed
label would name every later call at that source location.

Important environment groups:

- output: `HOTPATH_OUTPUT_FORMAT`, `HOTPATH_OUTPUT_PATH`, `HOTPATH_REPORT`, `HOTPATH_REPORT_LABEL`, `NO_COLOR`;
- sampling: `HOTPATH_TIME_SAMPLING_RATE` and resource-specific `FUNCTIONS`, `MUTEXES`, `RW_LOCKS`, `FUTURES`, `CHANNELS`, `IO` variants;
- aggregation: `HOTPATH_ALLOC_CUMULATIVE`, `HOTPATH_ALLOC_METRIC`, `HOTPATH_CPU_INCLUSIVE`, `HOTPATH_EXCLUDE_WRAPPER`, `HOTPATH_FOCUS`;
- bounds: `HOTPATH_LIMIT`, resource-specific limits, `HOTPATH_ENTRIES_LIMIT`, `HOTPATH_LOGS_LIMIT`, `HOTPATH_MAX_LOG_LEN`;
- servers: `HOTPATH_METRICS_PORT`, `HOTPATH_METRICS_SERVER_OFF`, `HOTPATH_METRICS_AUTH_TOKEN`, `HOTPATH_MCP_PORT`, `HOTPATH_MCP_AUTH_TOKEN`;
- SQL/privacy: `HOTPATH_SQL_RAW_LOGS` exposes raw inline SQL and should normally remain off;
- CPU: `HOTPATH_SAMPLY_BIN`, `HOTPATH_SAMPLY_WRAPPER_BIN`, `HOTPATH_CPU_BASELINE_OFF`.

Time sampling is deterministic one-in-k, not probabilistic. Counts remain exact; sampled duration totals are extrapolated. A zero rate is count-only.

## CLI

Install exact 0.24 tools when needed:

```bash
cargo install hotpath --version 0.24.0 --locked --features tui --bin hotpath
cargo install hotpath --version 0.24.0 --locked --features utils --bin hotpath-utils
cargo install hotpath --version 0.24.0 --locked --features hotpath-cpu --bin hotpath-samply
```

Public commands:

```text
hotpath init --agent <claude|codex|opencode>
hotpath console [--metrics-port N] [--metrics-host URL]
                [--metrics-auth-token TOKEN] [--refresh-interval MS]

hotpath-utils compare --before-json-path PATH --after-json-path PATH
hotpath-utils profile-pr --head-metrics PATH --base-metrics PATH
  [--github-token TOKEN] [--pr-number N] [--emoji-threshold N]
  [--benchmark-id ID] [--dry-run]
```

Metrics default to localhost port 6770. The metrics authorization value is exact, not implicitly `Bearer`.

`hotpath-utils compare` covers timing, allocation, threads, and CPU baseline present in both reports. It does not compare futures, locks, channels, I/O, HTTP/server, SQL, or sampled CPU results.

## Live MCP

The server uses stateful Streamable HTTP at `http://127.0.0.1:6771/mcp`, protocol `2024-11-05`. It requires an active guard and `hotpath-mcp`. `HOTPATH_MCP_AUTH_TOKEN` is compared to the exact `Authorization` header value; do not add `Bearer` unless the configured token itself contains it.

Summary/status tools without arguments:

```text
profiler_status
functions_timing
functions_alloc
functions_cpu
functions_cpu_snapshot
channels
streams
futures
rw_locks
mutexes
io
sql
http
server
threads
gauges
dbg_entries
val_entries
tokio_runtime
```

Detail tools and exact schemas:

```text
function_timing_logs  {"function_id": 3}
function_alloc_logs   {"function_id": 3}
channel_logs          {"channel_id": 3}
stream_logs           {"stream_id": 3}
future_logs           {"future_id": 3}
sql_logs              {"sql_id": 3}
http_logs             {"http_id": 3}
server_logs           {"server_id": 3}
gauge_logs            {"gauge_id": 3}
dbg_logs              {"debug_id": 3}
val_logs              {"debug_id": 3}
```

Hotpath 0.24 detail calls accept IDs, not names, and have no per-call `limit`. Retention is controlled globally by `HOTPATH_LOGS_LIMIT`.

Recommended order:

1. `profiler_status`.
2. `functions_timing`, `threads`, `tokio_runtime`.
3. `io`, `channels`, `futures`, `mutexes`, `rw_locks`.
4. `server`, `http`, `gauges`.
5. Allocation only in the alloc lane.
6. `functions_cpu_snapshot`, then poll `functions_cpu` until ready/error.
7. Use returned IDs for detail logs.

## Interpretation

- Function timing is inclusive. Nested parent and child totals overlap.
- Parallel calls contribute overlapping service demand. Divide neither by call count nor worker count unless the exact question and denominator justify it.
- Hotpath 0.24 exposes flat aggregates, not a parent call tree or exclusive wall-time flame graph.
- Allocation is exclusive by default. `HOTPATH_ALLOC_CUMULATIVE=true` includes children and is unsafe to interpret under recursion.
- CPU is exclusive to the innermost matched frame by default; inclusive mode credits every distinct matched frame.
- Async `#[measure]` bridges allocation attribution per poll. A synchronous `measure_block!` spanning `.await` can migrate threads; wall time remains useful but allocation attribution may be unavailable.
- Axum/HTTP client durations end at response headers. Measure streamed body/download/decode separately.
- Direct rusqlite emits no automatic SQL report. Use TraceDecay's writer/reader/transaction/checkpoint spans and truthful work gauges.
- I/O wrapper timing starts on first poll and completes on Ready. Cancellation while Pending is not detected; do not treat it as a full future-lifecycle replacement.
- Dynamic HTTP paths, SQL identifiers/comments, debug values, and per-instance `iter = true` can leak or explode cardinality. Keep production keys static and bounded.

## Feature-off proof

Build/run `production` without any profiling feature while setting report and port variables. Prove:

- no listener on 6770 or 6771;
- no Hotpath report file;
- no profiler background thread;
- identical user-visible results and durable digests;
- feature-on and feature-off latency are reported separately.

Disabled declarative macros preserve their primary expression but discard option/key expressions. Therefore compile both modes: invalid feature-on syntax may compile only while profiling is off.

## TraceDecay-specific workflow

1. Run `scripts/profile-hotpath-os-counters.sh --self-test` before relying on the OS harness.
2. Capture a fresh child process per worker-width benchmark because the indexing Rayon pool is process-wide and initialized once.
3. Keep cold catch-up, warm incremental, no-op, and idle samples separate.
4. For indexing/extraction, use an outer generation/sweep wall span as the latency authority. Inner parse/traverse totals are aggregate worker service demand.
5. Record configured/effective worker count, CPU limit, memory limit/reservation, queue depth, active workers, bytes, swap/faults, and concurrent serving p95/p99.
6. Accept a wider default only with byte-identical sealed output, measured wall improvement, bounded memory/no swap cliff, and preserved foreground responsiveness.

Official references: [functions](https://hotpath.rs/functions), [data flow](https://hotpath.rs/data_flow), [I/O](https://hotpath.rs/io_tracing), [locks](https://hotpath.rs/locks), [HTTP](https://hotpath.rs/http_tracing), [Tokio runtime](https://hotpath.rs/tokio_runtime), [configuration](https://hotpath.rs/configuration), and [overhead](https://hotpath.rs/profiling_overhead).
