#!/usr/bin/env bash
# End-to-end proof that `tracedecay install --agent opencode` works against a
# STOCK OpenCode CLI — the host's own config loader must accept every file the
# installer writes, and the host's own MCP client must negotiate a session
# with `tracedecay serve`. Used by the `opencode-integration` CI job and
# runnable locally:
#
#   npm install --global opencode-ai@<pinned>
#   cargo build --bin tracedecay
#   scripts/opencode_stock_integration.sh
#
# Environment:
#   TRACEDECAY_BIN  tracedecay binary to install/test (default: target/debug/tracedecay)
#   OPENCODE_BIN    stock opencode binary (default: opencode on PATH)
#
# Everything runs in a throwaway HOME and a throwaway initialized project; no
# model calls and no credentials. `opencode debug config` is the host's own
# strict loader: an invalid agent, command, skill, LSP, or MCP file we install
# fails the whole configuration here exactly as it does for a real user.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_PATH="$REPO_ROOT/scripts/opencode_stock_integration.sh"
DAEMON_HARNESS="$REPO_ROOT/scripts/with-isolated-tracedecay-daemon.sh"
STAGE=""

resolve_stock_opencode_bin() {
    local candidate="$1"
    local isolated_home="$2"
    local operator_home="$3"
    local candidate_version isolated_version payload payload_version

    candidate_version="$("$candidate" --version 2>/dev/null)" || {
        echo "error: stock opencode launcher failed: $candidate" >&2
        return 1
    }
    if isolated_version="$(HOME="$isolated_home" XDG_CONFIG_HOME="$isolated_home/.config" \
        "$candidate" --version 2>/dev/null)" \
        && [[ "$isolated_version" == "$candidate_version" ]]; then
        printf '%s\n' "$candidate"
        return 0
    fi

    # The official installer keeps its executable here. A PATH entry may be
    # an operator launcher that uses HOME to find this payload, which cannot
    # work after the stock journey isolates HOME. Require exact host-version
    # identity before bypassing such a launcher.
    payload="$operator_home/.opencode/bin/opencode"
    if [[ -x "$payload" ]]; then
        payload_version="$("$payload" --version 2>/dev/null)" || true
        if [[ "$payload_version" == "$candidate_version" ]]; then
            printf '%s\n' "$payload"
            return 0
        fi
    fi

    echo "error: $candidate cannot run under an isolated HOME; set OPENCODE_BIN to the stock executable payload" >&2
    return 1
}

run_mcp_probe() {
    # Runs under the isolated-daemon harness: TRACEDECAY_DATA_DIR and
    # TRACEDECAY_DAEMON_SOCKET point at the temporary sole-owner daemon, so the
    # `tracedecay serve` process the stock host spawns can reach it.
    local project="$1"
    local mcp_list

    echo "== stock opencode mcp list (real MCP handshake against tracedecay serve)"
    mcp_list="$(cd "$project" && COLUMNS=200 timeout 180 "$OPENCODE_BIN" mcp list 2>&1)"
    echo "$mcp_list"
    echo "$mcp_list" | grep -q "tracedecay" || {
        echo "error: stock opencode does not list the tracedecay MCP server" >&2
        return 1
    }
    if echo "$mcp_list" | grep "tracedecay" -A 2 | grep -qi "failed"; then
        echo "error: stock opencode failed to negotiate MCP with tracedecay serve" >&2
        return 1
    fi
    echo "$mcp_list" | grep -qi "connected" || {
        echo "error: stock opencode did not report the tracedecay MCP server connected" >&2
        return 1
    }
    echo "ok - stock opencode negotiated MCP with tracedecay serve"
    echo "stock opencode integration: PASS"
}

main() {
    local tracedecay_bin opencode_bin opencode_candidate operator_home
    local fake_home project config status

    tracedecay_bin="${TRACEDECAY_BIN:-$REPO_ROOT/target/debug/tracedecay}"
    tracedecay_bin="$(cd "$(dirname "$tracedecay_bin")" && pwd)/$(basename "$tracedecay_bin")"
    opencode_candidate="${OPENCODE_BIN:-$(command -v opencode || true)}"
    operator_home="$HOME"

    if [[ ! -x "$tracedecay_bin" ]]; then
        echo "error: tracedecay binary not found at $tracedecay_bin (build with: cargo build --bin tracedecay)" >&2
        return 1
    fi
    if [[ -z "$opencode_candidate" || ! -x "$opencode_candidate" ]]; then
        echo "error: stock opencode binary not found (set OPENCODE_BIN or npm install --global opencode-ai)" >&2
        return 1
    fi

    STAGE="$(mktemp -d -t opencode-stock-XXXXXX)"
    fake_home="$STAGE/home"
    project="$STAGE/project"
    mkdir -p "$fake_home" "$project/src" "$STAGE/bin"
    trap 'rm -rf "$STAGE"' EXIT
    opencode_bin="$(resolve_stock_opencode_bin \
        "$opencode_candidate" "$fake_home" "$operator_home")"

    echo "== stock opencode: $opencode_bin ($("$opencode_bin" --version))"
    echo "== tracedecay binary: $tracedecay_bin ($("$tracedecay_bin" --version))"

    # The installer records the PATH-resolved `tracedecay` in the host
    # registration and deliberately refuses transient cargo-target binaries.
    # Shim the binary under test into a neutral directory (the lifecycle
    # acceptance suite's pattern) so an operator's installed release can never
    # satisfy this gate.
    ln "$tracedecay_bin" "$STAGE/bin/tracedecay" 2>/dev/null \
        || cp "$tracedecay_bin" "$STAGE/bin/tracedecay"
    chmod 0755 "$STAGE/bin/tracedecay"
    tracedecay_bin="$STAGE/bin/tracedecay"
    PATH="$STAGE/bin:$PATH"
    export PATH
    unset CARGO_TARGET_DIR

    printf 'pub fn add(a: i32, b: i32) -> i32 { a + b }\n' > "$project/src/lib.rs"
    printf '[package]\nname = "throwaway"\nversion = "0.1.0"\nedition = "2021"\n' > "$project/Cargo.toml"
    git -C "$project" init -q
    git -C "$project" add -A
    git -C "$project" -c user.email=ci@tracedecay -c user.name=ci commit -qm init

    echo "== tracedecay install --agent opencode"
    (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        TRACEDECAY_DATA_DIR="$STAGE/profile" \
        "$tracedecay_bin" install --agent opencode)
    config="$fake_home/.config/opencode/opencode.json"
    test -f "$config"
    test -f "$fake_home/.config/opencode/plugins/tracedecay.ts"

    echo "== stock opencode debug config (host-owned strict loader)"
    (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        timeout 180 "$opencode_bin" debug config) > "$STAGE/resolved-config.json"
    python3 - "$STAGE/resolved-config.json" <<'EOF'
import json
import sys

with open(sys.argv[1], "rb") as handle:
    config = json.load(handle)

mcp = config.get("mcp", {}).get("tracedecay")
assert mcp and mcp.get("type") == "local", f"tracedecay MCP registration missing: {mcp!r}"
assert any("tracedecay" in part for part in mcp.get("command", [])), mcp

lsp = config.get("lsp", {}).get("tracedecay")
assert lsp, "tracedecay LSP registration missing from the resolved config"
initialization = lsp.get("initialization", {}).get("tracedecay", {})
assert initialization.get("duplicateAnalyzerAvoidance") is True, initialization
assert "retainedByExtension" in initialization.get("analyzerOwnership", {}), initialization

agents = config.get("agent", {})
assert "code-explorer" in agents, (
    "installed agent definitions were not accepted by the stock host: "
    f"{sorted(agents)}"
)
print("ok - stock opencode accepted the full installed configuration")
print(f"ok - {len(agents)} agents loaded; duplicateAnalyzerAvoidance engaged")
EOF

    set +e
    HOME="$fake_home" \
        XDG_CONFIG_HOME="$fake_home/.config" \
        OPENCODE_BIN="$opencode_bin" \
        "$DAEMON_HARNESS" --bin "$tracedecay_bin" --ready-timeout 30 \
        --lifecycle-label "temporary tracedecay daemon" -- \
        "$SCRIPT_PATH" --run "$project"
    status=$?
    set -e
    return "$status"
}

if [[ "${1:-}" == "--run" ]]; then
    shift
    run_mcp_probe "$@"
else
    main "$@"
fi
