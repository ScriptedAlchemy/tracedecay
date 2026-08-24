#!/usr/bin/env bash
# MCP conformance smoke test: drives `tracedecay serve` (stdio) with the
# official MCP Inspector CLI (@modelcontextprotocol/inspector --cli).
#
# Why this exists on top of tests/mcp_suite/ (which already exercises
# initialize/tools/call with hand-crafted JSON-RPC): the Inspector embeds the
# official TypeScript SDK client, so every call here additionally proves
#   - protocol-version negotiation with a *newer* client (the 0.22.0 client
#     requests protocolVersion 2025-11-25; the server answers 2024-11-05 and
#     the SDK accepts it — the Rust tests only ever send 2024-11-05),
#   - SDK-side Zod validation of the initialize result, capability shapes,
#     and every tool's inputSchema in tools/list,
#   - the notifications/initialized + logging/setLevel lifecycle a real
#     client performs.
#
# Requirements: node >= 18 + npx (network on first run to fetch the pinned
# inspector into the npx cache; warm runs are offline), git, and a tracedecay
# binary. Runs against a throwaway fixture project with HOME/XDG redirected
# into the temp dir, so it never touches the user's real tracedecay state.
#
# Usage:
#   scripts/mcp-conformance-smoke.sh                # auto-detect binary
#   TRACEDECAY_BIN=target/debug/tracedecay scripts/mcp-conformance-smoke.sh

set -euo pipefail

INSPECTOR_VERSION="${INSPECTOR_VERSION:-0.22.0}"
CALL_TIMEOUT_SECS="${CALL_TIMEOUT_SECS:-60}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_PATH="$REPO_ROOT/scripts/mcp-conformance-smoke.sh"
DAEMON_HARNESS="$REPO_ROOT/scripts/with-isolated-tracedecay-daemon.sh"
WORK_DIR=""
INIT_STDERR=""

run_smoke() {
  local work_dir="$1"
  local fixture="$2"
  local tools_a tools_b call_out res_out
  local failures=0

  inspect() {
    # Run from the fixture so the spawned server's cwd matches the indexed
    # project (otherwise tool results gain a cwd-mismatch warning block).
    (cd "$fixture" && timeout "$CALL_TIMEOUT_SECS" \
      npx -y "@modelcontextprotocol/inspector@$INSPECTOR_VERSION" --cli \
      "$TRACEDECAY_BIN" serve -p "$fixture" "$@")
  }

  # json_assert <file> <node expression over parsed json `j`>
  json_assert() {
    node -e '
      const fs = require("fs");
      const j = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
      if (!eval(process.argv[2])) process.exit(1);
    ' "$1" "$2"
  }

  fail() {
    echo "FAIL $1" >&2
    failures=$((failures + 1))
  }
  ok() {
    echo "ok   $1"
  }

  if ! (cd "$fixture" && "$TRACEDECAY_BIN" init >/dev/null 2>"$work_dir/init.stderr"); then
    echo "error: tracedecay init failed" >&2
    return 1
  fi
  "$TRACEDECAY_BIN" disable-upload-counter >/dev/null 2>&1 || true

  # 1. tools/list succeeds through the SDK client (implies the full initialize
  #    handshake + version negotiation + Zod validation of every tool schema).
  tools_a="$work_dir/tools-a.json"
  if inspect --method tools/list > "$tools_a" 2>"$work_dir/tools-a.err"; then
    ok "tools/list (SDK handshake + schema validation)"
    if json_assert "$tools_a" 'Array.isArray(j.tools) && j.tools.length > 5 && j.tools.every(t => t.name && t.inputSchema && t.inputSchema.type === "object")'; then
      ok "tools/list has tools with object inputSchemas"
    else
      fail "tools/list has tools with object inputSchemas"
    fi
    if json_assert "$tools_a" 'j.tools.some(t => t.name === "tracedecay_search")'; then
      ok "tools/list includes tracedecay_search"
    else
      fail "tools/list includes tracedecay_search"
    fi
  else
    cat "$work_dir/tools-a.err" >&2
    fail "tools/list (SDK handshake + schema validation)"
  fi

  # 2. Determinism: a second run must be byte-identical.
  tools_b="$work_dir/tools-b.json"
  if inspect --method tools/list > "$tools_b" 2>/dev/null && cmp -s "$tools_a" "$tools_b"; then
    ok "tools/list is deterministic across runs"
  else
    fail "tools/list is deterministic across runs"
  fi

  # 3. tools/call round-trip against the indexed fixture.
  call_out="$work_dir/call.json"
  if inspect --method tools/call --tool-name tracedecay_search --tool-arg query=main > "$call_out" 2>/dev/null &&
    json_assert "$call_out" 'Array.isArray(j.content) && j.content.some(c => c.type === "text" && c.text.includes("Search Results") && c.text.includes("main"))'; then
    ok "tools/call tracedecay_search finds main()"
  else
    fail "tools/call tracedecay_search finds main()"
  fi

  # 4. resources/list exposes the status resource.
  res_out="$work_dir/resources.json"
  if inspect --method resources/list > "$res_out" 2>/dev/null &&
    json_assert "$res_out" 'Array.isArray(j.resources) && j.resources.some(r => r.uri === "tracedecay://status")'; then
    ok "resources/list exposes tracedecay://status"
  else
    fail "resources/list exposes tracedecay://status"
  fi

  # 5. Error path: unknown tool must fail with a nonzero exit code.
  if inspect --method tools/call --tool-name definitely_not_a_tool >/dev/null 2>&1; then
    fail "tools/call unknown tool exits nonzero"
  else
    ok "tools/call unknown tool exits nonzero"
  fi

  if ((failures > 0)); then
    echo "mcp-conformance-smoke: $failures check(s) failed" >&2
    return 1
  fi
  echo "mcp-conformance-smoke: all checks passed"
}

cleanup() {
  local status=$?

  trap - EXIT
  if ((status != 0)) && [[ -s "$INIT_STDERR" ]]; then
    echo "tracedecay init stderr:" >&2
    cat "$INIT_STDERR" >&2 || true
  fi
  [[ -z "$WORK_DIR" ]] || rm -rf "$WORK_DIR"
  exit "$status"
}

find_tracedecay_bin() {
  local target_dir candidate

  if [[ -n "${TRACEDECAY_BIN:-}" ]]; then
    printf '%s\n' "$TRACEDECAY_BIN"
    return
  fi

  target_dir="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
  for candidate in "$target_dir/debug/tracedecay" "$target_dir/release/tracedecay"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
  command -v tracedecay || true
}

main() {
  local tracedecay_bin npm_cache fixture status

  tracedecay_bin="$(find_tracedecay_bin)"
  if [[ -z "$tracedecay_bin" || ! -x "$tracedecay_bin" ]]; then
    echo "error: no tracedecay binary found; build one or set TRACEDECAY_BIN" >&2
    return 2
  fi
  tracedecay_bin="$(readlink -f "$tracedecay_bin")"
  echo "using tracedecay binary: $tracedecay_bin ($("$tracedecay_bin" --version))"
  echo "using inspector: @modelcontextprotocol/inspector@$INSPECTOR_VERSION"

  # Resolve the effective npm cache before HOME is redirected so npx reuses
  # the warm inspector install instead of re-downloading it.
  npm_cache="${npm_config_cache:-$(npm config get cache)}"

  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mcp-smoke.XXXXXX")"
  INIT_STDERR="$WORK_DIR/init.stderr"
  fixture="$WORK_DIR/proj"
  mkdir -p "$fixture/src" "$WORK_DIR/home"
  printf 'fn main() { println!("hello"); }\n' > "$fixture/src/main.rs"
  git -C "$fixture" init --quiet
  trap cleanup EXIT

  set +e
  HOME="$WORK_DIR/home" \
    XDG_DATA_HOME="$WORK_DIR/home/.local/share" \
    XDG_CONFIG_HOME="$WORK_DIR/home/.config" \
    npm_config_cache="$npm_cache" \
    TRACEDECAY_BIN="$tracedecay_bin" \
    INSPECTOR_VERSION="$INSPECTOR_VERSION" \
    CALL_TIMEOUT_SECS="$CALL_TIMEOUT_SECS" \
    "$DAEMON_HARNESS" --bin "$tracedecay_bin" --ready-timeout 5 -- \
    "$SCRIPT_PATH" --run "$WORK_DIR" "$fixture"
  status=$?
  set -e
  return "$status"
}

if [[ "${1:-}" == "--run" ]]; then
  shift
  run_smoke "$@"
else
  main "$@"
fi
