# Shared preamble for the stock-host integration scripts
# (claude/opencode/hermes_stock_integration.sh). Source it, do not execute it:
#
#     . "$(dirname "${BASH_SOURCE[0]}")/lib/stock-host.sh"
#
# It centralizes the binary-under-test resolution and the neutral-directory
# shim whose subtleties (absolutization, PATH precedence, CARGO_TARGET_DIR)
# each stock journey was open-coding.
#
# shellcheck shell=bash

STOCK_HOST_REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
readonly STOCK_HOST_REPO_ROOT

# Path of the shimmed binary set by the most recent `shim_tracedecay_bin`.
STOCK_HOST_TRACEDECAY_BIN=""

# Prints the absolutized tracedecay binary under test ($TRACEDECAY_BIN, or the
# default cargo debug output) and requires it to be executable.
resolve_tracedecay_bin() {
    local bin
    bin="${TRACEDECAY_BIN:-$STOCK_HOST_REPO_ROOT/target/debug/tracedecay}"
    bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
    if [[ ! -x "$bin" ]]; then
        echo "error: tracedecay binary not found at $bin (build with: cargo build -p tracedecay-cli --bin tracedecay)" >&2
        return 1
    fi
    printf '%s\n' "$bin"
}

# shim_tracedecay_bin BIN STAGE_DIR
#
# The installer records the PATH-resolved `tracedecay` in host registrations
# and deliberately refuses transient cargo-target binaries. Shim the binary
# under test into a neutral directory (the lifecycle acceptance suite's
# pattern) so an operator's installed release can never satisfy this gate.
#
# Mutates the calling shell: puts STAGE_DIR/bin first on PATH, unsets
# CARGO_TARGET_DIR, and records the shimmed path in
# STOCK_HOST_TRACEDECAY_BIN (a function cannot both print and mutate PATH,
# because command substitution would confine the mutation to a subshell).
shim_tracedecay_bin() {
    local bin="$1"
    local stage="$2"
    mkdir -p "$stage/bin"
    ln "$bin" "$stage/bin/tracedecay" 2>/dev/null \
        || cp "$bin" "$stage/bin/tracedecay"
    chmod 0755 "$stage/bin/tracedecay"
    PATH="$stage/bin:$PATH"
    export PATH
    unset CARGO_TARGET_DIR
    STOCK_HOST_TRACEDECAY_BIN="$stage/bin/tracedecay"
}

# seed_throwaway_project DIR
#
# Turns DIR — into which the caller has already written its source files —
# into a single-commit throwaway git project.
seed_throwaway_project() {
    local project="$1"
    git -C "$project" init -q
    git -C "$project" add -A
    git -C "$project" -c user.email=ci@tracedecay -c user.name=ci commit -qm init
}
