#!/usr/bin/env bash
# Resolve the tracedecay binary an eval runs against and print its absolute path
# on stdout. Every eval harness (evals/agent_adoption, evals/hermetic,
# evals/memory) calls this, so they all execute the shipping target
# //crates/tracedecay-cli:tracedecay with the same features and dependency
# patches the release builds.
#
# Default: build that target with Bazel. A failed build is an error; no older
# binary is ever substituted. TRACEDECAY_BIN selects an already-built artifact
# (for example a release binary to compare against); it must exist and be
# executable.
#
# Env:
#   TRACEDECAY_BIN                  explicit binary; skips the build
#   TRACEDECAY_EVAL_BAZEL_CONFIG    Bazel config to build with (default: release;
#                                   `dist` adds thin LTO and symbol stripping to
#                                   build the exact release bytes)
#
# The selected artifact's version and sha256 go to stderr.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="//crates/tracedecay-cli:tracedecay"
config="${TRACEDECAY_EVAL_BAZEL_CONFIG:-release}"
build_cmd="bazel build --config=$config $target"

fail() {
  echo "error: $*" >&2
  exit 2
}

# MSYS `pwd -W` keeps the path usable by native Windows callers.
absolute() {
  echo "$(cd "$(dirname "$1")" && { pwd -W 2>/dev/null || pwd; })/$(basename "$1")"
}

if [[ -n "${TRACEDECAY_BIN:-}" ]]; then
  [[ -f "$TRACEDECAY_BIN" && -x "$TRACEDECAY_BIN" ]] ||
    fail "TRACEDECAY_BIN=$TRACEDECAY_BIN is not an executable file; unset it to build the default with: $build_cmd"
  bin="$(absolute "$TRACEDECAY_BIN")"
  origin="explicit TRACEDECAY_BIN"
else
  command -v bazel >/dev/null 2>&1 ||
    fail "bazel is required to build the eval binary (run: $build_cmd) or set TRACEDECAY_BIN"
  echo "building eval binary: $build_cmd" >&2
  (cd "$repo_root" && bazel build --config="$config" "$target") >&2 ||
    fail "eval binary build failed; not reusing any older binary. Reproduce with: $build_cmd"
  bazel_bin="$(cd "$repo_root" && bazel info --config="$config" bazel-bin)" ||
    fail "could not resolve the Bazel output directory (bazel info --config=$config bazel-bin)"
  exe=""
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) exe=".exe" ;;
  esac
  bin="$bazel_bin/crates/tracedecay-cli/tracedecay${exe}"
  [[ -f "$bin" && -x "$bin" ]] ||
    fail "expected a binary at $bin after: $build_cmd"
  bin="$(absolute "$bin")"
  origin="bazel --config=$config $target"
fi

if command -v sha256sum >/dev/null 2>&1; then
  digest="$(sha256sum "$bin")"
else
  digest="$(shasum -a 256 "$bin")"
fi
version="$("$bin" --version 2>/dev/null | head -1)" || version=unknown
echo "eval binary: $bin ($origin; ${version:-unknown}; sha256 ${digest%% *})" >&2
printf '%s\n' "$bin"
