#!/usr/bin/env bash

# Keep the native capabilities fixed across discovery conditions. Product
# conditions add only their MCP and supported CLI surface.
claude_allowed_tools() {
  local condition="$1" binary="$2"
  local allowed='Read,Write,Edit,Glob,Grep,Task,Agent,Skill'
  if [[ "$condition" != native ]]; then
    allowed+=",mcp__tracedecay__*,mcp__plugin_tracedecay_graph__*,Bash(tracedecay tool *),Bash($binary tool *)"
  fi
  printf '%s\n' "$allowed"
}
