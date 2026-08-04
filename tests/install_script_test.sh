#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/install.sh"

bash -n "$INSTALLER"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
mkdir -p "$tmpdir/archive" "$tmpdir/bin" "$tmpdir/install"

cat >"$tmpdir/archive/tracedecay" <<'SH'
#!/usr/bin/env bash
printf 'tracedecay 9.8.7\n'
SH
chmod +x "$tmpdir/archive/tracedecay"
tar -czf "$tmpdir/tracedecay-v9.8.7-x86_64-linux.tar.gz" -C "$tmpdir/archive" tracedecay
(
  cd "$tmpdir"
  sha256sum tracedecay-v9.8.7-x86_64-linux.tar.gz >SHA256SUMS
)

cat >"$tmpdir/bin/uname" <<'SH'
#!/usr/bin/env bash
case "${1:-}" in
  -s) printf 'Linux\n' ;;
  -m) printf 'x86_64\n' ;;
  *) printf 'Linux\n' ;;
esac
SH

cat >"$tmpdir/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

output=
url=
while (($#)); do
  case "$1" in
    -o)
      output=$2
      shift 2
      ;;
    http*)
      url=$1
      shift
      ;;
    *)
      shift
      ;;
  esac
done

case "$url" in
  */releases/latest) printf '%s\n' 'https://github.com/ScriptedAlchemy/tracedecay/releases/tag/v9.8.7' ;;
  */SHA256SUMS) cp "$TEST_CHECKSUMS" "$output" ;;
  *.tar.gz) cp "$TEST_ARCHIVE" "$output" ;;
  *) exit 2 ;;
esac
SH
chmod +x "$tmpdir/bin/uname" "$tmpdir/bin/curl"

PATH="$tmpdir/bin:$PATH" \
TRACEDECAY_INSTALL_DIR="$tmpdir/install" \
TEST_ARCHIVE="$tmpdir/tracedecay-v9.8.7-x86_64-linux.tar.gz" \
TEST_CHECKSUMS="$tmpdir/SHA256SUMS" \
  "$INSTALLER"

[[ "$("$tmpdir/install/tracedecay")" == "tracedecay 9.8.7" ]]
