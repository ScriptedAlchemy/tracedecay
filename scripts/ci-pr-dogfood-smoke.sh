#!/usr/bin/env bash
# Exercise the checked-out pull request through the shipped CLI against an
# isolated daemon/profile. This intentionally indexes the real checkout rather
# than a tiny fixture so CI records first-touch latency and cold-generation
# behavior that unit tests cannot reproduce.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_PATH="$REPO_ROOT/scripts/ci-pr-dogfood-smoke.sh"
DAEMON_HARNESS="$REPO_ROOT/scripts/with-isolated-tracedecay-daemon.sh"
OUTPUT_VALIDATOR="$REPO_ROOT/scripts/check-pr-dogfood-output.py"
PROCESS_HELPER="$REPO_ROOT/scripts/lib/portable_process.py"
WORK_DIR=""

cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
    rm -rf "$WORK_DIR"
  fi
  exit "$status"
}

elapsed_ms() {
  local started_ms="$1"
  local finished_ms
  finished_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
  printf '%s\n' "$((finished_ms - started_ms))"
}

print_compact_file() {
  local label="$1"
  local path="$2"
  echo "----- $label (last 16 KiB) -----" >&2
  if [[ -s "$path" ]]; then
    tail -c 16384 "$path" >&2 || true
  else
    echo "(no output captured)" >&2
  fi
}

# One line of code-index attribution from a status payload, for the attempts
# log. Strict readiness is several distinct phases -- source capture and seal
# (no progress phase yet), the bounded text projection, its finalization, then
# the optional native graph seat -- and a timeout must name the one it died
# in. So besides the phase and its counters this records the identities that
# tell them apart: the sealed source digest and generation the text projection
# is building, the coverage/staleness pair that gates `status=current`, the
# graph seat state with its typed reason, and the graph-statistics state the
# strict validator requires. Text readiness and graph seating stay separate
# columns on purpose: a complete text projection waiting on the graph must
# never read as a stalled text build (issue #917).
summarize_status_progress() {
  local path="$1"
  [[ -s "$path" ]] || {
    echo "progress=unavailable"
    return 0
  }
  python3 -S - "$path" <<'PY' 2>/dev/null || echo "progress=unparsed"
import json, sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        payload = json.load(handle)
except (OSError, ValueError):
    print("progress=unparsed")
    raise SystemExit(0)


def tail(value, width=12):
    if not isinstance(value, str) or not value:
        return None
    return value[-width:]


freshness = payload.get("code_index_freshness") or {}
worktree = freshness.get("worktree") or {}
progress = worktree.get("progress") or {}
serving = worktree.get("code_graph_serving") or {}
statistics = payload.get("graph_statistics") or {}
graph = serving.get("state")
if serving.get("reason"):
    graph = "%s:%s" % (graph, str(serving["reason"]).replace(" ", "_")[:48])
elapsed_micros = progress.get("elapsed_micros")
commit_micros = progress.get("last_commit_latency_micros")
print(
    "status=%s coverage=%s staleness=%s rebuild=%s gen=%s digest=%s graph=%s "
    "gstats=%s phase=%s files=%s/%s pages=%s payload_mb=%s elapsed_s=%s "
    "commit_ms=%s blocked=%s"
    % (
        freshness.get("status"),
        worktree.get("coverage"),
        worktree.get("staleness_state"),
        worktree.get("rebuild_in_flight"),
        tail(worktree.get("latest_generation_id")),
        tail(progress.get("sealed_source_digest")),
        graph,
        statistics.get("state"),
        progress.get("phase"),
        progress.get("completed_files"),
        progress.get("total_files"),
        progress.get("committed_pages"),
        None
        if progress.get("committed_payload_bytes") is None
        else int(progress["committed_payload_bytes"]) // 1_000_000,
        None if elapsed_micros is None else int(elapsed_micros) // 1_000_000,
        None if commit_micros is None else int(commit_micros) // 1_000,
        progress.get("blocked_reason"),
    )
)
PY
}

# The daemon's resident memory at this probe, in MiB, plus the peak the kernel
# reports where it has one. The harness exports TRACEDECAY_DAEMON_PID; without
# it (or once the daemon is gone) the sample says so instead of failing the
# probe. Memory is part of the readiness evidence: on a 16 GiB runner the
# native graph seat, not the text projection, is what can exhaust the host.
summarize_daemon_memory() {
  local pid="${TRACEDECAY_DAEMON_PID:-}"
  [[ -n "$pid" ]] || {
    echo "daemon_rss_mb=unavailable daemon_peak_rss_mb=unavailable"
    return 0
  }
  python3 -S - "$PROCESS_HELPER" "$pid" <<'PY' 2>/dev/null || echo "daemon_rss_mb=unavailable daemon_peak_rss_mb=unavailable"
import subprocess, sys

completed = subprocess.run(
    [sys.executable, "-S", sys.argv[1], "resident-memory", "--pid", sys.argv[2]],
    check=False,
    capture_output=True,
    text=True,
    timeout=10,
)
values = dict(
    field.split("=", 1) for field in completed.stdout.split() if "=" in field
)


def mib(key):
    raw = values.get(key)
    return str(int(raw) // 1024) if raw and raw.isdigit() else "unavailable"


print("daemon_rss_mb=%s daemon_peak_rss_mb=%s" % (mib("rss_kib"), mib("peak_rss_kib")))
PY
}

# Attribute the readiness journey from the attempts log: when progress first
# became visible, when the text projection reached its complete state, when
# the graph seat became ready, the peak daemon memory observed, and the last
# phase/graph state. Printed on success and on timeout alike, so a PASS
# carries its phase timings and peak memory rather than only a verdict, and a
# timeout says whether it died sealing, projecting text, or seating the graph.
report_readiness_phases() {
  local attempts_path="$1"
  local outcome="$2"
  [[ -s "$attempts_path" ]] || {
    echo "tracedecay_ci_readiness_phases outcome=$outcome attempts=0"
    return 0
  }
  python3 -S - "$attempts_path" "$outcome" <<'PY' 2>/dev/null || echo "tracedecay_ci_readiness_phases outcome=$outcome parse=failed"
import sys

first = {}
peak_rss = None
last = {}
attempts = 0
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        fields = dict(
            field.split("=", 1) for field in line.split() if "=" in field
        )
        if "attempt" not in fields:
            continue
        attempts += 1
        elapsed = fields.get("elapsed_ms")
        last = fields
        phase = fields.get("phase")
        graph = fields.get("graph", "")
        if phase not in (None, "None") and "first_progress_ms" not in first:
            first["first_progress_ms"] = elapsed
        if fields.get("gen") not in (None, "None") and "first_generation_ms" not in first:
            first["first_generation_ms"] = elapsed
        if (
            phase == "ready" or fields.get("coverage") == "complete"
        ) and "text_complete_ms" not in first:
            first["text_complete_ms"] = elapsed
        if graph.startswith("ready") and "graph_ready_ms" not in first:
            first["graph_ready_ms"] = elapsed
        if fields.get("status") == "current" and "status_current_ms" not in first:
            first["status_current_ms"] = elapsed
        for key in ("daemon_peak_rss_mb", "daemon_rss_mb"):
            raw = fields.get(key)
            if raw and raw.isdigit():
                peak_rss = max(peak_rss or 0, int(raw))
                break
print(
    "tracedecay_ci_readiness_phases outcome=%s attempts=%d first_progress_ms=%s "
    "first_generation_ms=%s text_complete_ms=%s graph_ready_ms=%s status_current_ms=%s "
    "peak_daemon_rss_mb=%s last_phase=%s last_files=%s last_graph=%s last_coverage=%s"
    % (
        sys.argv[2],
        attempts,
        first.get("first_progress_ms"),
        first.get("first_generation_ms"),
        first.get("text_complete_ms"),
        first.get("graph_ready_ms"),
        first.get("status_current_ms"),
        "unavailable" if peak_rss is None else peak_rss,
        last.get("phase"),
        last.get("files"),
        last.get("graph"),
        last.get("coverage"),
    )
)
PY
}

run_timed() {
  local label="$1"
  local timeout_seconds="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  shift 4
  local started_ms status duration_ms
  started_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
  status=0
  python3 -S "$PROCESS_HELPER" run \
    --timeout "$timeout_seconds" --kill-after 5 -- "$@" \
    >"$stdout_path" 2>"$stderr_path" || status=$?
  duration_ms="$(elapsed_ms "$started_ms")"
  echo "tracedecay_ci_timing phase=$label elapsed_ms=$duration_ms status=$status"
  if ((status != 0)); then
    echo "error: TraceDecay PR dogfood phase '$label' failed" >&2
    print_compact_file "$label stdout" "$stdout_path"
    print_compact_file "$label stderr" "$stderr_path"
    return "$status"
  fi
  cat "$stdout_path"
  if [[ -s "$stderr_path" ]]; then
    cat "$stderr_path" >&2
  fi
}

run_validation() {
  local label="$1"
  local input_path="$2"
  shift 2
  local stdout_path="${input_path}.validation.stdout"
  local stderr_path="${input_path}.validation.stderr"
  local status=0
  python3 -S "$OUTPUT_VALIDATOR" "$@" --input "$input_path" \
    >"$stdout_path" 2>"$stderr_path" || status=$?
  if ((status != 0)); then
    echo "error: TraceDecay PR dogfood '$label' validation failed" >&2
    print_compact_file "$label output" "$input_path"
    print_compact_file "$label validator" "$stderr_path"
    return "$status"
  fi
  cat "$stdout_path"
  if [[ -s "$stderr_path" ]]; then
    cat "$stderr_path" >&2
  fi
}

wait_for_strict_readiness() {
  local project_root="$1"
  local output_dir="$2"
  local binary="$3"
  local timeout_seconds="${TRACEDECAY_DOGFOOD_READINESS_TIMEOUT:-600}"
  local poll_interval="${TRACEDECAY_DOGFOOD_READINESS_POLL_INTERVAL:-5}"
  local timeout_ms started_ms deadline_ms now_ms remaining_ms probe_timeout
  local attempts=0 command_status validation_status duration_ms

  timeout_ms="$(python3 -S -c '
import math, sys
try:
    value = float(sys.argv[1])
except ValueError:
    raise SystemExit(1)
if not math.isfinite(value) or value <= 0:
    raise SystemExit(1)
print(max(1, int(value * 1000)))
' "$timeout_seconds")" || {
    echo "error: TRACEDECAY_DOGFOOD_READINESS_TIMEOUT must be greater than zero" >&2
    return 2
  }
  python3 -S -c '
import math, sys
try:
    value = float(sys.argv[1])
except ValueError:
    raise SystemExit(1)
raise SystemExit(0 if math.isfinite(value) and value > 0 else 1)
' "$poll_interval" || {
    echo "error: TRACEDECAY_DOGFOOD_READINESS_POLL_INTERVAL must be greater than zero" >&2
    return 2
  }

  started_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
  deadline_ms="$((started_ms + timeout_ms))"
  : >"$output_dir/status.validation.stderr"
  while :; do
    now_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
    remaining_ms="$((deadline_ms - now_ms))"
    ((remaining_ms > 0)) || break
    probe_timeout="$(python3 -S -c 'import sys; print(min(60.0, max(0.05, int(sys.argv[1]) / 1000)))' "$remaining_ms")"
    attempts="$((attempts + 1))"
    : >"$output_dir/status.validation.stdout"
    : >"$output_dir/status.validation.stderr"
    command_status=0
    probe_started_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
    # Probe into its own files: a probe the deadline kills must not erase
    # the last complete payload, which is the evidence a timeout report needs.
    python3 -S "$PROCESS_HELPER" run \
      --timeout "$probe_timeout" --kill-after 5 -- \
      "$binary" status --json "$project_root" \
      >"$output_dir/status.probe.json" 2>"$output_dir/status.probe.stderr" || command_status=$?
    probe_ms="$(elapsed_ms "$probe_started_ms")"
    validation_status=1
    if ((command_status == 0)) && [[ -s "$output_dir/status.probe.json" ]]; then
      # Validate the complete JSON document before replacing the last known
      # good payload. Strict readiness remains a separate check so a valid
      # status that is still warming up is retained as useful evidence.
      validation_status=0
      python3 -S "$OUTPUT_VALIDATOR" --kind status \
        --input "$output_dir/status.probe.json" \
        >"$output_dir/status.validation.stdout" \
        2>"$output_dir/status.validation.stderr" || validation_status=$?
      if ((validation_status == 0)); then
        mv -f "$output_dir/status.probe.json" "$output_dir/status.json"
        mv -f "$output_dir/status.probe.stderr" "$output_dir/status.stderr"
        validation_status=0
        python3 -S "$OUTPUT_VALIDATOR" --kind status --strict \
          --input "$output_dir/status.json" \
          >"$output_dir/status.validation.stdout" \
          2>"$output_dir/status.validation.stderr" || validation_status=$?
      fi
    fi
    printf 'attempt=%s elapsed_ms=%s probe_ms=%s status_rc=%s validation_rc=%s %s %s\n' \
      "$attempts" "$(elapsed_ms "$started_ms")" "$probe_ms" "$command_status" \
      "$validation_status" "$(summarize_status_progress "$output_dir/status.json")" \
      "$(summarize_daemon_memory)" \
      >>"$output_dir/status.attempts.log"
    if ((command_status == 0 && validation_status == 0)); then
      duration_ms="$(elapsed_ms "$started_ms")"
      cat "$output_dir/status.json"
      [[ ! -s "$output_dir/status.stderr" ]] || cat "$output_dir/status.stderr" >&2
      cat "$output_dir/status.validation.stdout"
      echo "tracedecay_ci_timing phase=status elapsed_ms=$duration_ms status=0"
      echo "tracedecay_ci_readiness attempts=$attempts elapsed_ms=$duration_ms"
      report_readiness_phases "$output_dir/status.attempts.log" ready
      return 0
    fi

    now_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
    remaining_ms="$((deadline_ms - now_ms))"
    ((remaining_ms > 0)) || break
    python3 -S -c 'import sys, time; time.sleep(min(float(sys.argv[1]), int(sys.argv[2]) / 1000))' \
      "$poll_interval" "$remaining_ms"
  done

  duration_ms="$(elapsed_ms "$started_ms")"
  echo "tracedecay_ci_timing phase=status elapsed_ms=$duration_ms status=1"
  report_readiness_phases "$output_dir/status.attempts.log" timeout
  echo "error: TraceDecay PR dogfood did not reach strict index readiness within ${timeout_seconds}s" >&2
  print_compact_file "status readiness attempts" "$output_dir/status.attempts.log"
  print_compact_file "last complete status output" "$output_dir/status.json"
  print_compact_file "last complete status stderr" "$output_dir/status.stderr"
  print_compact_file "last status validator" "$output_dir/status.validation.stderr"
  print_compact_file "last status probe stderr" "$output_dir/status.probe.stderr"
  return 1
}

run_smoke() {
  local project_root="$1"
  local base_ref="$2"
  local head_ref="$3"
  local output_dir="$4"
  local binary="$TRACEDECAY_BIN"
  local base_oid head_oid merge_base

  base_oid="$(git -C "$project_root" rev-parse "$base_ref^{commit}")"
  head_oid="$(git -C "$project_root" rev-parse "$head_ref^{commit}")"
  merge_base="$(git -C "$project_root" merge-base "$base_ref" "$head_ref")"

  echo "tracedecay_ci_checkout head=$(git -C "$project_root" rev-parse HEAD) base=$base_ref"
  echo "tracedecay_ci_binary path=$binary version=$($binary --version)"

  (
    cd "$project_root"
    run_timed init 180 "$output_dir/init.stdout" "$output_dir/init.stderr" \
      "$binary" init
  )

  wait_for_strict_readiness "$project_root" "$output_dir" "$binary"

  run_timed context 90 "$output_dir/context.json" "$output_dir/context.stderr" \
    "$binary" tool context --project "$project_root" \
      --task "Locate the TraceDecay CLI entry point and report available evidence." \
      --format json
  run_validation context "$output_dir/context.json" --kind context --strict

  run_timed pr_context 90 "$output_dir/pr-context.json" "$output_dir/pr-context.stderr" \
    "$binary" tool pr_context --project "$project_root" \
      --base-ref "$base_ref" --head-ref "$head_ref" --format json
  run_validation pr_context "$output_dir/pr-context.json" --kind pr_context --strict \
    --base-oid "$base_oid" --head-oid "$head_oid" --merge-base "$merge_base"

  run_timed runtime_status 60 "$output_dir/runtime-status.json" \
    "$output_dir/runtime-status.stderr" \
    "$binary" status --json --runtime "$project_root"
  run_validation runtime_status "$output_dir/runtime-status.json" --kind status

  echo "tracedecay_ci_dogfood outcome=complete"
}

main() {
  local binary project_root base_ref head_ref checked_out_oid head_oid started_ms status
  binary="${TRACEDECAY_BIN:-$REPO_ROOT/target/debug/tracedecay}"
  project_root="${TRACEDECAY_DOGFOOD_PROJECT:-$REPO_ROOT}"
  base_ref="${TRACEDECAY_DOGFOOD_BASE_REF:-}"
  head_ref="${TRACEDECAY_DOGFOOD_HEAD_REF:-HEAD}"

  [[ -x "$binary" ]] || {
    echo "error: TraceDecay binary is not executable: $binary" >&2
    return 2
  }
  [[ -d "$project_root/.git" || -f "$project_root/.git" ]] || {
    echo "error: dogfood project is not a Git checkout: $project_root" >&2
    return 2
  }
  [[ -n "$base_ref" ]] || {
    echo "error: TRACEDECAY_DOGFOOD_BASE_REF is required" >&2
    return 2
  }
  command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 is required for portable process control" >&2
    return 2
  }
  [[ -f "$PROCESS_HELPER" ]] || {
    echo "error: portable process helper is missing: $PROCESS_HELPER" >&2
    return 2
  }
  git -C "$project_root" rev-parse --verify "$base_ref^{commit}" >/dev/null
  git -C "$project_root" rev-parse --verify "$head_ref^{commit}" >/dev/null
  checked_out_oid="$(git -C "$project_root" rev-parse HEAD)"
  head_oid="$(git -C "$project_root" rev-parse "$head_ref^{commit}")"
  if [[ "$checked_out_oid" != "$head_oid" ]]; then
    echo "error: dogfood checkout $checked_out_oid does not match PR head $head_oid" >&2
    return 2
  fi

  binary="$(python3 -S "$PROCESS_HELPER" realpath "$binary")"
  project_root="$(cd "$project_root" && pwd)"
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-pr-dogfood.XXXXXX")"
  mkdir -p "$WORK_DIR/home" "$WORK_DIR/output"
  trap cleanup EXIT

  started_ms="$(python3 -S "$PROCESS_HELPER" monotonic-ms)"
  status=0
  HOME="$WORK_DIR/home" \
    XDG_DATA_HOME="$WORK_DIR/home/.local/share" \
    XDG_CONFIG_HOME="$WORK_DIR/home/.config" \
    TRACEDECAY_BIN="$binary" \
    "$DAEMON_HARNESS" --bin "$binary" --ready-timeout 60 \
      --lifecycle-label "TraceDecay PR dogfood daemon" -- \
      "$SCRIPT_PATH" --run "$project_root" "$base_ref" "$head_ref" \
      "$WORK_DIR/output" || status=$?
  echo "tracedecay_ci_timing phase=total_journey elapsed_ms=$(elapsed_ms "$started_ms") status=$status"
  return "$status"
}

if [[ "${1:-}" == "--run" ]]; then
  shift
  run_smoke "$@"
else
  main "$@"
fi
