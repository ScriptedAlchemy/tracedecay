#!/usr/bin/env bash
set -euo pipefail

repository=${TRACEDECAY_REPOSITORY:-ScriptedAlchemy/tracedecay}
install_dir=${TRACEDECAY_INSTALL_DIR:-${XDG_BIN_HOME:-${HOME}/.local/bin}}
requested_version=${TRACEDECAY_VERSION:-latest}
release_root="https://github.com/${repository}/releases"

fail() {
  printf 'tracedecay installer: %s\n' "$*" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v install >/dev/null 2>&1 || fail "install is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64)
    platform=x86_64-linux
    ;;
  Linux/aarch64 | Linux/arm64)
    platform=aarch64-linux
    ;;
  Darwin/arm64 | Darwin/aarch64)
    platform=aarch64-macos
    ;;
  *)
    fail "unsupported platform: $(uname -s) $(uname -m)"
    ;;
esac

# `latest` is the newest published release including prereleases, because the
# 0.1.0 beta line is where tracedecay ships. GitHub's `releases/latest`
# redirect never resolves to a prerelease, so it pinned installs to the last
# stable tag (v0.0.74, 2026-08-19) and the whole beta line was unreachable.
# `TRACEDECAY_VERSION=stable` opts back into that redirect.
case $requested_version in
  latest)
    tag=$(
      curl -fsSL "https://api.github.com/repos/${repository}/releases?per_page=1" |
        grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' |
        head -n 1 |
        cut -d'"' -f4
    )
    ;;
  stable)
    resolved_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "${release_root}/latest")
    tag=${resolved_url##*/}
    ;;
  *)
    tag="v${requested_version#v}"
    ;;
esac
[[ $tag == v* ]] || fail "GitHub did not return a valid release tag"

# release-beta.yml names prerelease archives `tracedecay-beta-<tag>-...`;
# release.yml names stable ones `tracedecay-<tag>-...`.
if [[ $tag == *-beta.* ]]; then
  asset="tracedecay-beta-${tag}-${platform}.tar.gz"
else
  asset="tracedecay-${tag}-${platform}.tar.gz"
fi
asset_root="${release_root}/download/${tag}"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

curl -fsSL "${asset_root}/${asset}" -o "${tmp_dir}/${asset}"
curl -fsSL "${asset_root}/SHA256SUMS" -o "${tmp_dir}/SHA256SUMS"

if ! expected=$(
  awk -v asset="$asset" '
    $2 == asset || $2 == "*" asset {
      matches += 1
      digest = $1
      fields = NF
    }
    END {
      if (matches != 1 || fields != 2) {
        exit 1
      }
      print digest
    }
  ' "${tmp_dir}/SHA256SUMS"
); then
  fail "SHA256SUMS must contain exactly one entry for ${asset}"
fi
[[ $expected =~ ^[[:xdigit:]]{64}$ ]] ||
  fail "SHA256SUMS has an invalid digest for ${asset}"
expected=$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')

if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "${tmp_dir}/${asset}" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "${tmp_dir}/${asset}" | awk '{print $1}')
else
  fail "sha256sum or shasum is required"
fi
[[ $actual == "$expected" ]] || fail "checksum mismatch for ${asset}"

tar -xzf "${tmp_dir}/${asset}" -C "$tmp_dir"
[[ -f ${tmp_dir}/tracedecay ]] || fail "archive does not contain tracedecay"

mkdir -p "$install_dir"
install -m 0755 "${tmp_dir}/tracedecay" "${install_dir}/tracedecay"
printf 'Installed tracedecay %s to %s\n' "${tag#v}" "${install_dir}/tracedecay"

case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) printf 'Add %s to PATH to run tracedecay.\n' "$install_dir" ;;
esac
