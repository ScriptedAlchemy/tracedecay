#!/usr/bin/env bash
set -euo pipefail

# Stable ad-hoc identifier for local macOS installs. TCC keys removable-volume
# and file grants on it. The linker default is the hashed deps filename
# (`tracedecay-<hash>`). Release strip runs after the link and writes that
# same filename back into the ad-hoc signature, so every rebuild is a new
# app and the daemon blocks in open() until the prompt is answered. This
# runs on the final installed file, after that strip. A Developer ID or
# other team signature is preserved.
TRACEDECAY_MACOS_CODE_SIGN_IDENTIFIER=dev.tracedecay.cli

# Re-sign `target` when it is an unsigned or ad-hoc Mach-O. No-op off Darwin
# and for non-Mach-O files (the Linux installer tests install a shell fixture).
stabilize_macos_adhoc_identity() {
  local target=$1
  if [[ ${TRACEDECAY_ASSUME_DARWIN:-} != 1 && "$(uname -s)" != Darwin ]]; then
    return 0
  fi
  [[ -f $target ]] || {
    printf 'tracedecay installer: cannot sign missing binary %s\n' "$target" >&2
    return 1
  }
  local magic
  magic=$(od -An -t x1 -N 4 "$target" | tr -d ' \n')
  case $magic in
    feedfacf | cffaedfe | feedface | cefaedfe | cafebabe | bebafeca | cafebabf | bfbafeca) ;;
    *) return 0 ;;
  esac
  local report ident team
  report=$(codesign -dv --verbose=2 "$target" 2>&1 || true)
  if printf '%s\n' "$report" | grep -q '^Authority='; then
    return 0
  fi
  team=$(printf '%s\n' "$report" | sed -n 's/^TeamIdentifier=//p' | head -n 1)
  if [[ -n $team && $team != "not set" ]]; then
    return 0
  fi
  ident=$(printf '%s\n' "$report" | sed -n 's/^Identifier=//p' | head -n 1)
  if [[ $ident == "$TRACEDECAY_MACOS_CODE_SIGN_IDENTIFIER" ]]; then
    return 0
  fi
  codesign --force --sign - --identifier "$TRACEDECAY_MACOS_CODE_SIGN_IDENTIFIER" "$target"
}

# The macOS link wrapper sources this file to reuse the function above.
if [[ ${TRACEDECAY_INSTALL_LIBRARY:-} == 1 && ${BASH_SOURCE[0]} != "$0" ]]; then
  return 0
fi

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

# release-beta.yml names prerelease archives `tracedecay-beta-<tag>-...`;
# release.yml names stable ones `tracedecay-<tag>-...`.
asset_name_for_tag() {
  local candidate=$1
  if [[ $candidate == *-beta.* ]]; then
    printf 'tracedecay-beta-%s-%s.tar.gz' "$candidate" "$platform"
  else
    printf 'tracedecay-%s-%s.tar.gz' "$candidate" "$platform"
  fi
}

# True when the releases API payload already lists both the platform archive
# and SHA256SUMS for this tag. Release Please can publish a non-draft
# prerelease before release-beta.yml uploads those assets; matching on
# browser_download_url paths skips that half-published window. The payload is
# far larger than a pipe buffer, so piping it into `grep -q` lets the writer
# die of SIGPIPE and `pipefail` turns a match into 141; grep a here-string.
release_has_install_assets() {
  local json=$1
  local candidate=$2
  local candidate_asset=$3
  grep -Fq -- "/download/${candidate}/${candidate_asset}" <<<"$json" &&
    grep -Fq -- "/download/${candidate}/SHA256SUMS" <<<"$json"
}

# `latest` is the newest published release including prereleases, because the
# 0.1.0 beta line is where tracedecay ships. GitHub's `releases/latest`
# redirect never resolves to a prerelease, so it pinned installs to the last
# stable tag (v0.0.74, 2026-08-19) and the whole beta line was unreachable.
# Walk recent releases and skip any that lack the platform archive + checksum
# (common while a beta build is still uploading). `TRACEDECAY_VERSION=stable`
# opts back into the releases/latest redirect.
case $requested_version in
  latest)
    tag=
    releases_json=$(
      curl -fsSL "https://api.github.com/repos/${repository}/releases?per_page=30"
    )
    while IFS= read -r candidate; do
      [[ -n $candidate ]] || continue
      candidate_asset=$(asset_name_for_tag "$candidate")
      if release_has_install_assets "$releases_json" "$candidate" "$candidate_asset"; then
        tag=$candidate
        break
      fi
    done < <(
      printf '%s' "$releases_json" |
        grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' |
        cut -d'"' -f4
    )
    [[ -n $tag ]] ||
      fail "no published release has install assets for ${platform}"
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

asset=$(asset_name_for_tag "$tag")
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
# After the archive checksum check. Re-signing changes the installed bytes
# only; the published digest still matches the archive.
stabilize_macos_adhoc_identity "${install_dir}/tracedecay"
printf 'Installed tracedecay %s to %s\n' "${tag#v}" "${install_dir}/tracedecay"

case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) printf 'Add %s to PATH to run tracedecay.\n' "$install_dir" ;;
esac
