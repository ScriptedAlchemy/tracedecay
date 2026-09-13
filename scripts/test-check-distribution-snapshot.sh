#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
gate="$root/scripts/check-distribution-acceptance.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-snapshot-test.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

repo="$work/repo"
bin="$work/bin"
state="$work/state"
mkdir -p -- \
  "$repo/crates/tracedecay-semantic/src/model_lifecycle" \
  "$repo/crates/tracedecay" \
  "$repo/crates/tracedecay-cli" \
  "$repo/crates/tracedecay-hooks/fixtures/host_events" \
  "$repo/tests/distribution/fastembed" \
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
cat >"$repo/crates/tracedecay-semantic/src/model_lifecycle.rs" <<'RS'
#[cfg(all(test, feature = "semantic-fastembed"))]
#[path = "model_lifecycle/distribution_acquisition_acceptance.rs"]
mod distribution_acquisition_acceptance;
RS
cat >"$repo/crates/tracedecay-semantic/src/model_lifecycle/distribution_acquisition_acceptance.rs" <<'RS'
#[test]
#[ignore = "distribution gate owns this fixture"]
fn fixture_is_acquired_by_distribution_gate() {}
RS
touch \
  "$repo/tests/distribution/fastembed/prepare_fixture.py" \
  "$repo/tests/distribution/fastembed/validate_fixture.py"

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

real_cp=$(command -v cp)
real_python=$(command -v python3)

cat >"$bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  build) exit 0 ;;
  package) exit 77 ;;
  *) exit 2 ;;
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
  */prepare_fixture.py)
    fixture=${@: -1}
    mkdir -p -- "$fixture"
    for required in fixture.json model.onnx tokenizer.json config.json \
      special_tokens_map.json tokenizer_config.json; do
      printf 'fixture\n' >"$fixture/$required"
    done
    ;;
  */validate_fixture.py)
    printf '1\t1\n'
    ;;
  *)
    exec "$REAL_PYTHON" "$@"
    ;;
esac
SH
cat >"$bin/cp" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ ! -e "$TEST_STATE/mutated" ]]; then
  : >"$TEST_STATE/mutated"
  printf 'mutated live readme\n' >"$TEST_REPO/README.md"
fi
exec "$REAL_CP" "$@"
SH
chmod +x "$bin/cargo" "$bin/rustc" "$bin/python3" "$bin/cp"

output="$work/output"
set +e
PATH="$bin:$PATH" \
REAL_CP="$real_cp" \
REAL_PYTHON="$real_python" \
TEST_REPO="$repo" \
TEST_STATE="$state" \
TMPDIR="$work" \
  "$gate" --repo "$repo" --keep-temp >"$output" 2>&1
status=$?
set -e
[[ $status -eq 77 ]] || {
  cat "$output" >&2
  echo "distribution gate stopped before the controlled cargo package boundary" >&2
  exit 1
}

distribution_work=$(printf '%s\n' "$work"/tracedecay-distribution.*)
staged="$distribution_work/staged"
grep -Fxq "original readme" "$staged/crates/tracedecay/README.md" || {
  echo "packaged asset was copied from the mutated live repository" >&2
  exit 1
}
grep -Fxq "mutated live readme" "$repo/README.md"

printf 'distribution staged-snapshot regression passed\n'
