#!/usr/bin/env bash
# Usage: check-dashboard-digests.sh DIR
#
# DIR holds one dashboard-sha256-<target>.txt per release target, each the
# //dashboard:bundle_sha256 digest that target's binary embeds. Every target
# must embed the same bundle.
set -euo pipefail

dir=${1:?usage: check-dashboard-digests.sh DIR}
shopt -s nullglob
expected=""
count=0
for file in "$dir"/dashboard-sha256-*.txt; do
  # Windows Python writes CRLF.
  digest=$(tr -d '\r\n' <"$file")
  if [[ ! $digest =~ ^[0-9a-f]{64}$ ]]; then
    echo "${file##*/}: not a dashboard bundle digest: '${digest}'" >&2
    exit 1
  fi
  if [[ -n "$expected" && "$digest" != "$expected" ]]; then
    echo "release targets embed different dashboard bundles:" >&2
    for reported_file in "$dir"/dashboard-sha256-*.txt; do
      echo "  ${reported_file##*/}: $(tr -d '\r\n' <"$reported_file")" >&2
    done
    exit 1
  fi
  expected="$digest"
  count=$((count + 1))
done
echo "$count release targets embed dashboard ${expected:-none}"
