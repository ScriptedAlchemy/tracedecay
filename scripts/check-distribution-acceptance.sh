#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
repo=$(cd -- "$(dirname -- "$script_path")/.." && pwd -P)
keep_temp=false

usage() {
  cat <<'EOF'
Usage: scripts/check-distribution-acceptance.sh [OPTIONS]

Build and exercise the release distribution with every Cargo feature enabled.
The gate packages every workspace crate, extracts the produced .crate archives
into an isolated temporary directory, and tests the packaged library and CLI.

Options:
  --repo PATH   Repository root (default: parent of this script)
  --keep-temp   Preserve the isolated package/install directory
  --plan        Print the heavyweight commands without running them
  --self-test   Run static script tests only
  -h, --help    Show this help
EOF
}

die() {
  echo "distribution acceptance: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

print_plan() {
  cat <<'EOF'
python3 tests/distribution/fastembed/prepare_fixture.py --check tests/distribution/fastembed
# Optional setup accelerator: TRACEDECAY_DISTRIBUTION_FASTEMBED_CACHE=<verified dir>
python3 tests/distribution/fastembed/prepare_fixture.py tests/distribution/fastembed <temporary verified fixture>
python3 tests/distribution/fastembed/validate_fixture.py <temporary verified fixture>
cargo build --workspace --release --all-features --lib --bins
cargo package --workspace --all-features --allow-dirty --no-verify
cargo check --release --all-features --lib <extracted tracedecay package>
CARGO_NET_OFFLINE=true cargo test --release --all-features --lib <packaged model-acquisition lifecycle suite>
CARGO_NET_OFFLINE=true HF_HUB_OFFLINE=1 cargo test --release --all-features --lib <packaged background model-acquisition acceptance>
CARGO_NET_OFFLINE=true cargo test --release --all-features --lib <packaged typed semantic-unavailable acceptance>
cargo install --root <temporary install root> --all-features <extracted tracedecay package>
CARGO_NET_OFFLINE=true cargo run --release --bin tracedecay-distribution-consumer <temporary packaged-library consumer>
cargo build --release --all-features --example fastembed_distribution_acceptance <extracted tracedecay package>
CARGO_NET_OFFLINE=true HF_HUB_OFFLINE=1 <built fastembed_distribution_acceptance> <verified local fixture>
<installed tracedecay> tool
<installed tracedecay> tool <required-tool> --help
<installed tracedecay> lsp servers --json
<installed tracedecay> lsp bridge --help
EOF
}

assert_fastembed_fixture() {
  local fixture_root=$1
  local validator=$2
  local required
  local -a missing=()
  for required in \
    fixture.json \
    model.onnx \
    tokenizer.json \
    config.json \
    special_tokens_map.json \
    tokenizer_config.json; do
    if [[ ! -f "$fixture_root/$required" || -L "$fixture_root/$required" ]]; then
      missing+=("$required")
    fi
  done
  if ((${#missing[@]})); then
    die "FastEmbed acceptance requires prepared real fixture bytes under $fixture_root; missing regular files: ${missing[*]}"
  fi
  [[ -f $validator ]] ||
    die "FastEmbed fixture validator is missing: $validator"
  python3 "$validator" "$fixture_root"
}

assert_required_assets() {
  local root_package=$1
  local application_package=$2
  local api_package=$3
  local lsp_package=$4
  local required
  local -a root_assets=(
    "plugin/.lsp.json"
    "plugin/.claude-plugin/plugin.json"
    "plugin/.codex-plugin/plugin.json"
    "plugin/.cursor-plugin/plugin.json"
    "plugin/.kimi-plugin/plugin.json"
    "plugin/cursor-native-extension/dist/extension.js"
    "dashboard/app-dist/index.html"
    "src/agents/host_bundle_registry.rs"
    "src/agents/host_bundle_v2.rs"
    "src/application/advisory/host_delivery.rs"
    "src/application/advisory/runtime.rs"
    "src/application/feedback/cycle_runtime.rs"
    "src/application/primitives/runtime.rs"
    "src/daemon/lsp_gateway/mod.rs"
    "src/query/retrieval/semantic/service.rs"
    "src/query/retrieval/semantic/tests.rs"
    "src/semantic_code/fastembed_adapter.rs"
    "src/semantic_code/model_lifecycle.rs"
    "tests/fixtures/packaged_host_events/claude.json"
    "tests/fixtures/packaged_host_events/claude/post_tool_use_write.json"
    "tests/fixtures/packaged_host_events/codex.json"
    "tests/fixtures/packaged_host_events/cursor.json"
    "tests/fixtures/packaged_host_events/hermes.json"
    "tests/fixtures/packaged_host_events/hermes/saved-edit.json"
    "tests/fixtures/packaged_host_events/hermes/terminal-receipt.json"
    "tests/fixtures/packaged_host_events/kiro.json"
    "tests/fixtures/packaged_host_events/kimi-code.json"
    "tests/fixtures/packaged_host_events/kimi/post-tool-use-edit.json"
    "tests/fixtures/packaged_host_events/opencode/baseline.json"
    # Packaged fixture/workload pins (PR5 harness embeds its workload via
    # include_str!; PR7 keeps a workload pin for distribution completeness).
    "tests/fixtures/provider_normalization/codex/session_meta.input.json"
    "tests/fixtures/provider_normalization/codex/agent_message.input.json"
    "tests/fixtures/analytics/codex_skill_prose.txt"
    "tests/session_suite/lcm_schema/mod.rs"
    "benchmarks/pr5-observation/workload-v1.json"
    "benchmarks/pr7-memory/workload-v1.json"
    "tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json"
    "benchmarks/search-quality/pr9-fallback-report-v1.json"
  )
  local -a application_assets=(
    "src/feedback/read.rs"
    "src/feedback/github_ci_proximity.rs"
    "src/advisory.rs"
  )
  local -a api_assets=(
    "src/http.rs"
    "src/sse.rs"
  )
  # The gateway protocol and stdio framing ship from their own crate now. The
  # root package keeps only the façade, so pinning them here is what keeps the
  # distribution gate covering the LSP surface end to end.
  local -a lsp_assets=(
    "src/bridge.rs"
    "src/protocol.rs"
  )

  for required in "${root_assets[@]}"; do
    [[ -f "$root_package/$required" ]] ||
      die "packaged tracedecay crate is missing $required"
  done
  for required in "${application_assets[@]}"; do
    [[ -f "$application_package/$required" ]] ||
      die "packaged tracedecay-application crate is missing $required"
  done
  for required in "${api_assets[@]}"; do
    [[ -f "$api_package/$required" ]] ||
      die "packaged tracedecay-api crate is missing $required"
  done
  for required in "${lsp_assets[@]}"; do
    [[ -f "$lsp_package/$required" ]] ||
      die "packaged tracedecay-lsp crate is missing $required"
  done
  python3 "$repo/scripts/check-dashboard-bundle.py" \
    "$root_package/dashboard/app-dist"

  python3 - "$root_package/plugin/.lsp.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    value = json.loads(path.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"distribution acceptance: invalid packaged JSON {path}: {error}")
if not isinstance(value, dict) or not value:
    raise SystemExit(
        f"distribution acceptance: packaged JSON must be a non-empty object: {path}"
    )
PY
  python3 "$repo/scripts/check-packaged-plugin-manifests.py" \
    "$root_package/plugin"
}

assert_code_extraction_assets() {
  local extraction_package=$1
  local required
  local -a assets=(
    "tests/rust.rs"
    "vendor/tree-sitter-rust/LICENSE"
    "vendor/tree-sitter-rust/queries/highlights.scm"
    "vendor/tree-sitter-rust/queries/injections.scm"
    "vendor/tree-sitter-rust/queries/tags.scm"
    "vendor/tree-sitter-rust/src/node-types.json"
    "vendor/tree-sitter-rust/src/parser.c"
    "vendor/tree-sitter-rust/src/scanner.c"
  )

  for required in "${assets[@]}"; do
    [[ -f "$extraction_package/$required" ]] ||
      die "packaged tracedecay-code-extraction crate is missing $required"
  done
}

verify_feature_wiring() {
  local source_manifest=$1
  local packaged_manifest=$2
  python3 - "$source_manifest" "$packaged_manifest" <<'PY'
import sys
import tomllib
from pathlib import Path

REQUIRED = {
    "full",
    "token-counting",
    "semantic-fastembed",
    "test-transport",
}


def load(path: str) -> dict:
    with Path(path).open("rb") as handle:
        return tomllib.load(handle)


def optional_dependencies(manifest: dict) -> set[str]:
    names: set[str] = set()

    def collect(table: object) -> None:
        if not isinstance(table, dict):
            return
        dependencies = table.get("dependencies")
        if isinstance(dependencies, dict):
            for name, spec in dependencies.items():
                if isinstance(spec, dict) and spec.get("optional") is True:
                    names.add(name)

    collect(manifest)
    for target in manifest.get("target", {}).values():
        collect(target)
    return names


source = load(sys.argv[1])
packaged = load(sys.argv[2])
source_features = source.get("features", {})
packaged_features = packaged.get("features", {})

missing = sorted(REQUIRED - source_features.keys())
if missing:
    raise SystemExit(
        "distribution acceptance: source manifest is missing required features: "
        + ", ".join(missing)
    )
if source_features != packaged_features:
    raise SystemExit(
        "distribution acceptance: packaged feature wiring differs from Cargo.toml"
    )

semantic_members = packaged_features.get("semantic-fastembed")
required_semantic_members = {
    "dep:fastembed",
    "fastembed/ort-download-binaries-rustls-tls",
}
if not isinstance(semantic_members, list) or not required_semantic_members.issubset(
    semantic_members
):
    raise SystemExit(
        "distribution acceptance: semantic-fastembed must enable dep:fastembed "
        "and fastembed/ort-download-binaries-rustls-tls"
    )
fastembed_dependency = packaged.get("dependencies", {}).get("fastembed")
if (
    not isinstance(fastembed_dependency, dict)
    or fastembed_dependency.get("optional") is not True
    or fastembed_dependency.get("default-features") is not False
):
    raise SystemExit(
        "distribution acceptance: fastembed must remain optional with default features disabled"
    )

references = {
    item
    for members in packaged_features.values()
    for item in members
    if isinstance(item, str)
}
unwired = sorted(
    dependency
    for dependency in optional_dependencies(packaged)
    if f"dep:{dependency}" not in references
    and dependency not in packaged_features
)
if unwired:
    raise SystemExit(
        "distribution acceptance: optional dependencies are not feature-wired: "
        + ", ".join(unwired)
    )
PY
}

run_self_test() {
  bash -n "$script_path"

  local plan
  plan=$("$script_path" --plan)
  local expected
  for expected in \
    "python3 tests/distribution/fastembed/prepare_fixture.py" \
    "python3 tests/distribution/fastembed/validate_fixture.py" \
    "cargo build --workspace --release --all-features --lib --bins" \
    "cargo package --workspace --all-features --allow-dirty --no-verify" \
    "cargo check --release --all-features --lib" \
    "packaged model-acquisition lifecycle suite" \
    "packaged background model-acquisition acceptance" \
    "CARGO_NET_OFFLINE=true cargo test --release --all-features --lib" \
    "cargo install --root <temporary install root> --all-features" \
    "cargo build --release --all-features --example fastembed_distribution_acceptance" \
    "CARGO_NET_OFFLINE=true HF_HUB_OFFLINE=1 <built fastembed_distribution_acceptance>"; do
    [[ $plan == *"$expected"* ]] ||
      die "self-test: plan omitted required all-feature command: $expected"
  done
  local pinned_fixture_metadata
  pinned_fixture_metadata=$(python3 \
    "$repo/tests/distribution/fastembed/prepare_fixture.py" \
    --check \
    "$repo/tests/distribution/fastembed")
  [[ $pinned_fixture_metadata == $'768\t8192' ]] ||
    die "self-test: pinned Jina fixture metadata is invalid"

  local fixture
  fixture=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-distribution-script-test.XXXXXX")
  local root="$fixture/root"
  local application="$fixture/application"
  local api="$fixture/api"
  local lsp="$fixture/lsp"
  local path
  for path in plugin/.lsp.json; do
    mkdir -p -- "$root/$(dirname -- "$path")"
    printf '{"fixture":true}\n' >"$root/$path"
  done
  for path in \
    plugin/.claude-plugin/plugin.json \
    plugin/.codex-plugin/plugin.json \
    plugin/.cursor-plugin/plugin.json \
    plugin/.kimi-plugin/plugin.json; do
    mkdir -p -- "$root/$(dirname -- "$path")"
    cp -- "$repo/$path" "$root/$path"
  done
  for path in \
    plugin/cursor-native-extension/dist/extension.js \
    src/agents/host_bundle_registry.rs \
    src/agents/host_bundle_v2.rs \
    src/application/advisory/host_delivery.rs \
    src/application/advisory/runtime.rs \
    src/application/feedback/cycle_runtime.rs \
    src/application/primitives/runtime.rs \
    src/daemon/lsp_gateway/mod.rs \
    src/query/retrieval/semantic/service.rs \
    src/query/retrieval/semantic/tests.rs \
    src/semantic_code/fastembed_adapter.rs \
    src/semantic_code/model_lifecycle.rs \
    tests/fixtures/packaged_host_events/claude.json \
    tests/fixtures/packaged_host_events/claude/post_tool_use_write.json \
    tests/fixtures/packaged_host_events/codex.json \
    tests/fixtures/packaged_host_events/cursor.json \
    tests/fixtures/packaged_host_events/hermes.json \
    tests/fixtures/packaged_host_events/hermes/saved-edit.json \
    tests/fixtures/packaged_host_events/hermes/terminal-receipt.json \
    tests/fixtures/packaged_host_events/kiro.json \
    tests/fixtures/packaged_host_events/kimi-code.json \
    tests/fixtures/packaged_host_events/kimi/post-tool-use-edit.json \
    tests/fixtures/packaged_host_events/opencode/baseline.json \
    tests/fixtures/provider_normalization/codex/session_meta.input.json \
    tests/fixtures/provider_normalization/codex/agent_message.input.json \
    tests/fixtures/analytics/codex_skill_prose.txt \
    tests/session_suite/lcm_schema/mod.rs \
    benchmarks/pr5-observation/workload-v1.json \
    benchmarks/pr7-memory/workload-v1.json \
    tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json \
    benchmarks/search-quality/pr9-fallback-report-v1.json; do
    mkdir -p -- "$root/$(dirname -- "$path")"
    : >"$root/$path"
  done
  for path in src/feedback/read.rs src/feedback/github_ci_proximity.rs src/advisory.rs; do
    mkdir -p -- "$application/$(dirname -- "$path")"
    : >"$application/$path"
  done
  for path in src/http.rs src/sse.rs; do
    mkdir -p -- "$api/$(dirname -- "$path")"
    : >"$api/$path"
  done
  for path in src/bridge.rs src/protocol.rs; do
    mkdir -p -- "$lsp/$(dirname -- "$path")"
    : >"$lsp/$path"
  done
  mkdir -p -- "$root/dashboard/app-dist/static/js"
  printf '<script src="/static/js/index.fixture.js"></script>\n' \
    >"$root/dashboard/app-dist/index.html"
  printf '%0800d' 0 >"$root/dashboard/app-dist/static/js/index.fixture.js"
  assert_required_assets "$root" "$application" "$api" "$lsp"
  rm -- "$lsp/src/protocol.rs"
  if (assert_required_assets "$root" "$application" "$api" "$lsp") >/dev/null 2>&1; then
    die "self-test: missing extracted LSP asset was accepted"
  fi
  : >"$lsp/src/protocol.rs"
  rm -- "$root/plugin/.lsp.json"
  if (assert_required_assets "$root" "$application" "$api" "$lsp") >/dev/null 2>&1; then
    die "self-test: missing distribution asset was accepted"
  fi

  cat >"$fixture/source.toml" <<'EOF'
[package]
name = "fixture"
version = "0.0.0"

[features]
full = ["medium"]
medium = ["dep:medium"]
token-counting = ["dep:tokens"]
semantic-fastembed = ["dep:fastembed"]
test-transport = []

[dependencies]
medium = { version = "1", optional = true }
tokens = { version = "1", optional = true }
fastembed = { version = "1", optional = true, default-features = false }
EOF
  python3 - "$fixture/source.toml" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
path.write_text(
    path.read_text(encoding="utf-8").replace(
        'semantic-fastembed = ["dep:fastembed"]',
        'semantic-fastembed = ["dep:fastembed", "fastembed/ort-download-binaries-rustls-tls"]',
    ),
    encoding="utf-8",
)
PY
  cp -- "$fixture/source.toml" "$fixture/packaged.toml"
  verify_feature_wiring "$fixture/source.toml" "$fixture/packaged.toml"
  python3 - "$fixture/packaged.toml" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
path.write_text(
    path.read_text(encoding="utf-8").replace(
        ', "fastembed/ort-download-binaries-rustls-tls"', ""
    ),
    encoding="utf-8",
)
PY
  if (verify_feature_wiring "$fixture/source.toml" "$fixture/packaged.toml") \
    >/dev/null 2>&1; then
    die "self-test: missing bundled ORT feature wiring was accepted"
  fi
  cp -- "$fixture/source.toml" "$fixture/packaged.toml"
  python3 - "$fixture/packaged.toml" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
path.write_text(
    path.read_text(encoding="utf-8").replace(
        'semantic-fastembed = ["dep:fastembed", "fastembed/ort-download-binaries-rustls-tls"]\n',
        "",
    ),
    encoding="utf-8",
)
PY
  if (verify_feature_wiring "$fixture/source.toml" "$fixture/packaged.toml") \
    >/dev/null 2>&1; then
    die "self-test: missing feature wiring was accepted"
  fi

  local fastembed_fixture="$fixture/fastembed"
  mkdir -p -- "$fastembed_fixture"
  python3 - "$fastembed_fixture" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
members = {
    "model": "model.onnx",
    "tokenizer": "tokenizer.json",
    "config": "config.json",
    "special_tokens_map": "special_tokens_map.json",
    "tokenizer_config": "tokenizer_config.json",
}
manifest_members = {}
for role, name in members.items():
    data = f"static validator fixture for {role}".encode()
    (root / name).write_bytes(data)
    manifest_members[role] = {
        "path": name,
        "upstream_path": name,
        "length": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }
(root / "fixture.json").write_text(
    json.dumps(
        {
            "schema": "tracedecay.distribution.fastembed-fixture.v1",
            "model": "JinaEmbeddingsV2BaseCode",
            "source": {
                "upstream": "self-test",
                "revision": "immutable-self-test",
                "license": "MIT",
                "license_url": "https://example.invalid/license",
                "provenance": "self-test provenance",
            },
            "expected_dimensions": 2,
            "max_length": 8,
            "members": manifest_members,
        }
    ),
    encoding="utf-8",
)
PY
  local fixture_metadata
  fixture_metadata=$(assert_fastembed_fixture \
    "$fastembed_fixture" \
    "$repo/tests/distribution/fastembed/validate_fixture.py")
  [[ $fixture_metadata == $'2\t8' ]] ||
    die "self-test: FastEmbed fixture validator returned unexpected metadata"
  printf 'corrupt\n' >>"$fastembed_fixture/model.onnx"
  if (assert_fastembed_fixture \
    "$fastembed_fixture" \
    "$repo/tests/distribution/fastembed/validate_fixture.py") >/dev/null 2>&1; then
    die "self-test: corrupt FastEmbed fixture was accepted"
  fi

  rm -rf -- "$fixture"
  echo "distribution acceptance script self-test passed"
}

while (($#)); do
  case "$1" in
    --repo)
      [[ $# -ge 2 ]] || die "--repo requires a path"
      repo=$2
      shift 2
      ;;
    --keep-temp)
      keep_temp=true
      shift
      ;;
    --plan)
      print_plan
      exit 0
      ;;
    --self-test)
      run_self_test
      exit 0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      die "unknown argument: $1"
      ;;
  esac
done

require_command cargo
require_command cmp
require_command curl
require_command python3
require_command tar

repo=$(cd -- "$repo" && pwd -P)
[[ -f "$repo/Cargo.toml" ]] || die "Cargo.toml not found under $repo"
for fixture in \
  claude.json \
  claude/post_tool_use_write.json \
  codex.json \
  cursor.json \
  hermes.json \
  hermes/saved-edit.json \
  hermes/terminal-receipt.json \
  kiro.json \
  kimi-code.json \
  kimi/post-tool-use-edit.json \
  opencode/baseline.json; do
  cmp -s \
    "$repo/crates/tracedecay-hooks/fixtures/host_events/$fixture" \
    "$repo/tests/fixtures/packaged_host_events/$fixture" ||
    die "packaged host-event fixture copy differs from its authority: $fixture"
done

work=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-distribution.XXXXXX")
cleanup() {
  if [[ $keep_temp == true ]]; then
    echo "distribution acceptance: preserved temporary directory $work"
  else
    rm -rf -- "$work"
  fi
}
trap cleanup EXIT

fastembed_fixture_source="$repo/tests/distribution/fastembed"
fastembed_acquisition_acceptance="$fastembed_fixture_source/acquisition_tests.rs.inc"
fastembed_fixture="$work/fastembed"
[[ -f $fastembed_acquisition_acceptance ]] ||
  die "FastEmbed acquisition acceptance is missing: $fastembed_acquisition_acceptance"
echo "distribution acceptance: acquiring immutable Jina FastEmbed fixture"
python3 \
  "$fastembed_fixture_source/prepare_fixture.py" \
  "$fastembed_fixture_source" \
  "$fastembed_fixture"
fixture_metadata=$(assert_fastembed_fixture \
  "$fastembed_fixture" \
  "$fastembed_fixture_source/validate_fixture.py")
IFS=$'\t' read -r fastembed_dimensions fastembed_max_length <<<"$fixture_metadata"

echo "distribution acceptance: release-building every feature"
cargo build \
  --manifest-path "$repo/Cargo.toml" \
  --workspace \
  --release \
  --all-features \
  --lib \
  --bins

echo "distribution acceptance: packaging every workspace crate"
cargo package \
  --manifest-path "$repo/Cargo.toml" \
  --workspace \
  --all-features \
  --allow-dirty \
  --no-verify

metadata="$work/metadata.json"
cargo metadata \
  --manifest-path "$repo/Cargo.toml" \
  --format-version 1 \
  --no-deps >"$metadata"

target_directory=$(python3 - "$metadata" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    print(json.load(handle)["target_directory"])
PY
)

package_table="$work/packages.tsv"
python3 - "$metadata" >"$package_table" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    metadata = json.load(handle)
members = set(metadata["workspace_members"])
for package in metadata["packages"]:
    if package["id"] in members:
        print(f'{package["name"]}\t{package["version"]}')
PY

packages="$work/packages"
mkdir -p -- "$packages"
declare -A package_dirs=()
while IFS=$'\t' read -r name version; do
  archive="$target_directory/package/$name-$version.crate"
  [[ -f "$archive" ]] ||
    die "cargo package did not produce $archive"
  tar -xzf "$archive" -C "$packages"
  directory="$packages/$name-$version"
  [[ -f "$directory/Cargo.toml" ]] ||
    die "package archive did not contain $name-$version/Cargo.toml"
  package_dirs["$name"]=$directory
done <"$package_table"

root_package=${package_dirs[tracedecay]:-}
application_package=${package_dirs[tracedecay-application]:-}
api_package=${package_dirs[tracedecay-api]:-}
catalog_package=${package_dirs[tracedecay-tool-catalog]:-}
lsp_package=${package_dirs[tracedecay-lsp]:-}
code_extraction_package=${package_dirs[tracedecay-code-extraction]:-}
[[ -n $root_package && -n $application_package && -n $api_package && -n $catalog_package && -n $lsp_package && -n $code_extraction_package ]] ||
  die "workspace packages required by the distribution gate were not produced"

assert_required_assets \
  "$root_package" "$application_package" "$api_package" "$lsp_package"
assert_code_extraction_assets "$code_extraction_package"
verify_feature_wiring "$repo/Cargo.toml" "$root_package/Cargo.toml"

patch_config="$work/packaged-crates.toml"
python3 - "$metadata" "$packages" >"$patch_config" <<'PY'
import json
import pathlib
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    metadata = json.load(handle)
members = set(metadata["workspace_members"])
packages = pathlib.Path(sys.argv[2])
print("[patch.crates-io]")
for package in sorted(metadata["packages"], key=lambda value: value["name"]):
    if package["id"] not in members or package["name"] == "tracedecay":
        continue
    path = packages / f'{package["name"]}-{package["version"]}'
    print(f'{json.dumps(package["name"])} = {{ path = {json.dumps(str(path))} }}')
PY

echo "distribution acceptance: testing packaged patched Rust grammar"
cargo test \
  --manifest-path "$code_extraction_package/Cargo.toml" \
  --all-features \
  --config "$patch_config" \
  --test rust \
  test_rust_cfg_attribute_in_struct_pattern_field \
  -- --exact

echo "distribution acceptance: compiling packaged library with every feature"
cargo check \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --all-features \
  --lib \
  --config "$patch_config"

ort_lib_path=${ORT_LIB_PATH:-$(python3 - <<'PY'
import os
from pathlib import Path

cache_root = Path(
    os.environ.get("ORT_CACHE_DIR", Path.home() / ".cache" / "ort.pyke.io")
)
names = {"libonnxruntime.a", "libonnxruntime.dylib", "onnxruntime.lib"}
candidates = [
    path
    for path in cache_root.glob("dfbin/**/*")
    if path.is_file()
    and (path.name in names or path.name.startswith("libonnxruntime.so"))
]
if candidates:
    print(max(candidates, key=lambda path: path.stat().st_mtime).parent)
PY
)}
[[ -n $ort_lib_path ]] ||
  die "cached ONNX Runtime library is unavailable for the offline semantic tests"
export ORT_LIB_PATH="$ort_lib_path"

echo "distribution acceptance: checking packaged model-acquisition lifecycle suite"
CARGO_NET_OFFLINE=true cargo test \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --all-features \
  --lib \
  --config "$patch_config" \
  semantic_code::model_lifecycle::tests::

cat "$repo/tests/distribution/fastembed/semantic_unavailable_tests.rs.inc" \
  >>"$root_package/src/query/retrieval/semantic/tests.rs"
cat "$fastembed_acquisition_acceptance" \
  >>"$root_package/src/semantic_code/model_lifecycle.rs"
echo "distribution acceptance: checking packaged background model acquisition"
TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE="$fastembed_fixture" \
  TRACEDECAY_DISTRIBUTION_FASTEMBED_PROFILE_PARENT="$work/semantic-model-profile" \
  CARGO_NET_OFFLINE=true \
  HF_HUB_OFFLINE=1 \
  cargo test \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --all-features \
  --lib \
  --config "$patch_config" \
  semantic_code::model_lifecycle::distribution_acquisition_acceptance::distribution_background_acquisition_installs_verified_jina_model \
  -- \
  --exact
echo "distribution acceptance: checking typed semantic fallback and strict unavailability"
CARGO_NET_OFFLINE=true cargo test \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --all-features \
  --lib \
  --config "$patch_config" \
  query::retrieval::semantic::tests::distribution_missing_or_invalid_artifacts_use_typed_fallback_and_strict_unavailable \
  -- \
  --exact

install_root="$work/install"
echo "distribution acceptance: installing packaged CLI with every feature"
cargo install \
  --path "$root_package" \
  --root "$install_root" \
  --all-features \
  --config "$patch_config"

consumer="$work/library-consumer"
mkdir -p -- "$consumer/src"
python3 - "$root_package/Cargo.toml" "$root_package" "$catalog_package" \
  >"$consumer/Cargo.toml" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

with Path(sys.argv[1]).open("rb") as handle:
    manifest = tomllib.load(handle)
features = sorted(manifest.get("features", {}))
print("""[package]
name = "tracedecay-distribution-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]""")
print(
    "tracedecay = { path = "
    + json.dumps(sys.argv[2])
    + ", default-features = false, features = "
    + json.dumps(features)
    + " }"
)
print(
    "tracedecay-tool-catalog = { path = "
    + json.dumps(sys.argv[3])
    + " }"
)
PY
cat >"$consumer/src/main.rs" <<'RS'
use tracedecay::agents::host_bundle_registry::{
    default_components, verified_embedded_default_host_component_set,
    verified_embedded_host_bundle,
};
use tracedecay::agents::host_bundle_v2::{HostKindV1, stock_host_kinds};
use tracedecay::catalog_composition::build_application_catalog_snapshot;
use tracedecay_tool_catalog::{AvailabilityContract, CapabilityId};

const REQUIRED_CAPABILITIES: [&str; 10] = [
    "capability.application.feedback.diagnostics",
    "capability.application.feedback.get",
    "capability.application.feedback.expand",
    "capability.application.feedback.list",
    "capability.application.feedback.impact",
    "capability.application.feedback.affected-tests",
    "capability.application.feedback.test-results",
    "capability.application.feedback.github-review-ingest",
    "capability.application.feedback.ci-failure-localize",
    "capability.application.feedback.proximity",
];

const REQUIRED_HOSTS: [HostKindV1; 6] = [
    HostKindV1::ClaudeCode,
    HostKindV1::CursorDesktop,
    HostKindV1::Codex,
    HostKindV1::Hermes,
    HostKindV1::KimiCode,
    HostKindV1::OpenCode,
];

fn main() {
    let snapshot = build_application_catalog_snapshot()
        .expect("packaged application catalog must compose");
    for raw_id in REQUIRED_CAPABILITIES {
        let id = CapabilityId::new(raw_id).expect("required capability ID must be valid");
        let capability = snapshot
            .capability(&id)
            .unwrap_or_else(|| panic!("packaged catalog omitted {raw_id}"));
        assert!(
            matches!(capability.availability(), AvailabilityContract::Available),
            "packaged catalog capability is not callable: {raw_id}"
        );
    }

    let supported_hosts = stock_host_kinds()
        .into_iter()
        .filter(|host| !default_components(*host).is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        supported_hosts,
        REQUIRED_HOSTS.to_vec(),
        "packaged default host bundle inventory changed"
    );
    for host in REQUIRED_HOSTS {
        let components = default_components(host);
        let component_set = verified_embedded_default_host_component_set(host, 0)
            .expect("default packaged host component set must verify");
        assert_eq!(component_set.component_set.components.len(), components.len());
        for component in components {
            let bundle = verified_embedded_host_bundle(host, component, 0)
                .expect("packaged host bundle must be callable");
            bundle
                .manifest
                .validate_structure()
                .expect("packaged host bundle manifest must validate");
            assert!(!bundle.contents.is_empty(), "packaged host bundle has no assets");
        }
    }
}
RS

echo "distribution acceptance: calling packaged catalog and host bundles"
CARGO_NET_OFFLINE=true cargo run \
  --manifest-path "$consumer/Cargo.toml" \
  --release \
  --bin tracedecay-distribution-consumer \
  --config "$patch_config"

mkdir -p -- "$root_package/examples"
cp -- \
  "$repo/tests/distribution/fastembed/acceptance.rs" \
  "$root_package/examples/fastembed_distribution_acceptance.rs"
cat >>"$root_package/Cargo.toml" <<'TOML'

[[example]]
name = "fastembed_distribution_acceptance"
path = "examples/fastembed_distribution_acceptance.rs"
TOML
echo "distribution acceptance: building packaged FastEmbed and bundled ORT smoke"
fastembed_build_messages="$work/fastembed-build.jsonl"
cargo build \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --all-features \
  --example fastembed_distribution_acceptance \
  --config "$patch_config" \
  --message-format=json-render-diagnostics >"$fastembed_build_messages"
fastembed_binary=$(python3 - "$fastembed_build_messages" <<'PY'
import json
import sys

executable = None
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        message = json.loads(line)
        target = message.get("target", {})
        if (
            message.get("reason") == "compiler-artifact"
            and target.get("name") == "fastembed_distribution_acceptance"
            and "example" in target.get("kind", [])
            and message.get("executable")
        ):
            executable = message["executable"]
if executable is None:
    raise SystemExit(
        "distribution acceptance: Cargo did not report the FastEmbed example executable"
    )
print(executable)
PY
)
[[ -x $fastembed_binary ]] ||
  die "FastEmbed acceptance executable is missing: $fastembed_binary"
echo "distribution acceptance: calling FastEmbed and bundled ORT with verified local bytes"
CARGO_NET_OFFLINE=true HF_HUB_OFFLINE=1 "$fastembed_binary" \
  "$fastembed_fixture" \
  "$fastembed_dimensions" \
  "$fastembed_max_length"

binary="$install_root/bin/tracedecay"
[[ -x $binary ]] || die "cargo install did not produce $binary"
"$binary" --version

tool_list="$work/tool-list.txt"
"$binary" tool >"$tool_list"
for required_tool in \
  diagnostics \
  impact \
  affected \
  test_map; do
  if ! python3 - "$tool_list" "$required_tool" <<'PY'
from pathlib import Path
import sys

names = {
    line.split()[0]
    for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
    if line.startswith("  ") and line.split()
}
raise SystemExit(0 if sys.argv[2] in names else 1)
PY
  then
    die "installed CLI tool catalog omitted $required_tool"
  fi
  "$binary" tool "$required_tool" --help >/dev/null
done

lsp_servers="$work/lsp-servers.json"
"$binary" lsp servers --json >"$lsp_servers"
python3 - "$lsp_servers" <<'PY'
import json
import sys
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if not isinstance(value, list) or not value:
    raise SystemExit("distribution acceptance: lsp servers returned an empty or invalid inventory")
required_languages = {"rust", "typescript", "javascript", "python", "go", "c", "cpp"}
languages = set()
for server in value:
    if (
        not isinstance(server, dict)
        or not isinstance(server.get("language"), str)
        or not isinstance(server.get("language_id"), str)
        or not isinstance(server.get("command"), str)
        or not isinstance(server.get("available"), bool)
        or not isinstance(server.get("extensions"), list)
        or not server["extensions"]
    ):
        raise SystemExit("distribution acceptance: lsp servers returned an invalid server entry")
    languages.add(server["language"])
missing = sorted(required_languages - languages)
if missing:
    raise SystemExit(
        "distribution acceptance: lsp server inventory omitted required languages: "
        + ", ".join(missing)
    )
PY
python3 "$repo/scripts/check-packaged-lsp-bridge.py" \
  "$binary" \
  "$work/lsp-bridge"

echo "distribution acceptance passed"
