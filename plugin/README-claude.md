# TraceDecay for Claude Code

This plugin bundles the TraceDecay MCP server, a suite of workflow skills, and
lifecycle hooks for code-graph, impact, recall, and context-saving workflows in
Claude Code.

## What it ships

- **Configured-language LSP bridge** (`.lsp.json`): one TraceDecay server maps
  the supported file extensions to `tracedecay lsp bridge --stdio --project .`.
  It is one plugin, not one plugin per language, and forwards to the
  daemon-owned gateway rather than embedding analyzer logic.
- **MCP server** (`.mcp.json`): the `tracedecay` stdio server exposing the code
  graph, search, call-graph, impact, memory, and session-recall tools.
- **Skills** (`skills/`): one skill per common workflow — searching for code,
  reading code cheaply, mapping architecture, impact analysis, reviewing diffs,
  recalling project memory and session context, and more. Claude Code
  auto-discovers each `SKILL.md` by its `name`/`description` frontmatter and
  loads the body only when the workflow matches.
- **Lifecycle hooks** (`hooks/hooks.json`): `SessionStart`, `Stop`, and saved
  edit `PostToolUse` events. Each handler submits one bounded native envelope
  to the daemon and returns. The daemon owns follow-up capture, indexing,
  staleness work, and any advisory delivery; hooks never read stores, route
  tools, or run a model.

## Install

Install the plugin (and register its hooks and MCP server) with:

```
tracedecay install --agent claude
```

The installer resolves the absolute path of the `tracedecay` binary and writes
it into the managed hooks, so the plugin works even when tracedecay lives on a
path with spaces.

The LSP bridge, hooks, skills, and CLI bindings form the MCP-free core. The MCP
registration is an independently installable companion in the signed host
bundle lifecycle; the compatibility installer composes both.

## CLI fallback

Every MCP tool is also available from the shell as `tracedecay tool <name>`
(`tracedecay tool` lists tools; `tracedecay tool <name> --help` shows
parameters). Bundled skills and steering use that CLI fallback when MCP
transport errors or times out, instead of querying `.tracedecay` databases.
