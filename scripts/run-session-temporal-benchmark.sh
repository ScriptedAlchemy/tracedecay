#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/run-session-temporal-benchmark.sh --dry-run|--run|--refresh-contract

  --dry-run  Read-only, Cargo-free validation of harness artifacts and
             Codex fixture provenance. Does not mutate the checkout.
  --run      Measurement through the optimized bench profile (Linux preferred).
             Isolates HOME and TRACEDECAY_DATA_DIR for the child process.
             Windows/macOS CI prove temporal durability via nextest; this
             measurement entrypoint remains Linux-hosted for bench tooling.
  --refresh-contract
             Run the same real measurement from a clean source commit, then
             regenerate the workload manifest and result together.
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
import hashlib
import json
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
benchmark_root = root / "benchmarks/session-temporal"
phases = [
    "rebuild_activate",
    "exact_replay",
    "compact_rank",
    "late_hydrate",
]
p95_label = "descriptive nearest-rank sample p95"
p99_label = "descriptive nearest-rank sample p99 (sample maximum when n=30)"
receipt_path = "benchmarks/session-temporal/fixtures/codex-sanitization-receipt.json"

def load(path):
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)

def require(condition, message):
    if not condition:
        raise SystemExit(f"Session-temporal dry-run failed: {message}")

def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

workload_path = benchmark_root / "workload-v1.json"
workload = load(workload_path)
index = load(benchmark_root / "evidence-index.json")
result = load(benchmark_root / "result-provisional.json")
receipt = load(root / receipt_path)

require(workload.get("schema_version") == 2, "unexpected workload schema")
require(workload.get("workload_id") == "session-temporal-v1", "workload id mismatch")
require(workload.get("status") == "harness_ready", "workload must be harness_ready")
fixture = workload.get("fixture_evidence", {})
require(fixture.get("independently_sourced") is True, "fixture must claim independent provenance")
require(fixture.get("sanitization_receipt") == receipt_path, "sanitization receipt path mismatch")
require(receipt.get("independently_sourced") is True, "receipt must be independently sourced")
require(receipt.get("provider") == "codex", "receipt provider must be codex")
for entry in receipt.get("files", []):
    path = root / entry["path"]
    require(path.is_file(), f"missing receipt file: {entry['path']}")
    require(sha256(path) == entry["sha256"], f"receipt hash mismatch: {entry['path']}")

contract = workload.get("measurement_contract") or {}
actual_phases = [item.get("phase") for item in contract.get("phases", [])]
require(actual_phases == phases, f"dry-run phases mismatch: {actual_phases}")
print("phases:", ", ".join(phases))

stats = workload.get("statistics") or {}
require(stats.get("p95_label") == p95_label, "p95 label mismatch")
require(stats.get("p99_label") == p99_label, "p99 label mismatch")
require(workload.get("production_path", {}).get("available_to_benchmark_target") is True,
        "production path must be available")

inventory = workload.get("file_inventory")
require(isinstance(inventory, list) and inventory, "file inventory is empty")
seen = set()
for entry in inventory:
    relative = pathlib.PurePosixPath(entry["path"])
    require(not relative.is_absolute() and ".." not in relative.parts,
            f"non-relative inventory path: {relative}")
    require(str(relative) not in seen, f"duplicate inventory path: {relative}")
    seen.add(str(relative))
    path = root / pathlib.Path(*relative.parts)
    require(path.is_file(), f"missing inventory file: {relative}")
    require(sha256(path) == entry["sha256"], f"hash mismatch: {relative}")

require(index == {
    "schema_version": 2,
    "current_acceptance": None,
    "blocked": None,
    "provisional": "result-provisional.json",
    "historical_stale": [],
}, "evidence index must expose provisional evidence only")
require(result.get("schema_version") == 2, "unexpected result schema")
require(result.get("workload_id") == workload["workload_id"], "result workload mismatch")
require(result.get("capture_status") == "provisional", "result must be provisional")
require(result.get("acceptance_eligible") is False, "result must be ineligible")
require(result.get("workload_manifest_sha256") == sha256(workload_path),
        "result workload hash mismatch")
require("source_attestation" not in result, "deleted source_attestation field is forbidden")
require(isinstance(result.get("source_identity"), dict), "source identity is required")
require("attestation" not in json.dumps(result).lower(),
        "deleted attestation terminology remains in result")
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
  if [[ "$(uname -s)" != "Linux" ]]; then
    printf '%s\n' "Session-temporal ${mode} measurement harness is Linux-hosted; use CI nextest durable coverage on Windows/macOS" >&2
    exit 64
  fi
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
  cargo bench --bench session_temporal --all-features -- "$mode"
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
