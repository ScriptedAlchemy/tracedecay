#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fake_target="$fixture/target"
fake_home="$fixture/home"
fake_bin="$fixture/bin"
mkdir -p "$fake_target/release" "$fake_home" "$fake_bin"

cat >"$fake_target/release/tracedecay" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"
if [[ "${1:-}" == "--version" ]]; then
  printf 'tracedecay 0.0.0-dogfood\n'
fi
EOF
chmod +x "$fake_target/release/tracedecay"

cat >"$fake_bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >>"${TRACEDECAY_DOGFOOD_TEST_LOG:?}"
EOF
chmod +x "$fake_bin/cargo"

log="$fixture/actions.log"
PATH="$fake_bin:$PATH" \
HOME="$fake_home" \
CARGO_TARGET_DIR="$fake_target" \
TRACEDECAY_DOGFOOD_TEST_LOG="$log" \
TRACEDECAY_DOGFOOD_SKIP_SERVICE_MANAGER=1 \
  "$repo_root/scripts/dogfood.sh"

staged="$fake_home/.local/lib/tracedecay/dogfood/tracedecay"
installed="$fake_home/.local/bin/tracedecay"
test -x "$staged"
test -x "$installed"
cmp "$fake_target/release/tracedecay" "$staged"
cmp "$staged" "$installed"

grep -Fxq 'cargo build --locked --release --bin tracedecay' "$log"
grep -Fxq 'post-update' "$log"
grep -Fxq 'daemon status' "$log"
grep -Fxq 'doctor' "$log"
grep -Fxq -- '--version' "$log"

grep -Fq 'dogfood = "run --quiet --release --bin tracedecay -- dogfood"' "$repo_root/.cargo/config.toml"

echo "dogfood command contract passed"
