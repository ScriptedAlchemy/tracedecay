#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/check-release-drift.sh [--repo PATH] [--registry-version VERSION]

Fails when Cargo.toml is ahead of crates.io. That state means a release-plz
version bump reached master without the crate publish/tag/release completing.
EOF
}

repo="."
registry_version=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:?missing value for --repo}"
      shift 2
      ;;
    --registry-version)
      registry_version="${2:?missing value for --registry-version}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

cargo_toml="$repo/Cargo.toml"
local_version="$(python3 - "$cargo_toml" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    manifest = tomllib.load(handle)
print(manifest["package"]["version"])
PY
)"

if [[ -z "$registry_version" ]]; then
  registry_version="$(curl -fsSL \
    -A "tracedecay-release-drift-check" \
    https://crates.io/api/v1/crates/tracedecay \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["crate"]["max_version"])')"
fi

comparison="$(python3 - "$local_version" "$registry_version" <<'PY'
import sys

def parse(version: str):
    main, sep, pre = version.partition("-")
    parts = tuple(int(part) for part in main.split("."))
    return parts + ((1, "") if not sep else (0, pre))

local = parse(sys.argv[1])
registry = parse(sys.argv[2])
if local > registry:
    print("ahead")
elif local < registry:
    print("behind")
else:
    print("equal")
PY
)"

case "$comparison" in
  equal)
    echo "release versions are aligned: $local_version"
    ;;
  ahead)
    echo "release drift detected: local Cargo.toml version $local_version is ahead of crates.io $registry_version" >&2
    echo "Reset the unpublished release bump so release-plz can recreate it, or publish $local_version manually before merging more release changes." >&2
    exit 1
    ;;
  behind)
    echo "release drift detected: local Cargo.toml version $local_version is behind crates.io $registry_version" >&2
    echo "Update the checkout from master before running release automation." >&2
    exit 1
    ;;
esac
