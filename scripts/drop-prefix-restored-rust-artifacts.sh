#!/usr/bin/env bash
# Drop compiled artifacts after a rust-cache prefix restore.
#
# rust-cache's restore key omits the lockfile hash, so a miss on the exact
# key still unpacks another generation's target/. rustc writes .rmeta as
# mode 0444; a later compile of the same crate hash then fails with
# "failed to write ... .rmeta". The cargo registry from the restore stays
# — only the incompatible compiler outputs are discarded. Never chmod.
set -euo pipefail

cache_hit="${1-}"
target_dir="${2:-target}"

if [ "$cache_hit" = "true" ]; then
  echo "exact rust-cache hit; keeping ${target_dir}"
  exit 0
fi

if [ -e "$target_dir" ]; then
  echo "dropping prefix-restored ${target_dir} (cache-hit=${cache_hit:-empty})"
  rm -rf -- "$target_dir"
else
  echo "no ${target_dir} to drop (cache-hit=${cache_hit:-empty})"
fi
