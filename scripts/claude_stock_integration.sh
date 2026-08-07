#!/usr/bin/env bash
# End-to-end proof that the TraceDecay Claude Code bundle installs through a
# STOCK Claude Code CLI. TraceDecay stages the marketplace bundle; the stock
# host itself performs marketplace registration, plugin install, enablement,
# and component resolution — exactly the documented operator journey. Used by
# the `claude-integration` CI job and runnable locally:
#
#   npm install --global @anthropic-ai/claude-code@<pinned>
#   cargo build --bin tracedecay
#   scripts/claude_stock_integration.sh
#
# Environment:
#   TRACEDECAY_BIN  tracedecay binary to install/test (default: target/debug/tracedecay)
#   CLAUDE_BIN      stock claude binary (default: claude on PATH)
#
# Everything runs in a throwaway HOME and a throwaway project. No model calls
# and no credentials: marketplace add, plugin install, plugin inventory, and
# the headless doctor are all local host operations.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Global so the EXIT trap can still see it after `main` returns under `set -u`.
stage=""

main() {
    local tracedecay_bin claude_bin
    local fake_home project marketplace
    local stage_output plugin_list details doctor_out

    tracedecay_bin="${TRACEDECAY_BIN:-$REPO_ROOT/target/debug/tracedecay}"
    tracedecay_bin="$(cd "$(dirname "$tracedecay_bin")" && pwd)/$(basename "$tracedecay_bin")"
    claude_bin="${CLAUDE_BIN:-$(command -v claude || true)}"

    if [[ ! -x "$tracedecay_bin" ]]; then
        echo "error: tracedecay binary not found at $tracedecay_bin (build with: cargo build --bin tracedecay)" >&2
        return 1
    fi
    if [[ -z "$claude_bin" || ! -x "$claude_bin" ]]; then
        echo "error: stock claude binary not found (set CLAUDE_BIN or npm install --global @anthropic-ai/claude-code)" >&2
        return 1
    fi

    echo "== stock claude: $claude_bin ($("$claude_bin" --version 2>/dev/null || echo unknown))"
    echo "== tracedecay binary: $tracedecay_bin ($("$tracedecay_bin" --version))"


    stage="$(mktemp -d -t claude-stock-XXXXXX)"
    fake_home="$stage/home"
    project="$stage/project"
    marketplace="$fake_home/.claude/plugins/marketplaces/tracedecay"
    mkdir -p "$fake_home" "$project/src" "$stage/bin"
    trap 'rm -rf "$stage"' EXIT

    # The installer records the PATH-resolved `tracedecay` in host
    # registrations and deliberately refuses transient cargo-target binaries.
    # Shim the binary under test into a neutral directory (the lifecycle
    # acceptance suite's pattern) so an operator's installed release can never
    # satisfy this gate.
    ln "$tracedecay_bin" "$stage/bin/tracedecay" 2>/dev/null \
        || cp "$tracedecay_bin" "$stage/bin/tracedecay"
    chmod 0755 "$stage/bin/tracedecay"
    tracedecay_bin="$stage/bin/tracedecay"
    PATH="$stage/bin:$PATH"
    export PATH
    unset CARGO_TARGET_DIR

    printf 'pub fn add(a: i32, b: i32) -> i32 { a + b }\n' > "$project/src/lib.rs"
    git -C "$project" init -q
    git -C "$project" add -A
    git -C "$project" -c user.email=ci@tracedecay -c user.name=ci commit -qm init

    # Stage the marketplace bundle. Claude Code owns marketplace registration
    # and enablement, so this step deliberately stops with handover guidance —
    # the bundle on disk plus the exact stock commands to run next.
    echo "== tracedecay install --agent claude (stages the marketplace bundle)"
    set +e
    stage_output="$(cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        TRACEDECAY_DATA_DIR="$stage/profile" \
        "$tracedecay_bin" install --agent claude 2>&1)"
    set -e
    echo "$stage_output"
    test -f "$marketplace/.claude-plugin/marketplace.json"
    echo "$stage_output" | grep -q "claude plugin marketplace add" || {
        echo "error: staged install did not hand over to the stock host commands" >&2
        return 1
    }

    echo "== stock claude plugin marketplace add"
    (cd "$project" && HOME="$fake_home" timeout 180 \
        "$claude_bin" plugin marketplace add "$marketplace")

    echo "== stock claude plugin install"
    (cd "$project" && HOME="$fake_home" timeout 180 \
        "$claude_bin" plugin install tracedecay@tracedecay)

    echo "== stock claude plugin list"
    plugin_list="$(cd "$project" && HOME="$fake_home" timeout 180 \
        "$claude_bin" plugin list 2>&1)"
    echo "$plugin_list"
    echo "$plugin_list" | grep -q "tracedecay@tracedecay" || {
        echo "error: stock claude does not list the installed tracedecay plugin" >&2
        return 1
    }
    echo "$plugin_list" | grep -q "enabled" || {
        echo "error: stock claude did not enable the tracedecay plugin" >&2
        return 1
    }

    # The stock host resolves the full component inventory itself: hooks for
    # real edit/stop events, the MCP server, the packaged LSP bridge, agents.
    echo "== stock claude plugin details (component inventory)"
    details="$(cd "$project" && HOME="$fake_home" timeout 180 \
        "$claude_bin" plugin details tracedecay@tracedecay 2>&1)"
    echo "$details"
    for expectation in "PostToolUse" "Stop" "MCP servers (1)" "LSP servers (1)" "Agents (8)"; do
        echo "$details" | grep -qF "$expectation" || {
            echo "error: stock claude inventory is missing: $expectation" >&2
            return 1
        }
    done
    echo "ok - stock claude resolved hooks, MCP, LSP bridge, and agents"

    echo "== stock claude doctor (headless)"
    doctor_out="$(cd "$project" && HOME="$fake_home" timeout 180 "$claude_bin" doctor 2>&1)"
    echo "$doctor_out"
    if echo "$doctor_out" | grep -qi "tracedecay" && echo "$doctor_out" | grep -qiE "error|corrupt|invalid"; then
        echo "error: stock claude doctor reports a tracedecay problem" >&2
        return 1
    fi

    echo "stock claude integration: PASS"
}

main "$@"
