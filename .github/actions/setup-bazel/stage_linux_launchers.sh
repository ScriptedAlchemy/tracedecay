#!/usr/bin/env bash
set -euo pipefail

# These public API IDs identify the binaries-20261025 release used by
# hermetic_launcher. Bazel checks its upstream SHA-256 before accepting a
# distdir file and uses the original release URL when the file is absent.
distdir=${1:?expected a runner-private distdir}
api=https://api.github.com/repos/hermeticbuild/hermetic-launcher/releases/assets

stage_asset() {
  local id=$1 name=$2
  if ! curl --fail --silent --show-error --location --remove-on-error \
      --connect-timeout 5 --max-time 30 --header 'Accept: application/octet-stream' \
      --output "$distdir/$name" "$api/$id"; then
    printf '::notice::Release API unavailable for %s; Bazel will use its configured source.\n' "$name"
  fi
}

stage_asset 674976726 finalize-stub-x86_64-linux
stage_asset 674976728 runfiles-stub-x86_64-linux
