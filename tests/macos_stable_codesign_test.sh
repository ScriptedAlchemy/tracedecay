#!/usr/bin/env bash
# Behavior of the stable macOS ad-hoc identity: the install helper, the manual
# signer, and the link wrapper. Runs on Linux with a mocked codesign.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/install.sh"
LINKER="$ROOT/scripts/macos-sign-linker.sh"
SIGNER="$ROOT/scripts/macos-stable-codesign.sh"

bash -n "$INSTALLER"
bash -n "$LINKER"
bash -n "$SIGNER"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

HASH=3d1e6be7cae777a9
IDENTIFIER=dev.tracedecay.cli

# Thin arm64 Mach-O magic. codesign is mocked, so the rest of the file is padding.
write_macho() {
  local path=$1
  printf '\xcf\xfa\xed\xfe' >"$path"
  printf 'rest' >>"$path"
}

mock_codesign() {
  local bin=$1
  mkdir -p "$bin"
  cat >"$bin/codesign" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == -dv ]]; then
  printf '%s\n' "${CODESIGN_REPORT:-}"
  exit "${CODESIGN_DV_STATUS:-0}"
fi
printf '%s\n' "$*" >>"${CODESIGN_LOG:?}"
SH
  chmod +x "$bin/codesign"
}

assert_signed() {
  local log=$1
  [[ -f $log ]] || {
    echo "expected codesign --force and the log is missing" >&2
    exit 1
  }
  local line
  line=$(cat "$log")
  [[ $line == "--force --sign - --identifier ${IDENTIFIER} "* ]] || {
    echo "unexpected codesign invocation: $line" >&2
    exit 1
  }
}

assert_not_signed() {
  local log=$1
  if [[ -s $log ]]; then
    echo "codesign --force ran: $(cat "$log")" >&2
    exit 1
  fi
}

sign_with_report() {
  local report=$1
  local status=${2:-0}
  local target=$tmpdir/bin.macho
  local log=$tmpdir/codesign.log
  local bin=$tmpdir/mock-bin
  write_macho "$target"
  mock_codesign "$bin"
  : >"$log"
  PATH="$bin:$PATH" \
    TRACEDECAY_ASSUME_DARWIN=1 \
    CODESIGN_REPORT="$report" \
    CODESIGN_DV_STATUS="$status" \
    CODESIGN_LOG="$log" \
    "$SIGNER" "$target"
  printf '%s' "$log"
}

# Hashed ad-hoc identity is replaced.
log=$(sign_with_report $'Identifier=tracedecay-'"$HASH"$'\nSignature=adhoc\nTeamIdentifier=not set\n')
assert_signed "$log"

# Unsigned.
log=$(sign_with_report "code object is not signed at all" 1)
assert_signed "$log"

# Already stable.
log=$(sign_with_report "Identifier=${IDENTIFIER}"$'\nSignature=adhoc\n')
assert_not_signed "$log"

# Developer ID.
log=$(sign_with_report $'Identifier=tracedecay-'"$HASH"$'\nAuthority=Developer ID Application: Example (TEAMID1234)\nTeamIdentifier=TEAMID1234\n')
assert_not_signed "$log"

# Team id without an Authority line.
log=$(sign_with_report $'Identifier=com.example.legacy\nTeamIdentifier=TEAMID1234\n')
assert_not_signed "$log"

# Off Darwin, do not invoke codesign even when it would fail.
write_macho "$tmpdir/linux.macho"
if TRACEDECAY_ASSUME_DARWIN= \
  PATH="$tmpdir/missing-bin:$PATH" \
  "$SIGNER" "$tmpdir/linux.macho"
then
  :
else
  echo "non-Darwin signing should be a no-op" >&2
  exit 1
fi

# A shell fixture is not Mach-O.
printf '#!/bin/sh\n' >"$tmpdir/script-bin"
chmod +x "$tmpdir/script-bin"
mock_codesign "$tmpdir/mock-bin"
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  "$SIGNER" "$tmpdir/script-bin"
assert_not_signed "$tmpdir/codesign.log"

# Missing binary is an error on Darwin.
if TRACEDECAY_ASSUME_DARWIN=1 "$SIGNER" "$tmpdir/does-not-exist" 2>"$tmpdir/missing.err"; then
  echo "missing binary was signed" >&2
  exit 1
fi
grep -Fq "cannot sign missing binary" "$tmpdir/missing.err"

# Linker output filter.
TRACEDECAY_LINKER_LIBRARY=1
# shellcheck source=../scripts/macos-sign-linker.sh
source "$LINKER"
unset TRACEDECAY_LINKER_LIBRARY

needs() {
  if tracedecay_link_output_needs_stable_identity "$1"; then
    echo yes
  else
    echo no
  fi
}
[[ $(needs "$tmpdir/tracedecay") == yes ]]
[[ $(needs "$tmpdir/deps/tracedecay-$HASH") == yes ]]
[[ $(needs "$tmpdir/deps/tracedecay-${HASH%?}") == no ]]
[[ $(needs "$tmpdir/deps/tracedecay-host-cli-fixture") == no ]]
[[ $(needs "$tmpdir/deps/libtracedecay-$HASH.dylib") == no ]]

# Response-file -o, including a backslash-escaped space, and a direct -o.
mkdir -p "$tmpdir/with space"
response=$tmpdir/linker-arguments
target="$tmpdir/with space/tracedecay-$HASH"
escaped=${target//\\/\\\\}
escaped=${escaped// /\\ }
printf '%s\n' -arch arm64 -o "$escaped" "-o${target}-attached" >"$response"
collected=$(collect_link_outputs "@$response" -o "$tmpdir/direct/tracedecay")
printf '%s\n' "$collected" >"$tmpdir/collected.txt"
grep -Fxq "$tmpdir/with space/tracedecay-$HASH" "$tmpdir/collected.txt"
grep -Fxq "$tmpdir/with space/tracedecay-$HASH-attached" "$tmpdir/collected.txt"
grep -Fxq "$tmpdir/direct/tracedecay" "$tmpdir/collected.txt"

# The wrapper forwards to the real linker, then signs only the product output.
cat >"$tmpdir/t.c" <<'EOF'
int main(void) { return 0; }
EOF
"$LINKER" -o "$tmpdir/not-tracedecay" "$tmpdir/t.c" 
# TRACEDECAY_MACOS_LD defaults to cc. This host's cc can compile the fixture.
test -x "$tmpdir/not-tracedecay"
"$tmpdir/not-tracedecay"

fake=$tmpdir/fake-ld
cat >"$fake" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$fake"
product=$tmpdir/out/tracedecay-$HASH
mkdir -p "$(dirname "$product")"
write_macho "$product"
other=$tmpdir/out/libother.dylib
write_macho "$other"
mock_codesign "$tmpdir/mock-bin"
: >"$tmpdir/codesign.log"
response=$tmpdir/product-args
printf -- '-o\n%s\n-o%s\n' "$product" "$other" >"$response"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_MACOS_LD="$fake" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_REPORT=$'Identifier=tracedecay-'"$HASH"$'\nSignature=adhoc\nTeamIdentifier=not set\n' \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  "$LINKER" "@$response"
assert_signed "$tmpdir/codesign.log"
grep -Fq -- "$product" "$tmpdir/codesign.log"
if grep -Fq -- "$other" "$tmpdir/codesign.log"; then
  echo "signed a non-product output" >&2
  exit 1
fi

echo "macos stable codesign: ok"
