#!/usr/bin/env bash
# Workspace rustc wrapper. On Darwin, re-signs the tracedecay executable
# after rustc returns.
#
# Cargo's release profile does not set `strip`. When no crate requests
# debuginfo, Cargo passes `-C strip=debuginfo`, and rustc runs
# `rust-objcopy --strip-debug` after the linker. On Mach-O, objcopy
# regenerates the ad-hoc signature from the output filename
# (`deps/tracedecay-<hash>`), so scripts/macos-sign-linker.sh's identifier
# is gone by the time rustc exits. Cargo does not pass `-o` for the bin.
# The executable is `<--out-dir>/<--crate-name><extra-filename>`
# (`extra-filename` already includes the leading hyphen). Cargo hardlinks
# that deps file into `target/release/tracedecay` (or a `cargo install`
# prefix) after this wrapper returns, so the signature on the deps file
# is the one the final binary keeps.
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

# Print rustc arguments, one per line, expanding a leading @response-file.
# Cargo uses that form when the rustc line exceeds ARG_MAX.
expand_rustc_args() {
  local arg file
  for arg in "$@"; do
    if [[ $arg == @* && -f ${arg#@} ]]; then
      file=${arg#@}
      awk '
        function unescape(s,    out, i, c) {
          out = ""
          i = 1
          while (i <= length(s)) {
            c = substr(s, i, 1)
            if (c == "\\" && i < length(s)) {
              i++
              out = out substr(s, i, 1)
            } else {
              out = out c
            }
            i++
          }
          return out
        }
        { print unescape($0) }
      ' "$file"
    else
      printf '%s\n' "$arg"
    fi
  done
}

# Cargo's bin invocation has no -o. When none was passed, the tracedecay
# executable is `<out-dir>/<crate-name><extra-filename>`.
tracedecay_cargo_bin_output() {
  local arg prev="" out_dir="" crate_name="" crate_type="" extra="" saw_o=0
  while IFS= read -r arg; do
    if [[ $prev == -o ]]; then
      saw_o=1
      prev=""
      continue
    fi
    if [[ $prev == --out-dir ]]; then
      out_dir=$arg
      prev=""
      continue
    fi
    if [[ $prev == --crate-name ]]; then
      crate_name=$arg
      prev=""
      continue
    fi
    if [[ $prev == --crate-type ]]; then
      crate_type=$arg
      prev=""
      continue
    fi
    if [[ $prev == -C ]]; then
      case $arg in
        extra-filename=*) extra=${arg#extra-filename=} ;;
      esac
      prev=""
      continue
    fi
    case $arg in
      -o)
        prev=-o
        saw_o=1
        ;;
      -o*)
        saw_o=1
        ;;
      --out-dir)
        prev=--out-dir
        ;;
      --out-dir=*)
        out_dir=${arg#--out-dir=}
        ;;
      --crate-name)
        prev=--crate-name
        ;;
      --crate-name=*)
        crate_name=${arg#--crate-name=}
        ;;
      --crate-type)
        prev=--crate-type
        ;;
      --crate-type=*)
        crate_type=${arg#--crate-type=}
        ;;
      -C)
        prev=-C
        ;;
      -Cextra-filename=*)
        extra=${arg#-Cextra-filename=}
        ;;
    esac
  done < <(expand_rustc_args "$@")
  if [[ $saw_o -eq 1 ]]; then
    return 0
  fi
  if [[ $crate_name == tracedecay && $crate_type == bin && -n $out_dir ]]; then
    printf '%s\n' "${out_dir%/}/${crate_name}${extra}"
  fi
}

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
if [[ ! -s $outputs ]]; then
  tracedecay_cargo_bin_output "$@" >>"$outputs"
fi
while IFS= read -r output; do
  [[ -n $output ]] || continue
  # Metadata-only invocations name an output they never write.
  [[ -f $output ]] || continue
  if tracedecay_link_output_needs_stable_identity "$output"; then
    "$root/scripts/macos-stable-codesign.sh" "$output"
  fi
done <"$outputs"
