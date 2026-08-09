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
result_path="benchmarks/claude-observation/$result_name"
index_path="benchmarks/claude-observation/evidence-index.json"
if [[ -e $result_path ]]; then
  echo "refusing to overwrite $result_path" >&2
  exit 1
fi
if ! grep -q '"current_acceptance": null' "$index_path"; then
  echo "evidence index already names a current acceptance artifact" >&2
  exit 1
fi

scratch_root="$repo_root/target/pr6-observation-source"
mkdir -p "$scratch_root"
build_root=$(mktemp -d "$scratch_root/archive.XXXXXXXX")
capture=$(mktemp)
index_backup=$(mktemp)
cp "$index_path" "$index_backup"
complete=false
cleanup() {
  cd "$repo_root"
  rm -f "$capture"
  if [[ $complete != true ]]; then
    rm -f "$result_path"
    cp "$index_backup" "$index_path"
  fi
  rm -f "$index_backup"
  if [[ -d ${build_root:-} ]]; then
    chmod -R u+w "$build_root" || true
    rm -rf "$build_root"
  fi
}
trap cleanup EXIT
git archive "$commit" | tar -x -C "$build_root"

write_source_manifest() {
  local destination=$1
  : >"$destination"
  while IFS= read -r -d '' relative; do
    mode=$(git ls-files -s -- "$relative" | awk '{print $1}')
    if [[ $mode == 160000 ]]; then
      digest=$(git rev-parse "$commit:$relative")
    else
      digest=$(sha256sum "$build_root/$relative" | awk '{print $1}')
    fi
    printf '%s\t%s\t%s\n' "$mode" "$digest" "$relative" >>"$destination"
  done < <(git ls-files -z)
}

source_manifest="$build_root/.tracedecay-benchmark-source-manifest"
write_source_manifest "$source_manifest"
source_manifest_sha256=$(sha256sum "$source_manifest" | awk '{print $1}')

host_target=$(rustc -Vv | sed -n 's/^host: //p')
[[ -n $host_target ]] || { echo "could not resolve rustc host target" >&2; exit 1; }
export CARGO_BUILD_TARGET=$host_target
unset RUSTFLAGS
export CARGO_ENCODED_RUSTFLAGS=

config_identity=$({
  for config in "$build_root/.cargo/config.toml" \
    "${CARGO_HOME:-${HOME:-}/.cargo}/config" \
    "${CARGO_HOME:-${HOME:-}/.cargo}/config.toml"; do
    if [[ -f $config ]]; then
      printf '%s\t%s\n' "$(basename "$config")" "$(git hash-object "$config")"
    else
      printf '%s\tmissing\n' "$(basename "$config")"
    fi
  done
  env | LC_ALL=C sort | grep -E '^(AR|CC|CFLAGS|CXX|CXXFLAGS|LDFLAGS|RUSTC|RUSTUP_TOOLCHAIN|CARGO_BUILD_TARGET|CARGO_PROFILE_RELEASE_)=' || true
} | git hash-object --stdin)

wrapper_identity() {
  local wrapper=$1
  if [[ -z $wrapper ]]; then
    printf 'environment:none;cargo_config:%s' "$config_identity"
    return
  fi
  local version
  version=$($wrapper --version 2>&1 | head -n 1) || version=version-unavailable
  printf 'environment:%s;%s' "$(basename "$wrapper")" "$version"
}

export TRACEDECAY_BENCHMARK_BUILD_COMMIT=$commit
export TRACEDECAY_BENCHMARK_BUILD_TREE=$tree
export TRACEDECAY_BENCHMARK_BUILD_PROFILE=release
export TRACEDECAY_BENCHMARK_BUILD_SOURCE_MODE=git_archive_read_only_v1
export TRACEDECAY_BENCHMARK_SOURCE_MANIFEST_SHA256=$source_manifest_sha256
export TRACEDECAY_BENCHMARK_BUILD_TARGET_TRIPLE=$host_target
export TRACEDECAY_BENCHMARK_BUILD_RUSTC_VERSION="$(rustc -Vv)"
export TRACEDECAY_BENCHMARK_BUILD_CARGO_VERSION="$(cargo -V)"
export TRACEDECAY_BENCHMARK_BUILD_RUSTFLAGS=normalized-empty
export TRACEDECAY_BENCHMARK_BUILD_RUSTC_WRAPPER="$(wrapper_identity "${RUSTC_WRAPPER:-}")"
export TRACEDECAY_BENCHMARK_BUILD_RUSTC_WORKSPACE_WRAPPER="$(wrapper_identity "${RUSTC_WORKSPACE_WRAPPER:-}")"
export TRACEDECAY_BENCHMARK_BUILD_CARGO_CONFIG_IDENTITY=$config_identity

# A fresh Git archive intentionally contains no generated dashboard assets.
# Generate them before freezing the source tree, then prove npm did not mutate
# any tracked input. The measured Cargo build can remain fully read-only.
(
  cd "$build_root/dashboard"
  npm ci
  npm run build
)
post_dashboard_manifest="$build_root/.tracedecay-benchmark-source-manifest.post-dashboard"
write_source_manifest "$post_dashboard_manifest"
if ! cmp -s "$source_manifest" "$post_dashboard_manifest"; then
  echo "dashboard build modified tracked benchmark source" >&2
  exit 1
fi
rm -f "$post_dashboard_manifest"

mkdir -p "$build_root/target"
chmod -R a-w "$build_root"
chmod u+rwx "$build_root/target"

(
  cd "$build_root"
  TRACEDECAY_SKIP_DASHBOARD_BUILD=1 cargo test --quiet --release --lib \
    claude_observation_benchmark::production_observation_pipeline_baseline -- \
    --ignored --exact --nocapture --test-threads=1
) 2>&1 | tee "$capture"

if [[ $(grep -c '^TRACEDECAY_CLAUDE_OBSERVATION_BENCHMARK_RESULT=' "$capture") -ne 1 ]]; then
  echo "benchmark did not emit exactly one result" >&2
  exit 1
fi
result_json=$(sed -n 's/^TRACEDECAY_CLAUDE_OBSERVATION_BENCHMARK_RESULT=\(.*\) $/\1/p' "$capture")
if [[ -z $result_json ]]; then
  echo "benchmark result marker was malformed" >&2
  exit 1
fi
printf '%s\n' "$result_json" >"$result_path"

sed "s/\"current_acceptance\": null/\"current_acceptance\": \"$result_name\"/" \
  "$index_backup" >"$index_path"
(
  cd "$build_root"
  TRACEDECAY_BENCHMARK_REQUIRE_ACCEPTANCE=1 \
  TRACEDECAY_BENCHMARK_EVIDENCE_DIR="$repo_root/benchmarks/claude-observation" \
  TRACEDECAY_SKIP_DASHBOARD_BUILD=1 \
    cargo test --quiet --release --lib \
      claude_observation_benchmark::evidence_directory_matches_index_contract -- \
      --exact --test-threads=1
)

complete=true
echo "validated $result_path"
echo "commit only the result, evidence index, and README summary as the evidence follow-up"
