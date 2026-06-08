#!/usr/bin/env bash
set -euo pipefail

# Development MCP launcher for Cursor.
#
# Stdio MCP servers cannot hot-reload their tool definitions after Cursor has
# connected. This wrapper keeps development fast by running the current
# worktree source with cargo, so restarting/reconnecting the MCP server in
# Cursor picks up code changes without installing a new global binary.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

MANIFEST="$REPO_ROOT/Cargo.toml"
PROJECT_ROOT="${TOKENSAVE_DEV_PROJECT_ROOT:-$REPO_ROOT}"

exec cargo run --quiet --manifest-path "$MANIFEST" -- serve --path "$PROJECT_ROOT" "$@"
