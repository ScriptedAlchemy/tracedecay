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

# One line of code-index progress from a status payload, for the attempts
# log: which phase the index is in and how far along, so a timeout report
# shows where the journey stalled without rerunning it.
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
freshness = payload.get("code_index_freshness") or {}
worktree = freshness.get("worktree") or {}
progress = worktree.get("progress") or {}
serving = worktree.get("code_graph_serving") or {}
print(
    "status=%s serving=%s phase=%s files=%s/%s pages=%s blocked=%s"
    % (
        freshness.get("status"),
        serving.get("state"),
        progress.get("phase"),
        progress.get("completed_files"),
        progress.get("total_files"),
        progress.get("committed_pages"),
        progress.get("blocked_reason"),
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
    printf 'attempt=%s elapsed_ms=%s probe_ms=%s status_rc=%s validation_rc=%s %s\n' \
      "$attempts" "$(elapsed_ms "$started_ms")" "$probe_ms" "$command_status" \
      "$validation_status" "$(summarize_status_progress "$output_dir/status.json")" \
      >>"$output_dir/status.attempts.log"
    if ((command_status == 0 && validation_status == 0)); then
      duration_ms="$(elapsed_ms "$started_ms")"
      cat "$output_dir/status.json"
      [[ ! -s "$output_dir/status.stderr" ]] || cat "$output_dir/status.stderr" >&2
      cat "$output_dir/status.validation.stdout"
      echo "tracedecay_ci_timing phase=status elapsed_ms=$duration_ms status=0"
      echo "tracedecay_ci_readiness attempts=$attempts elapsed_ms=$duration_ms"
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
