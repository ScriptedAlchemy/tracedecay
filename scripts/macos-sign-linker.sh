#!/usr/bin/env bash
# Link driver for Apple targets. Forwards to cc (or TRACEDECAY_MACOS_LD), then
# gives the tracedecay executable a stable ad-hoc codesign identifier.
#
# rustc sometimes passes a single @response-file when the link line exceeds
# ARG_MAX. The product binary's -o path is in that file, named
# tracedecay-<16 hex> under deps/.
set -euo pipefail

# True for the product binary and the hashed deps filename cargo links it as.
tracedecay_link_output_needs_stable_identity() {
  local base hex16
  base=$(basename -- "$1")
  hex16='[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'
  case $base in
    tracedecay | tracedecay-$hex16) return 0 ;;
    *) return 1 ;;
  esac
}

# Print every -o path from the linker argv, expanding @response-files.
collect_link_outputs() {
  local prev="" arg file
  prev=""
  for arg in "$@"; do
    if [[ $prev == -o ]]; then
      printf '%s\n' "$arg"
      prev=""
      continue
    fi
    case $arg in
      -o)
        prev=-o
        ;;
      -o*)
        printf '%s\n' "${arg#-o}"
        ;;
      @*)
        file=${arg#@}
        [[ -f $file ]] || continue
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
          {
            arg = unescape($0)
            if (prev == 1) {
              print arg
              prev = 0
              next
            }
            if (arg == "-o") {
              prev = 1
              next
            }
            if (substr(arg, 1, 2) == "-o" && length(arg) > 2) {
              print substr(arg, 3)
            }
          }
        ' "$file"
        ;;
    esac
  done
}

if [[ ${TRACEDECAY_LINKER_LIBRARY:-} == 1 && ${BASH_SOURCE[0]} != "$0" ]]; then
  return 0
fi

real_linker=${TRACEDECAY_MACOS_LD:-cc}
"$real_linker" "$@"

root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
TRACEDECAY_INSTALL_LIBRARY=1
# shellcheck source=../install.sh
source "$root/install.sh"
unset TRACEDECAY_INSTALL_LIBRARY

outputs=$(mktemp)
trap 'rm -f "$outputs"' EXIT
collect_link_outputs "$@" >"$outputs"
while IFS= read -r output; do
  [[ -n $output ]] || continue
  if tracedecay_link_output_needs_stable_identity "$output"; then
    stabilize_macos_adhoc_identity "$output" || exit $?
  fi
done <"$outputs"
