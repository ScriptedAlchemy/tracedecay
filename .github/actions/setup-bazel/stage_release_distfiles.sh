#!/usr/bin/env bash
set -euo pipefail

# Bazel accepts a staged file only when it matches the checksum in the
# dependency rule or registry and otherwise tries the configured source.
distdir=${1:?expected a runner-private distdir}

stage_asset() {
  local repo=$1 id=$2 name=$3
  if ! curl --fail --silent --show-error --location --remove-on-error \
      --connect-timeout 5 --max-time 30 --header 'Accept: application/octet-stream' \
      --output "$distdir/$name" "https://api.github.com/repos/$repo/releases/assets/$id"; then
    printf '::notice::Release API unavailable for %s; Bazel will use its configured source.\n' "$name"
  fi
}

case "$RUNNER_OS" in
  Linux)
    # hermetic_launcher release binaries-20261025, for Linux x86-64 tools.
    stage_asset hermeticbuild/hermetic-launcher 674976726 finalize-stub-x86_64-linux
    stage_asset hermeticbuild/hermetic-launcher 674976728 runfiles-stub-x86_64-linux
    ;;
  Windows)
    # rules_python 2.4.6, using the archive basename from its BCR source.json.
    stage_asset bazel-contrib/rules_python 690529045 rules_python-2.4.6.tar.gz
    ;;
esac
