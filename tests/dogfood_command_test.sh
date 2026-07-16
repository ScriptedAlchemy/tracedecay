#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fake_target="$fixture/target"
fake_home="$fixture/home"
fake_bin="$fixture/bin"
mkdir -p "$fake_target/release" "$fake_home" "$fake_bin"

write_fake_binary() {
  local path=$1
  local binary_id=$2
  {
    printf '#!/usr/bin/env bash\n'
    printf 'binary_id=%q\n' "$binary_id"
    cat <<'EOF'
command=$*
printf '%s:%s\n' "$binary_id" "$command" >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"
if [[ "${1:-}" == "--version" ]]; then
  printf 'tracedecay 0.0.0-dogfood\n'
fi
if [[ "$binary_id" == "${TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY:-}" \
  && "$command" == "${TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND:-}" ]]; then
  exit 42
fi
if [[ "$binary_id" == "${TRACEDECAY_DOGFOOD_TEST_HOLD_BINARY:-}" \
  && "$command" == "${TRACEDECAY_DOGFOOD_TEST_HOLD_COMMAND:-}" ]]; then
  : >"${TRACEDECAY_DOGFOOD_TEST_HOLD_MARKER:?}"
  while [[ ! -e "${TRACEDECAY_DOGFOOD_TEST_HOLD_RELEASE:?}" ]]; do
    sleep 0.02
  done
fi
EOF
  } >"$path"
  chmod +x "$path"
}

write_fake_binary "$fake_target/release/tracedecay" new

cat >"$fake_bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"
EOF
chmod +x "$fake_bin/cargo"

clean_path="$fake_bin:/usr/bin:/bin"
log="$fixture/actions.log"
PATH="$clean_path" \
HOME="$fake_home" \
CARGO_TARGET_DIR="$fake_target" \
TRACEDECAY_DOGFOOD_TEST_LOG="$log" \
  "$repo_root/scripts/dogfood.sh"

staged="$fake_home/.local/lib/tracedecay/dogfood/tracedecay"
installed="$fake_home/.local/bin/tracedecay"
test -x "$staged"
test -x "$installed"
cmp "$fake_target/release/tracedecay" "$staged"
cmp "$staged" "$installed"
test "$(grep -v '^cargo ' "$log")" = $'new:post-update --strict\nnew:doctor\nnew:--version'
grep -Fxq 'cargo build --release --all-features --bin tracedecay' "$log"
if grep -Eq 'daemon (restart|status)' "$log"; then
  echo 'dogfood script bypassed post-update daemon restoration' >&2
  exit 1
fi

write_fake_binary "$installed" old-installed
write_fake_binary "$staged" old-staged
cp "$installed" "$fixture/previous-installed"
cp "$staged" "$fixture/previous-staged"
: >"$log"
if PATH="$clean_path" \
  HOME="$fake_home" \
  CARGO_TARGET_DIR="$fake_target" \
  TRACEDECAY_DOGFOOD_TEST_LOG="$log" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND=doctor \
    "$repo_root/scripts/dogfood.sh"; then
  echo 'dogfood unexpectedly succeeded after doctor failure' >&2
  exit 1
fi
cmp "$fixture/previous-installed" "$installed"
cmp "$fixture/previous-staged" "$staged"
test "$(grep -v '^cargo ' "$log")" = \
  $'new:post-update --strict\nnew:doctor\nold-installed:post-update --strict'

rm -f "$installed" "$staged"
: >"$log"
if PATH="$clean_path" \
  HOME="$fake_home" \
  CARGO_TARGET_DIR="$fake_target" \
  TRACEDECAY_DOGFOOD_TEST_LOG="$log" \
  TRACEDECAY_DOGFOOD_TEST_FAIL_BINARY=new \
  TRACEDECAY_DOGFOOD_TEST_FAIL_COMMAND='post-update --strict' \
    "$repo_root/scripts/dogfood.sh"; then
  echo 'dogfood unexpectedly succeeded after post-update failure' >&2
  exit 1
fi
test ! -e "$installed"
test ! -e "$staged"
test "$(grep -v '^cargo ' "$log")" = \
  $'new:post-update --strict\nnew:uninstall\nnew:daemon uninstall-service'

: >"$log"
hold_marker="$fixture/hold-entered"
hold_release="$fixture/hold-release"
PATH="$clean_path" \
HOME="$fake_home" \
CARGO_TARGET_DIR="$fake_target" \
TRACEDECAY_DOGFOOD_TEST_LOG="$log" \
TRACEDECAY_DOGFOOD_TEST_HOLD_BINARY=new \
TRACEDECAY_DOGFOOD_TEST_HOLD_COMMAND='post-update --strict' \
TRACEDECAY_DOGFOOD_TEST_HOLD_MARKER="$hold_marker" \
TRACEDECAY_DOGFOOD_TEST_HOLD_RELEASE="$hold_release" \
  "$repo_root/scripts/dogfood.sh" &
dogfood_pid=$!
for _ in $(seq 1 100); do
  [[ -e "$hold_marker" ]] && break
  sleep 0.02
done
test -e "$hold_marker"
if flock -n "$fake_home/.tracedecay/dogfood.lock" true; then
  echo 'dogfood profile lock was released during validation' >&2
  kill "$dogfood_pid" || true
  exit 1
fi
touch "$hold_release"
wait "$dogfood_pid"
test "$(grep -v '^cargo ' "$log")" = $'new:post-update --strict\nnew:doctor\nnew:--version'

grep -Fq 'dogfood = "run --quiet --release --all-features --bin tracedecay -- dogfood"' "$repo_root/.cargo/config.toml"

echo 'dogfood command contract passed'
