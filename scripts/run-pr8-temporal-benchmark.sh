#!/usr/bin/env bash
set -euo pipefail

readonly blocked_reason="authentic_provider_capture_and_public_production_path_unavailable"

usage() {
  cat <<'EOF'
Usage: scripts/run-pr8-temporal-benchmark.sh --dry-run|--run

  --dry-run  Read-only, Cargo-free validation of the fail-closed evidence.
  --run      Reject measurement. The benchmark remains BLOCKED until authentic
             provider captures and public production adapters are available.
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
  printf '%s\n' "PR8 temporal validation requires Python 3" >&2
  return 1
}

validate_blocked_evidence() {
  local python_bin
  python_bin="$(find_python)"
  "$python_bin" - "$repo_root" "$blocked_reason" <<'PY'
import hashlib
import json
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
blocked_reason = sys.argv[2]
benchmark_root = root / "benchmarks/pr8-temporal"

def load(path):
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)

def require(condition, message):
    if not condition:
        raise SystemExit(f"PR8 temporal dry-run failed: {message}")

def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

workload_path = benchmark_root / "workload-v1.json"
workload = load(workload_path)
index = load(benchmark_root / "evidence-index.json")
result = load(benchmark_root / "result-provisional.json")

require(workload.get("schema_version") == 2, "unexpected workload schema")
require(workload.get("workload_id") == "pr8-session-temporal-v1", "workload id mismatch")
require(workload.get("status") == "blocked", "workload must fail closed")
require(workload.get("blocked_reason") == blocked_reason, "workload reason mismatch")
fixture = workload.get("fixture_evidence", {})
require(fixture.get("independently_sourced") is False,
        "fixture must not claim independent provenance")
require(fixture.get("sanitization_receipt") is None,
        "fixture must not fabricate a sanitization receipt")
require(workload.get("measurement_contract") is None,
        "blocked workload must not define measurements")

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
    "blocked": "result-provisional.json",
    "historical_stale": [],
}, "evidence index must expose only blocked evidence")
require(result.get("schema_version") == 2, "unexpected result schema")
require(result.get("workload_id") == workload["workload_id"], "result workload mismatch")
require(result.get("capture_status") == "blocked", "result must be blocked")
require(result.get("acceptance_eligible") is False, "result must be ineligible")
require(result.get("blocked_reason") == blocked_reason, "result reason mismatch")
require(result.get("measurement") is None, "blocked result must not contain samples")
require(result.get("source_attestation") is None,
        "blocked result must not fabricate source attestation")
require(result.get("workload_manifest_sha256") == sha256(workload_path),
        "result workload hash mismatch")

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
PY
}

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

case "$1" in
  --dry-run)
    validate_blocked_evidence
    printf 'BLOCKED: %s\n' "$blocked_reason"
    ;;
  --run)
    if [[ "$(uname -s)" != "Linux" ]]; then
      printf '%s\n' "PR8 temporal measurements require Linux; unsupported platform rejected" >&2
      exit 64
    fi
    printf 'BLOCKED: %s\n' "$blocked_reason" >&2
    exit 3
    ;;
  -h|--help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
