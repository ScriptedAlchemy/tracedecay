#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
gate="$root/scripts/check-distribution-acceptance.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-reuse-test.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

repo="$work/repo"
bin="$work/bin"
state="$work/state"
mkdir -p -- \
  "$repo/crates/tracedecay" \
  "$repo/crates/tracedecay-cli" \
  "$repo/crates/tracedecay-hooks/fixtures/host_events" \
  "$repo/tests/fixtures/packaged_host_events" \
  "$repo/.cargo" \
  "$repo/plugin" \
  "$repo/vendor" \
  "$repo/benchmark_data" \
  "$repo/dashboard/hermes-wrapper" \
  "$repo/dashboard/app-dist" \
  "$repo/scripts" \
  "$bin" \
  "$state"

cat >"$repo/Cargo.toml" <<'TOML'
[workspace]
members = ["crates/tracedecay", "crates/tracedecay-cli"]

[workspace.package]
version = "0.0.0"
TOML
cat >"$repo/crates/tracedecay/Cargo.toml" <<'TOML'
[package]
name = "tracedecay"
version = "0.0.0"
readme = "../../README.md"
TOML
cat >"$repo/crates/tracedecay-cli/Cargo.toml" <<'TOML'
[package]
name = "tracedecay-cli"
version = "0.0.0"
TOML

for fixture in \
  claude.json \
  claude/post_tool_use_write.json \
  codex.json \
  cline-family.json \
  cursor.json \
  hermes.json \
  hermes/saved-edit.json \
  hermes/terminal-receipt.json \
  kiro.json \
  kimi-code.json \
  kimi/post-tool-use-edit.json \
  opencode/baseline.json; do
  mkdir -p -- \
    "$repo/crates/tracedecay-hooks/fixtures/host_events/$(dirname -- "$fixture")" \
    "$repo/tests/fixtures/packaged_host_events/$(dirname -- "$fixture")"
  printf 'fixture\n' >"$repo/crates/tracedecay-hooks/fixtures/host_events/$fixture"
  printf 'fixture\n' >"$repo/tests/fixtures/packaged_host_events/$fixture"
done

printf 'original readme\n' >"$repo/README.md"
printf 'changelog\n' >"$repo/CHANGELOG.md"
printf 'license\n' >"$repo/LICENSE"
printf 'stable\n' >"$repo/rust-toolchain.toml"
printf '[net]\noffline = true\n' >"$repo/.cargo/config.toml"
printf 'plugin\n' >"$repo/plugin/fixture"
printf 'vendor\n' >"$repo/vendor/fixture"
printf 'benchmark\n' >"$repo/benchmark_data/fixture"
printf 'wrapper\n' >"$repo/dashboard/hermes-wrapper/fixture"
printf 'bundle\n' >"$repo/dashboard/app-dist/fixture"
printf '#!/usr/bin/env bash\n' >"$repo/scripts/run-session-temporal-benchmark.sh"

git -C "$repo" init -q
git -C "$repo" config user.name "TraceDecay test"
git -C "$repo" config user.email "test@tracedecay.local"
git -C "$repo" add -A
git -C "$repo" commit -qm "test fixture"
source_sha=$(git -C "$repo" rev-parse HEAD)

real_python=$(command -v python3)

cat >"$bin/tracedecay" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [[ \${1:-} == --version ]]; then
  printf 'tracedecay 0.0.0+%s\n' "$source_sha"
  exit 0
fi
if [[ \${1:-} == --help ]]; then
  exit 0
fi
exit 2
SH
chmod +x "$bin/tracedecay"

cat >"$bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$TEST_STATE/cargo.log"
case "${1:-}" in
  build)
    if [[ " $* " == *" --workspace "* ]]; then
      printf 'workspace-rebuild\n' >>"$TEST_STATE/cargo.log"
      exit 3
    fi
    exit 0
    ;;
  package)
    exit 77
    ;;
  *)
    exit 2
    ;;
esac
SH
cat >"$bin/rustc" <<'SH'
#!/usr/bin/env bash
if [[ ${1:-} == -vV ]]; then
  printf 'rustc 1.0.0\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\n'
  exit 0
fi
exit 2
SH
cat >"$bin/python3" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  */resolve-release-source-profile.py)
    output=
    while (($#)); do
      if [[ $1 == --github-output ]]; then
        output=$2
        break
      fi
      shift
    done
    printf 'profile=production\ncargo_features=production\n' >"$output"
    ;;
  *)
    exec "$REAL_PYTHON" "$@"
    ;;
esac
SH
chmod +x "$bin/cargo" "$bin/rustc" "$bin/python3"

run_gate() {
  local output=$1
  shift
  set +e
  PATH="$bin:$PATH" \
    REAL_PYTHON="$real_python" \
    TEST_STATE="$state" \
    TMPDIR="$work" \
    "$gate" --repo "$repo" "$@" >"$output" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status"
}

output="$work/output"

: >"$state/cargo.log"
status=$(run_gate "$output" --skip-packaged-runtime-battery)
[[ $status -ne 0 ]] || {
  cat "$output" >&2
  echo "skip-packaged-runtime-battery without a reused binary was accepted" >&2
  exit 1
}
grep -Fq -- "--skip-packaged-runtime-battery requires --reuse-release-binary" "$output" || {
  cat "$output" >&2
  echo "missing reuse-binary requirement was not reported" >&2
  exit 1
}
if [[ -s $state/cargo.log ]]; then
  echo "cargo ran before the reuse-binary requirement failed" >&2
  exit 1
fi

# Without a handed-in binary the gate goes straight to packaging: the
# packaged CLI it builds later is the production binary under test, so a
# source-tree workspace release build here would be a second compile of the
# same graph that nothing reads.
: >"$state/cargo.log"
status=$(run_gate "$output")
[[ $status -eq 77 ]] || {
  cat "$output" >&2
  echo "default path did not stop at the controlled cargo package boundary" >&2
  exit 1
}
if grep -Fxq "workspace-rebuild" "$state/cargo.log"; then
  echo "default path invoked a source-tree workspace release build" >&2
  exit 1
fi
if grep -Eq '^build( |$)' "$state/cargo.log"; then
  echo "default path invoked cargo build before packaging" >&2
  exit 1
fi
if grep -Fq "reusing the just-built production binary" "$output"; then
  cat "$output" >&2
  echo "default path claimed to reuse a binary it was never given" >&2
  exit 1
fi

: >"$state/cargo.log"
status=$(run_gate "$output" --reuse-release-binary "$bin/tracedecay")
[[ $status -eq 77 ]] || {
  cat "$output" >&2
  echo "reuse path did not stop at the controlled cargo package boundary" >&2
  exit 1
}
grep -Fq "distribution acceptance: reusing the just-built production binary" "$output" || {
  cat "$output" >&2
  echo "reuse path did not announce the just-built binary" >&2
  exit 1
}
if grep -Fxq "workspace-rebuild" "$state/cargo.log"; then
  echo "reuse path still invoked the workspace release rebuild" >&2
  exit 1
fi
grep -Eq '^package( |$)' "$state/cargo.log" || {
  echo "reuse path skipped cargo package" >&2
  exit 1
}

printf 'wrong version\n' >"$bin/bad-tracedecay"
chmod +x "$bin/bad-tracedecay"
cat >"$bin/bad-tracedecay" <<'SH'
#!/usr/bin/env bash
if [[ ${1:-} == --version ]]; then
  printf 'tracedecay 9.9.9+deadbeef\n'
  exit 0
fi
exit 2
SH
chmod +x "$bin/bad-tracedecay"
: >"$state/cargo.log"
status=$(run_gate "$output" --reuse-release-binary "$bin/bad-tracedecay")
[[ $status -ne 0 ]] || {
  cat "$output" >&2
  echo "reuse path accepted a binary with the wrong source sha" >&2
  exit 1
}
grep -Fq "reused-release tracedecay binary reported" "$output" || {
  cat "$output" >&2
  echo "wrong reused binary failed for an unexpected reason" >&2
  exit 1
}
if [[ -s $state/cargo.log ]]; then
  echo "cargo ran after a reused binary failed its source-sha check" >&2
  exit 1
fi

printf 'distribution reuse-binary regression passed\n'
