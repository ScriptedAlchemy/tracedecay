#!/usr/bin/env bash
set -euo pipefail

case_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
run_root="${1:-$(mktemp -d /tmp/tracedecay-vector-ab.XXXXXX)}"
mkdir -p "$run_root/home" "$run_root/cargo" "$run_root/target" "$run_root/data" "$run_root/artifacts"

manifest="$case_dir/Cargo.toml"
binary="$run_root/target/release/vector-publication-ab"
time_output="$run_root/artifacts/build.time"

env HOME="$run_root/home" CARGO_HOME="$run_root/cargo" cargo fetch --locked --manifest-path "$manifest"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) protoc_package='protoc-bin-vendored-linux-x86_64-*' ;;
  Linux-aarch64) protoc_package='protoc-bin-vendored-linux-aarch_64-*' ;;
  Darwin-x86_64) protoc_package='protoc-bin-vendored-macos-x86_64-*' ;;
  Darwin-arm64) protoc_package='protoc-bin-vendored-macos-aarch_64-*' ;;
  *) echo "unsupported benchmark platform: $(uname -s)-$(uname -m)" >&2; exit 2 ;;
esac
protoc="$(find "$run_root/cargo/registry/src" -type f -path "*/$protoc_package/bin/protoc" -perm -111 | head -1)"
test -n "$protoc"

/usr/bin/time -f '%e %M' -o "$time_output" \
  env HOME="$run_root/home" CARGO_HOME="$run_root/cargo" CARGO_TARGET_DIR="$run_root/target" PROTOC="$protoc" \
  cargo build --locked --release --manifest-path "$manifest"

"$binary" build-metrics "$time_output" "$binary" "$run_root/artifacts/build.json"
"$binary" sqlite "$run_root/data/sqlite" "$run_root/artifacts/sqlite.json"
"$binary" lance "$run_root/data/lance" "$run_root/artifacts/lance.json"
"$binary" compare \
  "$run_root/artifacts/sqlite.json" \
  "$run_root/artifacts/lance.json" \
  "$run_root/artifacts/build.json" \
  "$run_root/artifacts/comparison.json"

echo "$run_root/artifacts/comparison.json"
