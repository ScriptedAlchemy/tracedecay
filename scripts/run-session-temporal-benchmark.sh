#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/run-session-temporal-benchmark.sh --dry-run|--run|--refresh-contract

  --dry-run  Read-only, Cargo-free validation of harness artifacts and
             Codex fixture provenance. Does not mutate the checkout.
  --run      Diagnostic measurement through the optimized bench profile on
             Linux and macOS.
             Isolates HOME and TRACEDECAY_DATA_DIR for the child process.
             Windows CI proves temporal durability via nextest.
  --refresh-contract
             Run the same real measurement from a clean source commit, then
             publish the result with its clean-commit provenance (Linux-hosted).
             The workload manifest is static configuration and is not rewritten.
EOF
}

find_python() {
  local candidate
  for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  printf '%s\n' "Session-temporal validation requires Python 3" >&2
  return 1
}

validate_harness_evidence() {
  local python_bin
  python_bin="$(find_python)"
  "$python_bin" - "$repo_root" <<'PY'
import json
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
benchmark_root = root / "benchmark_data/session-temporal"
phases = [
    "rebuild_activate",
    "exact_replay",
    "compact_rank",
    "late_hydrate",
]
p95_label = "descriptive nearest-rank sample p95"
p99_label = "descriptive nearest-rank sample p99 (sample maximum when n=30)"
receipt_path = "benchmark_data/session-temporal/fixtures/codex-sanitization-receipt.json"

def load(path):
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)

def require(condition, message):
    if not condition:
        raise SystemExit(f"Session-temporal dry-run failed: {message}")

workload_path = benchmark_root / "workload-v1.json"
workload = load(workload_path)
index = load(benchmark_root / "evidence-index.json")
historical_result_path = benchmark_root / "result-provisional.json"
current_result_path = benchmark_root / "result-current.json"
historical_result = load(historical_result_path)
receipt = load(root / receipt_path)

# Content identity is the git commit; the dry run validates artifact shape and
# state, not file hashes.
require(workload.get("schema_version") == 2, "unexpected workload schema")
require(workload.get("workload_id") == "session-temporal-v1", "workload id mismatch")
require(workload.get("status") == "harness_ready", "workload must be harness_ready")
fixture = workload.get("fixture_evidence", {})
require(fixture.get("independently_sourced") is True, "fixture must claim independent provenance")
require(fixture.get("sanitization_receipt") == receipt_path, "sanitization receipt path mismatch")
require(receipt.get("independently_sourced") is True, "receipt must be independently sourced")
require(receipt.get("provider") == "codex", "receipt provider must be codex")
for entry in receipt.get("files", []):
    require((root / entry["path"]).is_file(), f"missing receipt file: {entry['path']}")

contract = workload.get("measurement_contract") or {}
actual_phases = [item.get("phase") for item in contract.get("phases", [])]
require(actual_phases == phases, f"dry-run phases mismatch: {actual_phases}")
print("phases:", ", ".join(phases))

stats = workload.get("statistics") or {}
require(stats.get("p95_label") == p95_label, "p95 label mismatch")
require(stats.get("p99_label") == p99_label, "p99 label mismatch")
require(workload.get("production_path", {}).get("available_to_benchmark_target") is True,
        "production path must be available")
implementation = workload.get("implementation") or {}
require(implementation.get("path") == "crates/tracedecay/src/session_temporal_benchmark.rs",
        "implementation path mismatch")
require((root / implementation["path"]).is_file(), "implementation source missing")
runner = workload.get("runner") or {}
require(runner.get("path") == "scripts/run-session-temporal-benchmark.sh",
        "runner path mismatch")
require((root / runner["path"]).is_file(), "runner script missing")

require(index.get("schema_version") == 2, "unexpected evidence index schema")
require(index.get("current_acceptance") is None, "current acceptance must remain null")
require(index.get("blocked") is None, "blocked result pointer must remain null")
require(index.get("historical_stale") == ["result-provisional.json"],
        "historical stale result index mismatch")
require(historical_result.get("schema_version") == 2, "unexpected historical result schema")
require(historical_result.get("workload_id") == workload["workload_id"],
        "historical result workload mismatch")
require(historical_result.get("capture_status") == "provisional",
        "historical capture must retain its original status")
require(historical_result.get("evidence_status") == "historical_stale",
        "historical result must be marked stale")
require(historical_result.get("acceptance_eligible") is False,
        "historical result must be ineligible")
require(historical_result.get("source_identity", {}).get("harness")
        == "src/sessions/session_temporal_benchmark.rs",
        "historical result harness identity mismatch")
require(bool(historical_result.get("stale_reason")),
        "historical result must explain why it is stale")

provisional = index.get("provisional")
if provisional is None:
    require(not current_result_path.exists(),
            "current measurement exists without an evidence-index pointer")
elif provisional == "result-current.json":
    result = load(current_result_path)
    require(result.get("schema_version") == 2, "unexpected current result schema")
    require(result.get("workload_id") == workload["workload_id"], "current result workload mismatch")
    require(result.get("capture_status") == "provisional", "current result must be provisional")
    require(result.get("acceptance_eligible") is False, "current result must be ineligible")
    require(result.get("workload_manifest") == "benchmark_data/session-temporal/workload-v1.json",
            "current result workload manifest mismatch")
    identity = result.get("source_identity", {})
    require(identity.get("harness")
            == "crates/tracedecay/src/session_temporal_benchmark.rs",
            "current result harness identity mismatch")
    require(identity.get("runner") == "scripts/run-session-temporal-benchmark.sh",
            "current result runner identity mismatch")
    require(identity.get("source_mode") == "clean_git_worktree_v1",
            "current result must record clean-source provenance")
    commit = identity.get("commit")
    require(isinstance(commit, str) and len(commit) == 40
            and all(char in "0123456789abcdef" for char in commit),
            "current result must record a full source commit")
    measurement = result.get("measurement", {})
    require(measurement.get("warmup_repetitions") == 3,
            "current result warmup repetitions mismatch")
    require(measurement.get("measured_repetitions") == 30,
            "current result measured repetitions mismatch")
    records_per_repetition = measurement.get("records_per_repetition")
    require(isinstance(records_per_repetition, int) and records_per_repetition > 0,
            "current result records_per_repetition must be positive")
    require(measurement.get("measured_record_count") == records_per_repetition * 30,
            "current result measured record count mismatch")
else:
    require(False, f"unexpected provisional result pointer: {provisional}")

require("source_attestation" not in historical_result,
        "deleted source_attestation field is forbidden")
require(isinstance(historical_result.get("source_identity"), dict),
        "historical source identity is required")
require("attestation" not in json.dumps(historical_result).lower(),
        "deleted attestation terminology remains in historical result")
require("attestation" not in json.dumps(workload).lower(),
        "deleted attestation terminology remains in workload")

with (root / "Cargo.toml").open("rb") as handle:
    cargo = tomllib.load(handle)
profile = cargo.get("profile", {}).get("bench", {})
require(profile == {
    "opt-level": 3,
    "debug": False,
    "debug-assertions": False,
    "overflow-checks": False,
    "incremental": False,
}, "optimized bench profile mismatch")

storage = workload.get("storage_isolation") or {}
require("HOME" in storage.get("required_environment", []), "HOME isolation required")
require("TRACEDECAY_DATA_DIR" in storage.get("required_environment", []),
        "TRACEDECAY_DATA_DIR isolation required")
PY
}

run_benchmark() {
  local mode="$1"
  local host_os
  host_os="$(uname -s)"
  case "$mode:$host_os" in
    --run:Linux|--run:Darwin|--refresh-contract:Linux) ;;
    --refresh-contract:*)
      printf '%s\n' "Session-temporal contract refresh is Linux-hosted" >&2
      exit 64
      ;;
    *)
      printf '%s\n' "Session-temporal ${mode} measurement harness supports Linux and macOS; use CI nextest durable coverage on other platforms" >&2
      exit 64
      ;;
  esac
  isolation_root="$(mktemp -d "${TMPDIR:-/tmp}/session-temporal-bench.XXXXXX")"
  cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
  cleanup() {
    rm -rf "$isolation_root"
  }
  trap cleanup EXIT
  export HOME="$isolation_root/home"
  export TRACEDECAY_DATA_DIR="$isolation_root/tracedecay-data"
  export CARGO_HOME="$cargo_home"
  export RUSTUP_HOME="$rustup_home"
  mkdir -p "$HOME" "$TRACEDECAY_DATA_DIR"
  cargo bench -p tracedecay --bench session_temporal --all-features -- "$mode"
}

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

case "$1" in
  --dry-run)
    validate_harness_evidence
    printf 'OK: session-temporal dry-run validated harness_ready evidence (Cargo-free, no mutation)\n'
    ;;
  --run)
    run_benchmark --run
    ;;
  --refresh-contract)
    run_benchmark --refresh-contract
    ;;
  -h|--help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
