#!/usr/bin/env bash
# Behavior of the stable macOS ad-hoc identity: the install helper, the manual
# signer, the link wrapper, and the post-rustc wrapper. Runs on Linux with a
# mocked codesign. Release strip is a stub that re-signs with the filename.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/install.sh"
LINKER="$ROOT/scripts/macos-sign-linker.sh"
SIGNER="$ROOT/scripts/macos-stable-codesign.sh"
RUSTC_WRAPPER="$ROOT/scripts/macos-rustc-wrapper.sh"

bash -n "$INSTALLER"
bash -n "$LINKER"
bash -n "$SIGNER"
bash -n "$RUSTC_WRAPPER"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

HASH=3d1e6be7cae777a9
IDENTIFIER=dev.tracedecay.cli
# One argv. The `=` marks inline requirement text (`-r='designated => ...'`).
REQUIREMENT_ARG="-r=designated => identifier \"${IDENTIFIER}\""

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
if [[ ${1:-} == -d || ${1:-} == -dv || ${1:-} == --display ]]; then
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
  [[ $line == "--force --sign - --identifier ${IDENTIFIER} ${REQUIREMENT_ARG} "* ]] || {
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

# Already stable: identifier requirement, not a cdhash. CandidateCDHash in the
# verbose display is not the designated requirement.
log=$(sign_with_report "Identifier=${IDENTIFIER}"$'\nSignature=adhoc\nCandidateCDHash sha256=0123456789abcdef\ndesignated => identifier "'"${IDENTIFIER}"$'"\n')
assert_not_signed "$log"

# Stable identifier whose designated requirement is still the binary cdhash.
log=$(sign_with_report "Identifier=${IDENTIFIER}"$'\nSignature=adhoc\nTeamIdentifier=not set\ndesignated => cdhash H"0123456789abcdef0123456789abcdef01234567"\n')
assert_signed "$log"

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

# Release strip re-signs the linked product with its filename, then the
# rustc wrapper signs that same deps file as dev.tracedecay.cli. Cargo
# hardlinks the deps file into target/release after the wrapper returns.
# The fake rustc speaks Cargo's bin argv: --out-dir and -C extra-filename,
# with no -o. It also still honors an explicit -o.
fake_rustc=$tmpdir/fake-rustc
cat >"$fake_rustc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${FAKE_RUSTC_STATUS:-0} -ne 0 ]]; then
  exit "$FAKE_RUSTC_STATUS"
fi

args=()
for arg in "$@"; do
  if [[ $arg == @* && -f ${arg#@} ]]; then
    while IFS= read -r line || [[ -n $line ]]; do
      line=${line%$'\r'}
      args+=("$line")
    done <"${arg#@}"
  else
    args+=("$arg")
  fi
done

prev=
out=
out_dir=
crate_name=
extra=
for arg in "${args[@]}"; do
  if [[ $prev == -o ]]; then
    out=$arg
    prev=
    continue
  fi
  if [[ $prev == --out-dir ]]; then
    out_dir=$arg
    prev=
    continue
  fi
  if [[ $prev == --crate-name ]]; then
    crate_name=$arg
    prev=
    continue
  fi
  if [[ $prev == -C ]]; then
    case $arg in
      extra-filename=*) extra=${arg#extra-filename=} ;;
    esac
    prev=
    continue
  fi
  case $arg in
    -o) prev=-o ;;
    --out-dir) prev=--out-dir ;;
    --out-dir=*) out_dir=${arg#--out-dir=} ;;
    --crate-name) prev=--crate-name ;;
    --crate-name=*) crate_name=${arg#--crate-name=} ;;
    -C) prev=-C ;;
    -Cextra-filename=*) extra=${arg#-Cextra-filename=} ;;
  esac
done

if [[ ${FAKE_RUSTC_SKIP_OUTPUT:-} == 1 ]]; then
  exit 0
fi
if [[ -z $out && -n $out_dir && -n $crate_name ]]; then
  out="${out_dir%/}/${crate_name}${extra}"
fi
[[ -n $out ]] || exit 3
mkdir -p "$(dirname "$out")"
printf '\xcf\xfa\xed\xfe' >"$out"
printf 'rest' >>"$out"
if [[ -n ${FAKE_RUSTC_STRIP:-} ]]; then
  "$FAKE_RUSTC_STRIP" "$out"
fi
EOF
chmod +x "$fake_rustc"

fake_strip=$tmpdir/fake-strip
cat >"$fake_strip" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
base=$(basename -- "$1")
codesign --force --sign - --identifier "$base" "$1"
EOF
chmod +x "$fake_strip"

product=$tmpdir/release/deps/tracedecay-$HASH
mock_codesign "$tmpdir/mock-bin"
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_REPORT=$'Identifier=tracedecay-'"$HASH"$'\nSignature=adhoc\nTeamIdentifier=not set\n' \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" \
  --crate-name tracedecay \
  --crate-type bin \
  --out-dir "$tmpdir/release/deps" \
  -C extra-filename=-"$HASH"
strip_line=$(head -n 1 "$tmpdir/codesign.log")
stable_line=$(tail -n 1 "$tmpdir/codesign.log")
[[ $strip_line == "--force --sign - --identifier tracedecay-${HASH} ${product}" ]]
[[ $stable_line == "--force --sign - --identifier ${IDENTIFIER} ${REQUIREMENT_ARG} ${product}" ]]
[[ $strip_line != "$stable_line" ]]

# Glued spellings: --out-dir= and -Cextra-filename=.
glued=$tmpdir/glued/deps/tracedecay-$HASH
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_REPORT=$'Identifier=tracedecay-'"$HASH"$'\nSignature=adhoc\nTeamIdentifier=not set\n' \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" \
  --crate-name=tracedecay \
  --crate-type=bin \
  --out-dir="$tmpdir/glued/deps" \
  -Cextra-filename=-"$HASH"
strip_line=$(head -n 1 "$tmpdir/codesign.log")
stable_line=$(tail -n 1 "$tmpdir/codesign.log")
[[ $strip_line == "--force --sign - --identifier tracedecay-${HASH} ${glued}" ]]
[[ $stable_line == "--force --sign - --identifier ${IDENTIFIER} ${REQUIREMENT_ARG} ${glued}" ]]

# The same Cargo argv inside an @response-file.
response=$tmpdir/rustc-args
printf '%s\n' \
  --crate-name tracedecay \
  --crate-type bin \
  --out-dir "$tmpdir/response/deps" \
  -C "extra-filename=-${HASH}" \
  >"$response"
responded=$tmpdir/response/deps/tracedecay-$HASH
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_REPORT=$'Identifier=tracedecay-'"$HASH"$'\nSignature=adhoc\nTeamIdentifier=not set\n' \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" "@$response"
strip_line=$(head -n 1 "$tmpdir/codesign.log")
stable_line=$(tail -n 1 "$tmpdir/codesign.log")
[[ $strip_line == "--force --sign - --identifier tracedecay-${HASH} ${responded}" ]]
[[ $stable_line == "--force --sign - --identifier ${IDENTIFIER} ${REQUIREMENT_ARG} ${responded}" ]]

# An explicit -o is still signed.
direct=$tmpdir/direct/tracedecay-$HASH
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_REPORT=$'Identifier=tracedecay-'"$HASH"$'\nSignature=adhoc\nTeamIdentifier=not set\n' \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" -o "$direct"
strip_line=$(head -n 1 "$tmpdir/codesign.log")
stable_line=$(tail -n 1 "$tmpdir/codesign.log")
[[ $strip_line == "--force --sign - --identifier tracedecay-${HASH} ${direct}" ]]
[[ $stable_line == "--force --sign - --identifier ${IDENTIFIER} ${REQUIREMENT_ARG} ${direct}" ]]

# A tracedecay lib, and a different bin, keep the strip identifier.
lib_product=$tmpdir/lib/deps/tracedecay-$HASH
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" \
  --crate-name tracedecay \
  --crate-type lib \
  --out-dir "$tmpdir/lib/deps" \
  -C extra-filename=-"$HASH"
[[ $(cat "$tmpdir/codesign.log") == "--force --sign - --identifier tracedecay-${HASH} ${lib_product}" ]]

other_bin=$tmpdir/other-bin/deps/other-$HASH
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" \
  --crate-name other \
  --crate-type bin \
  --out-dir "$tmpdir/other-bin/deps" \
  -C extra-filename=-"$HASH"
[[ $(cat "$tmpdir/codesign.log") == "--force --sign - --identifier other-${HASH} ${other_bin}" ]]

# A non-product output is left with the strip identifier.
other=$tmpdir/release/deps/libother.dylib
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_REPORT=$'Identifier=libother.dylib\nSignature=adhoc\nTeamIdentifier=not set\n' \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" --crate-name other -o "$other"
[[ $(cat "$tmpdir/codesign.log") == "--force --sign - --identifier libother.dylib ${other}" ]]

# Metadata rustc names an -o it does not write. That must not fail the build.
: >"$tmpdir/codesign.log"
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_SKIP_OUTPUT=1 \
  "$RUSTC_WRAPPER" "$fake_rustc" --emit metadata -o "$tmpdir/metadata/deps/tracedecay-$HASH"
assert_not_signed "$tmpdir/codesign.log"

# rustc's status is the wrapper's status, and a failed compile is not signed.
: >"$tmpdir/codesign.log"
if PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN=1 \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STATUS=9 \
  "$RUSTC_WRAPPER" "$fake_rustc" -o "$product"
then
  echo "failing rustc was treated as success" >&2
  exit 1
else
  status=$?
  [[ $status -eq 9 ]]
fi
assert_not_signed "$tmpdir/codesign.log"

# Off Darwin the wrapper execs rustc and does not sign, even when strip did.
: >"$tmpdir/codesign.log"
off=$tmpdir/off/deps/tracedecay-$HASH
PATH="$tmpdir/mock-bin:$PATH" \
  TRACEDECAY_ASSUME_DARWIN= \
  CODESIGN_LOG="$tmpdir/codesign.log" \
  FAKE_RUSTC_STRIP="$fake_strip" \
  "$RUSTC_WRAPPER" "$fake_rustc" -o "$off"
[[ $(cat "$tmpdir/codesign.log") == "--force --sign - --identifier tracedecay-${HASH} ${off}" ]]

echo "macos stable codesign: ok"
