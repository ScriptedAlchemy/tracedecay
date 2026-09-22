#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/install.sh"

bash -n "$INSTALLER"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
mkdir -p "$tmpdir/archive" "$tmpdir/bin" "$tmpdir/install"

# Default install path resolves the newest *complete* release via the
# releases API, including prereleases. Fixture tag uses the beta prefix
# release-beta.yml publishes.
BETA_TAG=v9.8.7-beta.1
BETA_ASSET="tracedecay-beta-${BETA_TAG}-x86_64-linux.tar.gz"
STABLE_TAG=v9.8.7
STABLE_ASSET="tracedecay-${STABLE_TAG}-x86_64-linux.tar.gz"
# Newer prerelease that Release Please published before assets uploaded.
INCOMPLETE_TAG=v9.8.8-beta.1

stage_release_archive() {
  local version=$1
  local asset=$2
  local sums=$3
  cat >"$tmpdir/archive/tracedecay" <<SH
#!/usr/bin/env bash
printf 'tracedecay ${version}\n'
SH
  chmod +x "$tmpdir/archive/tracedecay"
  tar -czf "$tmpdir/${asset}" -C "$tmpdir/archive" tracedecay
  (
    cd "$tmpdir"
    sha256sum "$asset" >"$sums"
  )
}

stage_release_archive "9.8.7-beta.1" "$BETA_ASSET" SHA256SUMS
stage_release_archive "9.8.7" "$STABLE_ASSET" STABLE_SHA256SUMS

# Incomplete first, complete beta second: default latest must skip the empty
# prerelease and install the beta that already lists archive + SHA256SUMS.
cat >"$tmpdir/releases.json" <<JSON
[
  {
    "tag_name": "${INCOMPLETE_TAG}",
    "prerelease": true,
    "assets": []
  },
  {
    "tag_name": "${BETA_TAG}",
    "prerelease": true,
    "assets": [
      {
        "name": "${BETA_ASSET}",
        "browser_download_url": "https://github.com/ScriptedAlchemy/tracedecay/releases/download/${BETA_TAG}/${BETA_ASSET}"
      },
      {
        "name": "SHA256SUMS",
        "browser_download_url": "https://github.com/ScriptedAlchemy/tracedecay/releases/download/${BETA_TAG}/SHA256SUMS"
      }
    ]
  },
  {
    "tag_name": "${STABLE_TAG}",
    "prerelease": false,
    "assets": [
      {
        "name": "${STABLE_ASSET}",
        "browser_download_url": "https://github.com/ScriptedAlchemy/tracedecay/releases/download/${STABLE_TAG}/${STABLE_ASSET}"
      },
      {
        "name": "SHA256SUMS",
        "browser_download_url": "https://github.com/ScriptedAlchemy/tracedecay/releases/download/${STABLE_TAG}/SHA256SUMS"
      }
    ]
  }
]
JSON

cat >"$tmpdir/bin/uname" <<'SH'
#!/usr/bin/env bash
case "${1:-}" in
  -s) printf 'Linux\n' ;;
  -m) printf 'x86_64\n' ;;
  *) printf 'Linux\n' ;;
esac
SH

cat >"$tmpdir/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

output=
url=
write_out=
while (($#)); do
  case "$1" in
    -o)
      output=$2
      shift 2
      ;;
    -w)
      write_out=$2
      shift 2
      ;;
    http*)
      url=$1
      shift
      ;;
    *)
      shift
      ;;
  esac
done

case "$url" in
  */api.github.com/repos/*/releases*)
    cat "$TEST_RELEASES_JSON"
    ;;
  */releases/latest)
    if [[ -n $write_out ]]; then
      printf '%s' 'https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v9.8.7'
    else
      printf '%s\n' 'https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v9.8.7'
    fi
    ;;
  */download/v9.8.7-beta.1/SHA256SUMS)
    cp "$TEST_CHECKSUMS" "$output"
    ;;
  */download/v9.8.7/SHA256SUMS)
    cp "$TEST_STABLE_CHECKSUMS" "$output"
    ;;
  */download/v9.8.7-beta.1/*.tar.gz)
    cp "$TEST_ARCHIVE" "$output"
    ;;
  */download/v9.8.7/*.tar.gz)
    cp "$TEST_STABLE_ARCHIVE" "$output"
    ;;
  *)
    exit 2
    ;;
esac
SH
chmod +x "$tmpdir/bin/uname" "$tmpdir/bin/curl"

run_installer() {
  PATH="$tmpdir/bin:$PATH" \
  TRACEDECAY_INSTALL_DIR="$tmpdir/install" \
  TEST_RELEASES_JSON="$tmpdir/releases.json" \
  TEST_ARCHIVE="$tmpdir/${BETA_ASSET}" \
  TEST_CHECKSUMS="${INSTALLER_CHECKSUMS:-$tmpdir/SHA256SUMS}" \
  TEST_STABLE_ARCHIVE="$tmpdir/${STABLE_ASSET}" \
  TEST_STABLE_CHECKSUMS="$tmpdir/STABLE_SHA256SUMS" \
    "$@"
}

rm -rf "$tmpdir/install"
mkdir -p "$tmpdir/install"
run_installer "$INSTALLER"
[[ "$("$tmpdir/install/tracedecay")" == "tracedecay 9.8.7-beta.1" ]]
[[ "$(ls -A "$tmpdir/install")" == "tracedecay" ]]

rm -rf "$tmpdir/install"
mkdir -p "$tmpdir/install"
run_installer env TRACEDECAY_VERSION=stable "$INSTALLER"
[[ "$("$tmpdir/install/tracedecay")" == "tracedecay 9.8.7" ]]

expect_installer_failure() {
  local checksums=$1
  local expected_message=$2
  local output="$tmpdir/installer-failure.log"
  if INSTALLER_CHECKSUMS="$checksums" run_installer "$INSTALLER" >"$output" 2>&1
  then
    echo "installer unexpectedly accepted invalid release inputs" >&2
    exit 1
  fi
  grep -Fq "$expected_message" "$output"
}

printf '%064d  %s\n' 0 "$BETA_ASSET" \
  >"$tmpdir/mismatched-SHA256SUMS"
expect_installer_failure \
  "$tmpdir/mismatched-SHA256SUMS" \
  "checksum mismatch for ${BETA_ASSET}"

{
  cat "$tmpdir/SHA256SUMS"
  cat "$tmpdir/SHA256SUMS"
} >"$tmpdir/duplicate-SHA256SUMS"
expect_installer_failure \
  "$tmpdir/duplicate-SHA256SUMS" \
  "SHA256SUMS must contain exactly one entry for ${BETA_ASSET}"

printf 'not-a-digest  %s\n' "$BETA_ASSET" \
  >"$tmpdir/invalid-SHA256SUMS"
expect_installer_failure \
  "$tmpdir/invalid-SHA256SUMS" \
  "SHA256SUMS has an invalid digest for ${BETA_ASSET}"
