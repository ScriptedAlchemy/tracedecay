#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repo_root"

git_root=$(git rev-parse --show-toplevel)
if [[ $(cd "$git_root" && pwd -P) != "$repo_root" ]]; then
  echo "benchmark runner must execute from the current Git worktree" >&2
  exit 1
fi
if [[ -n $(git status --porcelain=v1 --untracked-files=normal --ignore-submodules=none) ]]; then
  echo "benchmark runner requires a clean worktree" >&2
  exit 1
fi

commit=$(git rev-parse HEAD)
short_commit=${commit:0:8}
result_name="result-$(date -u +%F)-${short_commit}.json"
result_path="benchmark_data/claude-observation/$result_name"
index_path="benchmark_data/claude-observation/evidence-index.json"
if [[ -e $result_path ]]; then
  echo "refusing to overwrite $result_path" >&2
  exit 1
fi
if ! grep -q '"current_acceptance": null' "$index_path"; then
  echo "evidence index already names a current acceptance artifact" >&2
  exit 1
fi

staging=$(mktemp -d "${TMPDIR:-/tmp}/claude-observation-bench.XXXXXXXX")
cleanup() {
  rm -rf "$staging"
}
trap cleanup EXIT
capture="$staging/capture.log"

bazel test --config=release --config=ci //crates/tracedecay:claude_observation_benchmark \
  --test_strategy=standalone --nocache_test_results --test_output=all \
  --test_env=TRACEDECAY_BENCHMARK_REPO_ROOT="$repo_root" \
  --test_env=TRACEDECAY_BENCHMARK_BAZEL_VERSION="$(bazel --version)" \
  --test_arg=production_observation_pipeline_baseline \
  --test_arg=--ignored --test_arg=--exact --test_arg=--quiet --test_arg=--nocapture --test_arg=--test-threads=1 \
  2>&1 | tee "$capture"

if [[ $(grep -c '^TRACEDECAY_CLAUDE_OBSERVATION_BENCHMARK_RESULT=' "$capture") -ne 1 ]]; then
  echo "benchmark did not emit exactly one result" >&2
  exit 1
fi
result_json=$(sed -n 's/^TRACEDECAY_CLAUDE_OBSERVATION_BENCHMARK_RESULT=\(.*\) $/\1/p' "$capture")
if [[ -z $result_json ]]; then
  echo "benchmark result marker was malformed" >&2
  exit 1
fi
# Stage only the new output. Historical evidence is read through symlinks,
# without creating a second copy of any existing artifact.
for historical in "$repo_root"/benchmark_data/claude-observation/result-*.json; do
  ln -s "$historical" "$staging/$(basename "$historical")"
done
printf '%s\n' "$result_json" >"$staging/$result_name"
sed "s/\"current_acceptance\": null/\"current_acceptance\": \"$result_name\"/" \
  "$index_path" >"$staging/evidence-index.json"
scripts/require-exact-test.sh bazel test --config=release --config=ci //crates/tracedecay:claude_observation_benchmark \
  --test_strategy=standalone --nocache_test_results --test_output=all \
  --test_env=TRACEDECAY_BENCHMARK_REPO_ROOT="$repo_root" \
  --test_env=TRACEDECAY_BENCHMARK_REQUIRE_ACCEPTANCE=1 \
  --test_env=TRACEDECAY_BENCHMARK_EVIDENCE_DIR="$staging" \
  --test_arg=evidence_directory_matches_index_contract --test_arg=--exact \
  --test_arg=--test-threads=1

mv "$staging/$result_name" "$result_path"
mv "$staging/evidence-index.json" "$index_path"
echo "validated $result_path"
