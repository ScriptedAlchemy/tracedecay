#!/usr/bin/env bash
# Workspace rustc wrapper. On Darwin, re-signs the tracedecay executable
# after rustc returns.
#
# Cargo's release profile does not set `strip`. When no crate requests
# debuginfo, Cargo passes `-C strip=debuginfo`, and rustc runs
# `rust-objcopy --strip-debug` after the linker. On Mach-O, objcopy
# regenerates the ad-hoc signature from the output filename
# (`deps/tracedecay-<hash>`), so scripts/macos-sign-linker.sh's identifier
# is gone by the time rustc exits. Cargo then copies that file to
# `target/release/tracedecay` or a `cargo install` prefix. Signing here,
# on the rustc `-o` path, is what those copies keep.
#
# Off Darwin this execs rustc and does nothing else. Release jobs do not
# Apple-sign; a Developer ID signature is left untouched.
set -euo pipefail

if [[ $# -lt 1 ]]; then
  printf 'usage: %s <rustc> [args...]\n' "$0" >&2
  exit 2
fi

real=$1
shift

# Bash sets OSTYPE. Skip the uname process on the Linux path that every
# workspace rustc invocation takes.
if [[ ${TRACEDECAY_ASSUME_DARWIN:-} != 1 && $OSTYPE != darwin* ]]; then
  exec "$real" "$@"
fi

"$real" "$@"

root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
TRACEDECAY_LINKER_LIBRARY=1
# shellcheck source=macos-sign-linker.sh
source "$root/scripts/macos-sign-linker.sh"
unset TRACEDECAY_LINKER_LIBRARY

outputs=$(mktemp)
trap 'rm -f "$outputs"' EXIT
collect_link_outputs "$@" >"$outputs"
while IFS= read -r output; do
  [[ -n $output ]] || continue
  # Metadata-only rustc invocations name an `-o` they never write.
  [[ -f $output ]] || continue
  if tracedecay_link_output_needs_stable_identity "$output"; then
    "$root/scripts/macos-stable-codesign.sh" "$output"
  fi
done <"$outputs"
