#!/usr/bin/env bash
set -euo pipefail

# Bazel accepts a staged file only when it matches the checksum in the
# dependency rule or registry and otherwise tries the configured source.
distdir=${1:?expected a runner-private distdir}

stage_asset() {
  local repo=$1 id=$2 name=$3
  # Hosted runners share unauthenticated API quotas. Read the job token only
  # for the fixed GitHub API origin. Curl drops Authorization on a redirect
  # to the release storage host, and the header stays off the command line.
  local api_header=''
  if [[ -n ${GH_TOKEN:-} ]]; then
    api_header="Authorization: Bearer $GH_TOKEN"
  fi
  if ! curl --proto '=https' --proto-redir '=https' --header @- --fail --silent --show-error --location --remove-on-error \
      --connect-timeout 5 --max-time 30 --header 'Accept: application/octet-stream' \
      --output "$distdir/$name" "https://api.github.com/repos/$repo/releases/assets/$id" <<< "$api_header"; then
    printf '::notice::Release API unavailable for %s; Bazel will use its configured source.\n' "$name"
  fi
}

# The same rules_python archive is required by Bazel on every host.
stage_asset bazel-contrib/rules_python 744869973 rules_python-2.5.1.tar.gz

if [[ "$RUNNER_OS" == Linux && "$RUNNER_ARCH" == X64 ]]; then
  # hermetic_launcher release binaries-20261025, for Linux x86-64 tools.
  stage_asset hermeticbuild/hermetic-launcher 674976726 finalize-stub-x86_64-linux
  stage_asset hermeticbuild/hermetic-launcher 674976728 runfiles-stub-x86_64-linux
fi
