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

# The FastEmbed distribution-acquisition regression suite is doubly conditional:
# its module is `#[cfg(all(test, feature = "semantic-fastembed"))]` and its
# tests are `#[ignore]`d because they need this gate's isolated profile and
# verified Jina fixture. That means it runs in exactly one place — the semantic
# leg below, under `--features semantic-fastembed --run-ignored all`. If either
# side of that pairing is dropped the suite stops running *silently*: the lib
# test binary still has hundreds of other tests, so `--no-tests=fail` would not
# notice. Assert the pairing statically, before the expensive packaging work.
assert_gated_acquisition_suite() {
  local source_repo=$1
  local gate_script=$2
  python3 - "$source_repo" "$gate_script" <<'PY'
import re
import sys
from pathlib import Path

repo = Path(sys.argv[1])
gate = Path(sys.argv[2])

declaration = repo / "crates/tracedecay-semantic/src/model_lifecycle.rs"
suite = repo / "crates/tracedecay-semantic/src/model_lifecycle/distribution_acquisition_acceptance.rs"

if not suite.is_file():
    raise SystemExit(
        "distribution acceptance: the FastEmbed distribution-acquisition regression "
        f"suite is missing: {suite}"
    )

declaration_text = declaration.read_text(encoding="utf-8")
if not re.search(
    r'#\[cfg\(all\(test,\s*feature\s*=\s*"semantic-fastembed"\)\)\]\s*\n'
    r'#\[path = "model_lifecycle/distribution_acquisition_acceptance\.rs"\]\s*\n'
    r"mod distribution_acquisition_acceptance;",
    declaration_text,
):
    raise SystemExit(
        "distribution acceptance: the acquisition suite is no longer declared under "
        f'#[cfg(all(test, feature = "semantic-fastembed"))] in {declaration}'
    )

suite_text = suite.read_text(encoding="utf-8")
ignored = len(re.findall(r"#\[ignore", suite_text))
tests = len(re.findall(r"#\[test\]", suite_text))
if tests == 0 or ignored != tests:
    raise SystemExit(
        "distribution acceptance: the acquisition suite must be entirely #[ignore]d "
        f"so only this gate runs it (found {tests} tests, {ignored} ignored)"
    )

gate_text = gate.read_text(encoding="utf-8")
for required in ("--features semantic-fastembed", "--run-ignored all"):
    # Match the flag as a real continued command-line argument, not as any
    # mention of the string — otherwise this very check, which names both flags
    # in its own diagnostics, would keep satisfying itself after the invocation
    # below lost them.
    if not re.search(rf"^\s+{re.escape(required)} \\$", gate_text, re.MULTILINE):
        raise SystemExit(
            "distribution acceptance: this gate no longer passes "
            f"{required!r}, so the FastEmbed acquisition suite would never run"
        )
print(
    f"distribution acceptance: FastEmbed acquisition suite gated and reachable "
    f"({tests} tests)"
)
PY
}

assert_required_assets() {
  local root_package=$1
  local required
  local -a root_assets=(
    "plugin/.lsp.json"
    "plugin/.claude-plugin/plugin.json"
    "plugin/.codex-plugin/plugin.json"
    "plugin/.cursor-plugin/plugin.json"
    "plugin/.kimi-plugin/plugin.json"
    "plugin/cursor-native-extension/embedded/extension.js"
    "dashboard/app-dist/index.html"
    "tests/fixtures/packaged_host_events/claude.json"
    "tests/fixtures/packaged_host_events/claude/post_tool_use_write.json"
    "tests/fixtures/packaged_host_events/cline-family.json"
    "tests/fixtures/packaged_host_events/codex.json"
    "tests/fixtures/packaged_host_events/cursor.json"
    "tests/fixtures/packaged_host_events/hermes.json"
    "tests/fixtures/packaged_host_events/hermes/saved-edit.json"
    "tests/fixtures/packaged_host_events/hermes/terminal-receipt.json"
    "tests/fixtures/packaged_host_events/kiro.json"
    "tests/fixtures/packaged_host_events/kimi-code.json"
    "tests/fixtures/packaged_host_events/kimi/post-tool-use-edit.json"
    "tests/fixtures/packaged_host_events/opencode/baseline.json"
    "tests/fixtures/provider_normalization/codex/session_meta.input.json"
    "tests/fixtures/provider_normalization/codex/agent_message.input.json"
    "tests/fixtures/analytics/codex_skill_prose.txt"
    "benchmarks/claude-observation/workload-v1.json"
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"
    "benchmarks/search-quality/query-fallback-report-v1.json"
  )

  for required in "${root_assets[@]}"; do
    [[ -f "$root_package/$required" ]] ||
      die "packaged tracedecay crate is missing $required"
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
  local semantic_source_manifest=$3
  local semantic_packaged_manifest=$4
  python3 "$repo/scripts/check-distribution-feature-wiring.py" \
    --root-source "$source_manifest" \
    --root-packaged "$packaged_manifest" \
    --semantic-source "$semantic_source_manifest" \
    --semantic-packaged "$semantic_packaged_manifest"
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
  cline-family.json \
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

assert_gated_acquisition_suite "$repo" "$script_path"

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
fastembed_fixture="$work/fastembed"
echo "distribution acceptance: acquiring immutable Jina FastEmbed fixture"
python3 \
  "$fastembed_fixture_source/prepare_fixture.py" \
  "$fastembed_fixture_source" \
  "$fastembed_fixture"
fixture_metadata=$(assert_fastembed_fixture \
  "$fastembed_fixture" \
  "$fastembed_fixture_source/validate_fixture.py")
IFS=$'\t' read -r fastembed_dimensions fastembed_max_length <<<"$fixture_metadata"

echo "distribution acceptance: release-building the production feature set"
cargo build \
  --manifest-path "$repo/Cargo.toml" \
  --workspace \
  --release \
  --no-default-features \
  --features tracedecay/production \
  --lib \
  --bins

echo "distribution acceptance: packaging every workspace crate"
# Every workspace member is publish = false: releases ship GitHub-release
# artifacts, never crates.io uploads. Packaging is used here only to produce
# the per-crate source trees the acceptance battery runs against, so skip the
# per-package lockfile: generating it would resolve the unpublished internal
# dependencies against crates.io and fail. Nothing downstream reads the
# embedded lock — every extracted tree resolves through the [patch.crates-io]
# path overlay below, and the install step builds from a path, not an archive.
cargo package \
  --manifest-path "$repo/Cargo.toml" \
  --workspace \
  --allow-dirty \
  --no-verify \
  --exclude-lockfile

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

for required_package in \
  tracedecay \
  tracedecay-application \
  tracedecay-api \
  tracedecay-tool-catalog \
  tracedecay-lsp \
  tracedecay-code-extraction \
  tracedecay-query \
  tracedecay-semantic; do
  [[ -n ${package_dirs[$required_package]:-} ]] ||
    die "workspace package required by the distribution gate was not produced: $required_package"
done
root_package=${package_dirs[tracedecay]}
lsp_package=${package_dirs[tracedecay-lsp]}
code_extraction_package=${package_dirs[tracedecay-code-extraction]}
query_package=${package_dirs[tracedecay-query]}
semantic_package=${package_dirs[tracedecay-semantic]}
catalog_package=${package_dirs[tracedecay-tool-catalog]}

assert_required_assets "$root_package"
assert_code_extraction_assets "$code_extraction_package"
verify_feature_wiring \
  "$repo/Cargo.toml" \
  "$root_package/Cargo.toml" \
  "$repo/crates/tracedecay-semantic/Cargo.toml" \
  "$semantic_package/Cargo.toml"

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
cargo nextest run \
  --manifest-path "$code_extraction_package/Cargo.toml" \
  --all-features \
  --config "$patch_config" \
  --test rust \
  --no-tests=fail

echo "distribution acceptance: compiling packaged library with production features"
cargo check \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --no-default-features \
  --features production \
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

echo "distribution acceptance: checking extracted query semantic fallback behavior"
CARGO_NET_OFFLINE=true cargo nextest run \
  --manifest-path "$query_package/Cargo.toml" \
  --release \
  --all-features \
  --lib \
  --config "$patch_config" \
  --no-tests=fail

echo "distribution acceptance: checking extracted root strict semantic unavailability"
CARGO_NET_OFFLINE=true cargo nextest run \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --no-default-features \
  --features production \
  --lib \
  --config "$patch_config" \
  --no-tests=fail

echo "distribution acceptance: checking extracted LSP framing and protocol behavior"
CARGO_NET_OFFLINE=true cargo nextest run \
  --manifest-path "$lsp_package/Cargo.toml" \
  --release \
  --all-features \
  --lib \
  --config "$patch_config" \
  --no-tests=fail

echo "distribution acceptance: checking packaged MCP tool behavior"
CARGO_NET_OFFLINE=true cargo nextest run \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --no-default-features \
  --features production \
  --test mcp_suite \
  --config "$patch_config" \
  --no-tests=fail

echo "distribution acceptance: checking packaged semantic lifecycle and Jina acquisition"
TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE="$fastembed_fixture" \
  TRACEDECAY_DISTRIBUTION_FASTEMBED_PROFILE_PARENT="$work/semantic-model-profile" \
  CARGO_NET_OFFLINE=true \
  HF_HUB_OFFLINE=1 \
  cargo nextest run \
  --manifest-path "$semantic_package/Cargo.toml" \
  --release \
  --features semantic-fastembed \
  --lib \
  --run-ignored all \
  --config "$patch_config" \
  --no-tests=fail

echo "distribution acceptance: exercising packaged semantic activation and recovery"
TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE="$fastembed_fixture" \
  TRACEDECAY_DISTRIBUTION_FASTEMBED_PROFILE_PARENT="$work/semantic-activation-profile" \
  CARGO_NET_OFFLINE=true \
  HF_HUB_OFFLINE=1 \
  cargo nextest run \
  --manifest-path "$root_package/Cargo.toml" \
  --release \
  --no-default-features \
  --features production \
  --lib \
  --config "$patch_config" \
  -E 'test(~semantic_activation_journey_test::public_semantic_activation_rollback_and_exact_retry_preserve_graph_authority)' \
  --no-tests=fail

install_root="$work/install"
echo "distribution acceptance: installing packaged CLI with production features"
cargo install \
  --path "$root_package" \
  --root "$install_root" \
  --no-default-features \
  --features production \
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
if "production" not in manifest.get("features", {}):
    raise SystemExit("packaged tracedecay manifest omitted the production feature")
print("""[package]
name = "tracedecay-distribution-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]""")
print(
    "tracedecay = { path = "
    + json.dumps(sys.argv[2])
    + ', default-features = false, features = ["production"] }'
)
print(
    "tracedecay-tool-catalog = { path = "
    + json.dumps(sys.argv[3])
    + " }"
)
PY
cat >"$consumer/src/main.rs" <<'RS'
use std::collections::BTreeSet;

use tracedecay::agents::host_bundle_registry::{
    RECEIPT_BACKED_HOST_KINDS, default_components, verified_embedded_default_host_component_set,
    verified_embedded_host_bundle,
};
use tracedecay::agents::host_bundle_v2::stock_host_kinds;
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
        .collect::<BTreeSet<_>>();
    let receipt_backed_hosts = RECEIPT_BACKED_HOST_KINDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        supported_hosts,
        receipt_backed_hosts,
        "packaged default host bundle inventory changed"
    );
    for host in RECEIPT_BACKED_HOST_KINDS {
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

test_api_probe="$work/test-api-probe"
mkdir -p -- "$test_api_probe/src"
cat >"$test_api_probe/Cargo.toml" <<TOML
[package]
name = "tracedecay-test-api-probe"
version = "0.0.0"
edition = "2024"

[dependencies]
tracedecay = { path = "$root_package", default-features = false, features = ["production"] }
TOML
cat >"$test_api_probe/src/main.rs" <<'RS'
use tracedecay::mcp::McpServer;

fn main() {
    let _ = McpServer::has_project_session_retrieval_service_for_test;
}
RS
echo "distribution acceptance: proving production package omits test APIs"
test_api_stderr="$work/test-api-probe.stderr"
if CARGO_NET_OFFLINE=true cargo check \
  --manifest-path "$test_api_probe/Cargo.toml" \
  --config "$patch_config" \
  2>"$test_api_stderr"; then
  die "production package exposed test-transport APIs"
fi
grep -Eq "no function or associated item named .*has_project_session_retrieval_service_for_test" \
  "$test_api_stderr" ||
  die "test API probe failed for an unexpected reason"

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
  --no-default-features \
  --features production \
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

binary=$(python3 "$repo/scripts/resolve-installed-binary.py" \
  "$install_root" \
  "${RUNNER_OS:-}")
"$binary" --version

echo "distribution acceptance: exercising installed MCP behavior"
if [[ ${RUNNER_OS:-} == Windows ]]; then
  python3 "$repo/scripts/check-packaged-mcp-stdio.py" \
    "$binary" \
    "$work/mcp-stdio"
else
  TRACEDECAY_BIN="$binary" "$repo/scripts/mcp-conformance-smoke.sh"
fi

lsp_servers="$work/lsp-servers.json"
"$binary" lsp servers --json >"$lsp_servers"
python3 - "$lsp_servers" <<'PY'
import json
import sys
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if not isinstance(value, list) or not value:
    raise SystemExit("distribution acceptance: lsp servers returned an empty inventory")
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
        raise SystemExit("distribution acceptance: invalid lsp server entry")
    languages.add(server["language"])
missing = sorted(required_languages - languages)
if missing:
    raise SystemExit(
        "distribution acceptance: lsp inventory omitted " + ", ".join(missing)
    )
PY

python3 "$repo/scripts/check-packaged-lsp-bridge.py" \
  "$binary" \
  "$work/lsp-bridge"

echo "distribution acceptance passed"
