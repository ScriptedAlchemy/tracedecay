#!/usr/bin/env bash
# Ad-hoc sign one tracedecay executable as dev.tracedecay.cli.
#
# Used when a build bypasses scripts/macos-sign-linker.sh
# (CARGO_TARGET_<triple>_LINKER). install.sh and `tracedecay update` share
# this policy. A Developer ID or other team signature is left unchanged.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  printf 'usage: %s <tracedecay-binary>\n' "$0" >&2
  exit 2
fi

root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
TRACEDECAY_INSTALL_LIBRARY=1
# shellcheck source=../install.sh
source "$root/install.sh"
unset TRACEDECAY_INSTALL_LIBRARY

stabilize_macos_adhoc_identity "$1"
