#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/check-release-drift.sh [--repo PATH] [--release-version VERSION]

Fails when Cargo.toml differs from its published GitHub release. Stable
versions are compared with the latest non-prerelease release; prerelease
versions require their exact published prerelease tag.
EOF
}

repo="."
release_version=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:?missing value for --repo}"
      shift 2
      ;;
    --release-version)
      release_version="${2:?missing value for $1}"
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

if [[ -z "$release_version" ]]; then
  curl_args=(-fsSL -A "tracedecay-release-drift-check")
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    curl_args+=(-H "Authorization: Bearer $GITHUB_TOKEN")
  fi
  release_endpoint="https://api.github.com/repos/ScriptedAlchemy/tracedecay/releases/latest"
  expected_prerelease=false
  if [[ "$local_version" == *-* ]]; then
    release_endpoint="https://api.github.com/repos/ScriptedAlchemy/tracedecay/releases/tags/v${local_version}"
    expected_prerelease=true
  fi
  if ! release_response="$(curl "${curl_args[@]}" "$release_endpoint")"; then
    echo "release drift detected: no published GitHub release v$local_version" >&2
    exit 1
  fi
  release_version="$(python3 - "$local_version" "$expected_prerelease" "$release_response" <<'PY'
import json
import sys

local_version, expected_prerelease, payload = sys.argv[1:]
release = json.loads(payload)
tag = release.get("tag_name")
if not isinstance(tag, str) or not tag:
    raise SystemExit("GitHub release response has no tag_name")
if expected_prerelease == "true":
    if tag != f"v{local_version}":
        raise SystemExit(f"GitHub prerelease tag mismatch: expected v{local_version}, got {tag}")
    if release.get("draft") is not False or release.get("prerelease") is not True:
        raise SystemExit(f"GitHub prerelease v{local_version} is not published")
print(tag)
PY
)"
fi

release_version="${release_version#v}"

comparison="$(python3 - "$local_version" "$release_version" <<'PY'
import sys

def parse(version: str):
    main, sep, pre = version.partition("-")
    parts = tuple(int(part) for part in main.split("."))
    return parts + ((1, "") if not sep else (0, pre))

local = parse(sys.argv[1])
release = parse(sys.argv[2])
if local > release:
    print("ahead")
elif local < release:
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
    echo "release drift detected: local Cargo.toml version $local_version is ahead of GitHub release v$release_version" >&2
    echo "Reset the unpublished release bump so release automation can recreate it, or create GitHub release v$local_version manually before merging more release changes." >&2
    exit 1
    ;;
  behind)
    echo "release drift detected: local Cargo.toml version $local_version is behind GitHub release v$release_version" >&2
    echo "Update the checkout from master before running release automation." >&2
    exit 1
    ;;
esac
