#!/usr/bin/env bash
# Measure how a running TraceDecay process spreads work across its Tokio
# async workers, and which labelled futures pin those workers.
#
# Reads only Hotpath's metrics server, so it needs no product code change and
# no restart: build the binary with `hotpath` (see
# .claude/skills/using-hotpath) and point this at the live port.
#
# The two numbers this exists to produce:
#
#   * per-worker busy share -- how lopsided the pool is;
#   * microseconds per poll -- WHY. A healthy async poll is single-digit
#     microseconds. Milliseconds per poll means a task ran synchronous work
#     inside one `poll()`, which no amount of work-stealing can rebalance
#     because the worker is not preemptible until poll returns.
#
# Both are deltas across the sample window, so a long-lived daemon and a fresh
# one are directly comparable. `--futures` attributes the in-poll time to
# labelled futures; `total_poll_duration_ns` excludes every `.await`, so a
# label with a high core count is running synchronous work on the async pool.
#
# Sample a daemon for 60s:
#   scripts/profile-tokio-worker-balance.sh --seconds 60
#
# Gate a before/after comparison (non-zero exit when breached):
#   scripts/profile-tokio-worker-balance.sh --seconds 60 \
#       --max-top2-share 60 --max-us-per-poll 200
#
# Hermetic arithmetic tests (no daemon, no cargo):
#   scripts/profile-tokio-worker-balance.sh --self-test
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -P -- "$(dirname -- "$0")" && pwd)

usage() {
  sed -n '2,30p' "$0" >&2
  exit 2
}

HOST=127.0.0.1
PORT=6770
SECONDS_TO_SAMPLE=60
JSON_OUT=
MAX_TOP2=
MAX_US_PER_POLL=
SELF_TEST=0

while [ $# -gt 0 ]; do
  case "$1" in
    --host) HOST=${2:?--host needs a value}; shift 2 ;;
    --port) PORT=${2:?--port needs a value}; shift 2 ;;
    --seconds) SECONDS_TO_SAMPLE=${2:?--seconds needs a value}; shift 2 ;;
    --json) JSON_OUT=${2:?--json needs a path}; shift 2 ;;
    --max-top2-share) MAX_TOP2=${2:?--max-top2-share needs a value}; shift 2 ;;
    --max-us-per-poll) MAX_US_PER_POLL=${2:?--max-us-per-poll needs a value}; shift 2 ;;
    --self-test) SELF_TEST=1; shift ;;
    -h|--help) usage ;;
    *) echo "unknown argument: $1" >&2; usage ;;
  esac
done

PYTHON=${PYTHON:-python3}

if [ "$SELF_TEST" = 1 ]; then
  exec "$PYTHON" -c "
import sys
sys.path.insert(0, '$SCRIPT_DIR/lib')
import tokio_worker_balance as twb
raise SystemExit(twb.self_test())
"
fi

exec "$PYTHON" - "$HOST" "$PORT" "$SECONDS_TO_SAMPLE" "$JSON_OUT" \
  "$MAX_TOP2" "$MAX_US_PER_POLL" <<PY
import json
import sys
import time

sys.path.insert(0, "$SCRIPT_DIR/lib")
import tokio_worker_balance as twb

host, port, seconds, json_out, max_top2, max_us = sys.argv[1:7]
port = int(port)
seconds = float(seconds)

runtime_before = twb.fetch_json(host, port, "/tokio_runtime")
if runtime_before is None:
    print(
        f"no Hotpath tokio_runtime metrics at {host}:{port}. Build with the "
        "'hotpath' feature, register the runtime with hotpath::tokio_runtime!, "
        "and set HOTPATH_METRICS_PORT.",
        file=sys.stderr,
    )
    raise SystemExit(3)
futures_before = twb.fetch_json(host, port, "/futures")

started = time.monotonic()
time.sleep(seconds)
window = time.monotonic() - started

runtime_after = twb.fetch_json(host, port, "/tokio_runtime")
futures_after = twb.fetch_json(host, port, "/futures")

report = twb.build_report(
    window, runtime_before, runtime_after, futures_before, futures_after
)
print(twb.render_report(report))

if json_out:
    with open(json_out, "w") as handle:
        json.dump(
            {
                "window_secs": report.window_secs,
                "num_workers": report.num_workers,
                "blocking_threads": report.blocking_threads,
                "idle_blocking_threads": report.idle_blocking_threads,
                "busy_cores": report.busy_cores,
                "top1_share": report.top_share(1),
                "top2_share": report.top_share(2),
                "active_workers": report.active_workers,
                "total_steals": report.total_steals,
                "worst_us_per_poll": report.worst_us_per_poll(),
                "workers": [
                    {
                        "index": w.index,
                        "busy_ms": w.busy_ms,
                        "share_pct": report.busy_share(w),
                        "polls": w.polls,
                        "us_per_poll": w.us_per_poll,
                        "steals": w.steals,
                        "parks": w.parks,
                    }
                    for w in report.workers
                ],
                "futures": [
                    {
                        "label": f.label,
                        "source": f.source,
                        "poll_seconds": f.poll_ns / 1e9,
                        "cores": f.cores(report.window_secs),
                        "polls": f.polls,
                        "us_per_poll": f.us_per_poll,
                    }
                    for f in report.futures
                    if f.poll_ns > 0
                ],
            },
            handle,
            indent=2,
        )
    print(f"\nwrote {json_out}")

failures = twb.check_thresholds(
    report,
    float(max_top2) if max_top2 else None,
    float(max_us) if max_us else None,
)
if failures:
    print("")
    for failure in failures:
        print(f"THRESHOLD BREACHED: {failure}")
    raise SystemExit(1)
PY
