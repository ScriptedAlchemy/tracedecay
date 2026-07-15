#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repo_root"

git_root=$(git rev-parse --show-toplevel)
if [[ $(cd "$git_root" && pwd -P) != "$repo_root" ]]; then
  echo "benchmark runner must execute from the CARGO_MANIFEST_DIR Git worktree" >&2
  exit 1
fi
if [[ -n $(git status --porcelain=v1 --untracked-files=normal --ignore-submodules=none) ]]; then
  echo "benchmark runner requires a clean worktree" >&2
  exit 1
fi

commit=$(git rev-parse HEAD)
tree=$(git rev-parse 'HEAD^{tree}')
short_commit=${commit:0:8}
result_name="result-$(date -u +%F)-${short_commit}.json"
result_path="benchmarks/pr5-observation/$result_name"
index_path="benchmarks/pr5-observation/evidence-index.json"
if [[ -e $result_path ]]; then
  echo "refusing to overwrite $result_path" >&2
  exit 1
fi
if ! grep -q '"current_acceptance": null' "$index_path"; then
  echo "evidence index already names a current acceptance artifact" >&2
  exit 1
fi

target_root=${TRACEDECAY_BENCHMARK_TARGET_ROOT:-${CARGO_TARGET_DIR:-$repo_root/target/pr5-observation-benchmark}}
export CARGO_TARGET_DIR="${target_root%/}/$commit"
export TRACEDECAY_DATA_DIR="$CARGO_TARGET_DIR/test-profile/.tracedecay"
export TRACEDECAY_BENCHMARK_BUILD_COMMIT=$commit
export TRACEDECAY_BENCHMARK_BUILD_TREE=$tree
export TRACEDECAY_BENCHMARK_BUILD_PROFILE=release
export TRACEDECAY_BENCHMARK_BUILD_TARGET_DIR=$CARGO_TARGET_DIR

capture=$(mktemp)
index_backup=$(mktemp)
cp "$index_path" "$index_backup"
complete=false
cleanup() {
  rm -f "$capture"
  if [[ $complete != true ]]; then
    rm -f "$result_path"
    cp "$index_backup" "$index_path"
  fi
  rm -f "$index_backup"
}
trap cleanup EXIT

cargo test --quiet --locked --release --lib \
  sessions::claude_observation_benchmark::production_observation_pipeline_baseline -- \
  --ignored --exact --nocapture --test-threads=1 2>&1 | tee "$capture"

if [[ $(grep -c '^TRACEDECAY_PR5_BENCHMARK_RESULT=' "$capture") -ne 1 ]]; then
  echo "benchmark did not emit exactly one result" >&2
  exit 1
fi
result_json=$(sed -n 's/^TRACEDECAY_PR5_BENCHMARK_RESULT=\(.*\) $/\1/p' "$capture")
if [[ -z $result_json ]]; then
  echo "benchmark result marker was malformed" >&2
  exit 1
fi
printf '%s\n' "$result_json" >"$result_path"

sed "s/\"current_acceptance\": null/\"current_acceptance\": \"$result_name\"/" \
  "$index_backup" >"$index_path"
TRACEDECAY_BENCHMARK_REQUIRE_ACCEPTANCE=1 \
  cargo test --quiet --locked --release --lib \
  sessions::claude_observation_benchmark::evidence_directory_matches_index_contract -- \
  --exact --test-threads=1

complete=true
echo "validated $result_path"
echo "commit only the result, evidence index, and README summary as the evidence follow-up"
