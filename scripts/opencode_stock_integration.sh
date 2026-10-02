#!/usr/bin/env bash
# End-to-end proof that `tracedecay install --agent opencode` works against a
# STOCK OpenCode 2 CLI: the host's own loader must accept every file the
# installer writes, the host must load both deployed V2 plugins, and the
# host's own MCP client must negotiate a session with `tracedecay serve`.
# Used by the `host-stock-integration` CI job and runnable locally:
#
#   npm install --global @opencode/cli@<pinned>
#   cargo build -p tracedecay-cli --bin tracedecay
#   scripts/opencode_stock_integration.sh
#
# Environment:
#   TRACEDECAY_BIN  tracedecay binary to install/test (default: target/debug/tracedecay)
#   OPENCODE_BIN    stock opencode binary (default: opencode on PATH)
#
# Everything runs in a throwaway HOME and a throwaway initialized project; no
# model calls and no credentials. OpenCode 2 serves every CLI command from a
# background service keyed on the (isolated) HOME, so the journey stops that
# service on exit. `opencode debug agents` and `opencode plugin list` are the
# host's own strict loader: an invalid agent, command, skill, plugin, or MCP
# file we install fails here exactly as it does for a real user.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO_ROOT/scripts/lib/stock-host.sh"
SCRIPT_PATH="$REPO_ROOT/scripts/opencode_stock_integration.sh"
DAEMON_HARNESS="$REPO_ROOT/scripts/with-isolated-tracedecay-daemon.sh"
STAGE=""
FAKE_HOME=""

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

stop_stock_service() {
    # The background service inherits the isolated HOME, so this stops only
    # the service this journey started.
    [[ -n "$1" ]] || return 0
    HOME="$1" XDG_CONFIG_HOME="$1/.config" timeout 30 "$OPENCODE_BIN" service stop >/dev/null 2>&1 || true
}

free_loopback_port() {
    python3 - <<'EOF'
import socket
with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    print(probe.getsockname()[1])
EOF
}

run_mcp_probe() {
    # Runs under the isolated-daemon harness: TRACEDECAY_DATA_DIR and
    # TRACEDECAY_DAEMON_SOCKET point at the temporary sole-owner daemon, so the
    # `tracedecay serve` process the stock host spawns can reach it. The stock
    # service is restarted here so its MCP client inherits that environment.
    local project="$1"
    local mcp_list

    stop_stock_service "$HOME"
    echo "== stock opencode mcp list (real MCP handshake against tracedecay serve)"
    # The restarted service answers "No MCP servers configured" until it has
    # booted the project location and connected the configured servers.
    local deadline=$((SECONDS + 180))
    while true; do
        mcp_list="$(cd "$project" && COLUMNS=200 timeout 180 "$OPENCODE_BIN" mcp list 2>&1)"
        if echo "$mcp_list" | grep -q "tracedecay" && ! echo "$mcp_list" | grep -qi "connecting"; then
            break
        fi
        if (( SECONDS >= deadline )); then
            break
        fi
        sleep 2
    done
    stop_stock_service "$HOME"
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

    tracedecay_bin="$(resolve_tracedecay_bin)"
    opencode_candidate="${OPENCODE_BIN:-$(command -v opencode || true)}"
    operator_home="$HOME"

    if [[ -z "$opencode_candidate" || ! -x "$opencode_candidate" ]]; then
        echo "error: stock opencode binary not found (set OPENCODE_BIN or npm install --global @opencode/cli)" >&2
        return 1
    fi

    STAGE="$(mktemp -d -t opencode-stock-XXXXXX)"
    fake_home="$STAGE/home"
    project="$STAGE/project"
    mkdir -p "$fake_home" "$project/src"
    opencode_bin="$(resolve_stock_opencode_bin \
        "$opencode_candidate" "$fake_home" "$operator_home")"
    OPENCODE_BIN="$opencode_bin"
    FAKE_HOME="$fake_home"
    trap 'stop_stock_service "$FAKE_HOME"; rm -rf "$STAGE"' EXIT

    echo "== stock opencode: $opencode_bin ($("$opencode_bin" --version))"
    echo "== tracedecay binary: $tracedecay_bin ($("$tracedecay_bin" --version))"

    shim_tracedecay_bin "$tracedecay_bin" "$STAGE"
    tracedecay_bin="$STOCK_HOST_TRACEDECAY_BIN"

    printf 'pub fn add(a: i32, b: i32) -> i32 { a + b }\n' > "$project/src/lib.rs"
    printf '[package]\nname = "throwaway"\nversion = "0.1.0"\nedition = "2021"\n' > "$project/Cargo.toml"
    seed_throwaway_project "$project"

    echo "== tracedecay install --agent opencode"
    (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        TRACEDECAY_DATA_DIR="$STAGE/profile" \
        "$tracedecay_bin" install --agent opencode)
    config="$fake_home/.config/opencode/opencode.json"
    test -f "$config"
    test -f "$fake_home/.config/opencode/plugins/tracedecay.ts"
    test -f "$fake_home/.config/opencode/plugins/tracedecay-mcp.ts"

    # OpenCode 2 binds its background service to one fixed loopback port per
    # HOME; the isolated HOME must not collide with an operator's own service
    # on this machine, so give it a free port before the first start.
    (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        timeout 60 "$opencode_bin" service set port "$(free_loopback_port)")
    # OpenCode's first run in a fresh HOME starts its background service,
    # boots the project location, and loads its plugins; `debug agents`
    # answers `[]` until that boot completes. Warm that one-time cost under
    # its own generous budget so the strict loader checks below time the
    # loader, not the service start.
    echo "== stock opencode first-run warm-up (service start, untimed check)"
    local warm_deadline=$((SECONDS + 600))
    until (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        timeout 120 "$opencode_bin" debug agents 2>/dev/null) | grep -q '"id"'; do
        if (( SECONDS >= warm_deadline )); then
            echo "warning: opencode did not finish its first-run boot in time; the strict loader below is authoritative" >&2
            break
        fi
        sleep 2
    done
    echo "== stock opencode debug agents (host-owned strict loader)"
    (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        timeout 180 "$opencode_bin" debug agents) > "$STAGE/agents.json"
    echo "== stock opencode plugin list (host-owned plugin loader)"
    (cd "$project" && HOME="$fake_home" XDG_CONFIG_HOME="$fake_home/.config" \
        COLUMNS=200 timeout 180 "$opencode_bin" plugin list) > "$STAGE/plugins.txt"
    cat "$STAGE/plugins.txt"
    python3 - "$STAGE/agents.json" "$STAGE/plugins.txt" "$config" <<'EOF'
import json
import sys

with open(sys.argv[1], "rb") as handle:
    agents = {agent["id"] for agent in json.load(handle)}
assert "code-explorer" in agents, (
    "installed agent definitions were not accepted by the stock host: "
    f"{sorted(agents)}"
)

with open(sys.argv[2], encoding="utf-8") as handle:
    plugins = handle.read()
for plugin_id in ("tracedecay-hooks", "tracedecay-mcp"):
    assert plugin_id in plugins, f"stock opencode did not load the {plugin_id} plugin:\n{plugins}"

with open(sys.argv[3], "rb") as handle:
    config = json.load(handle)
mcp = config.get("mcp", {}).get("servers", {}).get("tracedecay")
assert mcp and mcp.get("type") == "local", f"native tracedecay MCP registration missing: {mcp!r}"
assert any("tracedecay" in part for part in mcp.get("command", [])), mcp
assert "tracedecay" not in config.get("mcp", {}), "V1-era mcp.tracedecay key must not be written"
assert "lsp" not in config, "OpenCode 2 runs no LSP; the installer must not register one"
print("ok - stock opencode accepted the full installed configuration")
print(f"ok - {len(agents)} agents loaded; tracedecay-hooks and tracedecay-mcp plugins active")
EOF
    stop_stock_service "$fake_home"

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
