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
  local started_ns="$1"
  local finished_ns
  finished_ns="$(date +%s%N)"
  printf '%s\n' "$(((finished_ns - started_ns) / 1000000))"
}

run_timed() {
  local label="$1"
  local timeout_seconds="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  shift 4
  local started_ns status duration_ms
  started_ns="$(date +%s%N)"
  status=0
  timeout --signal=TERM --kill-after=5s "${timeout_seconds}s" "$@" \
    >"$stdout_path" 2>"$stderr_path" || status=$?
  duration_ms="$(elapsed_ms "$started_ns")"
  cat "$stdout_path"
  if [[ -s "$stderr_path" ]]; then
    cat "$stderr_path" >&2
  fi
  echo "tracedecay_ci_timing phase=$label elapsed_ms=$duration_ms status=$status"
  if ((status != 0)); then
    echo "error: TraceDecay PR dogfood phase '$label' failed" >&2
    return "$status"
  fi
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

  run_timed status 60 "$output_dir/status.json" "$output_dir/status.stderr" \
    "$binary" status --json "$project_root"
  python3 "$OUTPUT_VALIDATOR" --kind status --input "$output_dir/status.json"

  run_timed context 90 "$output_dir/context.json" "$output_dir/context.stderr" \
    "$binary" tool context --project "$project_root" \
      --task "Locate the TraceDecay CLI entry point and report available evidence." \
      --format json
  python3 "$OUTPUT_VALIDATOR" --kind context --input "$output_dir/context.json"

  run_timed pr_context 90 "$output_dir/pr-context.json" "$output_dir/pr-context.stderr" \
    "$binary" tool pr_context --project "$project_root" \
      --base-ref "$base_ref" --head-ref "$head_ref" --format json
  python3 "$OUTPUT_VALIDATOR" --kind pr_context \
    --input "$output_dir/pr-context.json" \
    --base-oid "$base_oid" --head-oid "$head_oid" --merge-base "$merge_base"

  run_timed runtime_status 60 "$output_dir/runtime-status.json" \
    "$output_dir/runtime-status.stderr" \
    "$binary" status --json --runtime "$project_root"
  python3 "$OUTPUT_VALIDATOR" --kind status --input "$output_dir/runtime-status.json"

  echo "tracedecay_ci_dogfood outcome=complete"
}

main() {
  local binary project_root base_ref head_ref checked_out_oid head_oid started_ns status
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
  git -C "$project_root" rev-parse --verify "$base_ref^{commit}" >/dev/null
  git -C "$project_root" rev-parse --verify "$head_ref^{commit}" >/dev/null
  checked_out_oid="$(git -C "$project_root" rev-parse HEAD)"
  head_oid="$(git -C "$project_root" rev-parse "$head_ref^{commit}")"
  if [[ "$checked_out_oid" != "$head_oid" ]]; then
    echo "error: dogfood checkout $checked_out_oid does not match PR head $head_oid" >&2
    return 2
  fi

  binary="$(readlink -f "$binary")"
  project_root="$(cd "$project_root" && pwd)"
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-pr-dogfood.XXXXXX")"
  mkdir -p "$WORK_DIR/home" "$WORK_DIR/output"
  trap cleanup EXIT

  started_ns="$(date +%s%N)"
  status=0
  HOME="$WORK_DIR/home" \
    XDG_DATA_HOME="$WORK_DIR/home/.local/share" \
    XDG_CONFIG_HOME="$WORK_DIR/home/.config" \
    TRACEDECAY_BIN="$binary" \
    "$DAEMON_HARNESS" --bin "$binary" --ready-timeout 60 \
      --lifecycle-label "TraceDecay PR dogfood daemon" -- \
      "$SCRIPT_PATH" --run "$project_root" "$base_ref" "$head_ref" \
      "$WORK_DIR/output" || status=$?
  echo "tracedecay_ci_timing phase=total_journey elapsed_ms=$(elapsed_ms "$started_ns") status=$status"
  return "$status"
}

if [[ "${1:-}" == "--run" ]]; then
  shift
  run_smoke "$@"
else
  main "$@"
fi
