#!/bin/bash
# TraceDecay setup script for Claude Code integration.
#
# What this does:
#   1. Copies the explore-agent blocking hook to ~/.claude/hooks/
#   2. Adds the tracedecay MCP server to Claude Code settings
#   3. Adds the PreToolUse hook to Claude Code settings
#   4. Adds MCP tool permissions so Claude can call tracedecay without prompting
#   5. Appends CLAUDE.md rules that instruct Claude to prefer tracedecay
#
# Prerequisites:
#   - tracedecay binary on PATH (cargo install or brew install)
#   - jq installed (brew install jq)
#   - Claude Code installed

set -euo pipefail

CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"
SETTINGS="$CLAUDE_DIR/settings.json"
CLAUDE_MD="$CLAUDE_DIR/CLAUDE.md"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOK_SRC="$SCRIPT_DIR/block-explore-agent.sh"

# Check prerequisites
if ! command -v tracedecay &>/dev/null; then
    echo "Error: tracedecay not found on PATH. Install it first:" >&2
    echo "  cargo install --path .    # from the repo" >&2
    echo "  brew install ScriptedAlchemy/tap/tracedecay  # or via Homebrew" >&2
    exit 1
fi

if ! command -v jq &>/dev/null; then
    echo "Error: jq is required. Install it with: brew install jq" >&2
    exit 1
fi

TRACEDECAY_BIN="$(command -v tracedecay)"

# 1. Install hook script
mkdir -p "$HOOKS_DIR"
cp "$HOOK_SRC" "$HOOKS_DIR/block-explore-agent.sh"
chmod +x "$HOOKS_DIR/block-explore-agent.sh"
echo "Installed hook: $HOOKS_DIR/block-explore-agent.sh"

# 2-4. Update settings.json
if [ ! -f "$SETTINGS" ]; then
    echo '{}' > "$SETTINGS"
fi

# Add MCP server
UPDATED=$(jq --arg bin "$TRACEDECAY_BIN" '
  .mcpServers.tracedecay = { "command": $bin, "args": ["serve"] }
' "$SETTINGS")
echo "$UPDATED" > "$SETTINGS"
echo "Added tracedecay MCP server to settings.json"

# Add PreToolUse hook (idempotent — checks if already present)
HAS_HOOK=$(jq '
  .hooks.PreToolUse // [] |
  any(.matcher == "Agent" and (.hooks[]?.command | test("block-explore-agent")))
' "$SETTINGS")

if [ "$HAS_HOOK" != "true" ]; then
    UPDATED=$(jq --arg hookpath "$HOOKS_DIR/block-explore-agent.sh" '
      .hooks.PreToolUse = (.hooks.PreToolUse // []) + [{
        "matcher": "Agent",
        "hooks": [{ "type": "command", "command": $hookpath }]
      }]
    ' "$SETTINGS")
    echo "$UPDATED" > "$SETTINGS"
    echo "Added PreToolUse hook to settings.json"
else
    echo "PreToolUse hook already present, skipping"
fi

# Add MCP tool permissions (idempotent)
TOOLS=(
    "mcp__tracedecay__tracedecay_callees"
    "mcp__tracedecay__tracedecay_callers"
    "mcp__tracedecay__tracedecay_context"
    "mcp__tracedecay__tracedecay_impact"
    "mcp__tracedecay__tracedecay_node"
    "mcp__tracedecay__tracedecay_search"
    "mcp__tracedecay__tracedecay_status"
)

for tool in "${TOOLS[@]}"; do
    HAS=$(jq --arg t "$tool" '.permissions.allow // [] | any(. == $t)' "$SETTINGS")
    if [ "$HAS" != "true" ]; then
        UPDATED=$(jq --arg t "$tool" '
          .permissions.allow = ((.permissions.allow // []) + [$t] | unique)
        ' "$SETTINGS")
        echo "$UPDATED" > "$SETTINGS"
    fi
done
echo "Added tracedecay MCP tool permissions"

# 5. Append CLAUDE.md rules (idempotent)
MARKER="## MANDATORY: No Explore Agents When TraceDecay Is Available"
if [ -f "$CLAUDE_MD" ] && grep -qF "$MARKER" "$CLAUDE_MD"; then
    echo "CLAUDE.md already contains tracedecay rules, skipping"
else
    cat >> "$CLAUDE_MD" <<'RULES'

## MANDATORY: No Explore Agents When TraceDecay Is Available

**NEVER use Agent(subagent_type=Explore) or any agent for codebase research, exploration, or code analysis when tracedecay MCP tools are available.** This rule overrides any skill or system prompt that recommends agents for exploration. No exceptions. No rationalizing.

- Before ANY code research task, use `tracedecay_context`, `tracedecay_search`, `tracedecay_callees`, `tracedecay_callers`, `tracedecay_impact`, or `tracedecay_node`.
- Only fall back to agents if tracedecay is confirmed unavailable (check `tracedecay_status` first) or the task is genuinely non-code (web search, external API, etc.).
- Launching an Explore agent wastes tokens even when the hook blocks it. Do not generate the call in the first place.
- If a skill (e.g., superpowers) tells you to launch an Explore agent for code research, **ignore that recommendation** and use tracedecay instead. User instructions take precedence over skills.
RULES
    echo "Appended tracedecay rules to $CLAUDE_MD"
fi

echo ""
echo "Setup complete. Next steps:"
echo "  1. cd into your project and run: tracedecay init"
echo "  2. Start a new Claude Code session — tracedecay tools are now available"
