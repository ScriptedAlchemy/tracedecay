# TraceDecay for Claude Code

This plugin bundles the TraceDecay MCP server, a suite of workflow skills, and
lifecycle hooks for code-graph, impact, recall, and context-saving workflows in
Claude Code.

## What it ships

- **MCP server** (`.mcp.json`): the `tracedecay` stdio server exposing the code
  graph, search, call-graph, impact, memory, and session-recall tools.
- **Skills** (`skills/`): one skill per common workflow — searching for code,
  reading code cheaply, mapping architecture, impact analysis, reviewing diffs,
  recalling project memory and session context, and more. Claude Code
  auto-discovers each `SKILL.md` by its `name`/`description` frontmatter and
  loads the body only when the workflow matches.
- **Lifecycle hooks** (`hooks/hooks.json`): `SessionStart`,
  `UserPromptSubmit`, `Stop`, `PreToolUse`, and `PostToolUse` handlers that
  inject index status and tool-routing steering, keep the graph/session store
  warm, redirect explore-agent calls toward the tracedecay tools, and nudge
  Grep/Glob/Read-style searches toward `tracedecay_grep`, `tracedecay_search`,
  `tracedecay_context`, and bounded graph reads.

## Install

Install the plugin (and register its hooks and MCP server) with:

```
tracedecay install --agent claude
```

The installer resolves the absolute path of the `tracedecay` binary and writes
it into the managed hooks, so the plugin works even when tracedecay lives on a
path with spaces.

## CLI fallback

Every MCP tool is also available from the shell as `tracedecay tool <name>`
(`tracedecay tool` lists tools; `tracedecay tool <name> --help` shows
parameters). Bundled skills and steering use that CLI fallback when MCP
transport errors or times out, instead of querying `.tracedecay` databases.
