# TraceDecay for OpenCode

This plugin bundles the TraceDecay MCP server, a custom LSP registration, a
native TypeScript plugin, and — through the optional Agent component — the
shared workflow skills, command prompt templates, and schema-adapted
subagents.

## What it ships

- **Native plugin** (`opencode/tracedecay.ts`): OpenCode discovers
  `{plugin,plugins}/*.{ts,js}` one level deep under the config directory.
  The module consumes `file.edited`, `lsp.updated`, `session.idle` (and idle
  `session.status`), and `tool.execute.after`. Each handler schedules a
  bounded daemon-admission child (`hook-opencode-event` or
  `hook-opencode-tool-after`) and returns without waiting for it. The daemon
  owns capture, indexing, and any advisory toast delivery.
- **MCP companion** (`opencode/tracedecay-mcp.ts` and
  `opencode/opencode.registration.json`): registers the `tracedecay` stdio
  server as a local MCP command (`tracedecay serve`). The installer merges
  this into the existing `opencode.json`.
- **Custom LSP** (`opencode/opencode.registration.json`):
  `tracedecay lsp bridge --stdio` for the configured extensions. Upstream
  analyzer brokering is disabled by default
  (`TRACEDECAY_LSP_BROKER_UPSTREAM=0`) so OpenCode's built-ins and TraceDecay
  never claim the same analyzer.
- **Skills, commands, and agents**: the Agent component deploys the shared
  `skills/` tree, the shared command prompt templates from `commands/`, and
  OpenCode-schema agent definitions derived from `agents/`. `AGENTS.md`
  remains Core instruction content managed by the prompt-rule reconciler; it
  is not a separate rules product.

OpenCode has no `plugin.json` manifest. TraceDecay does not drive
`opencode plugin`: that CLI would double-load the auto-discovered module and
has no removal counterpart.

## Install

Install the plugin (and merge its MCP and LSP registration) with:

```
tracedecay install --agent opencode
```

The installer writes the plugin to `~/.config/opencode/plugins/tracedecay.ts`
(or `.opencode/plugins/tracedecay.ts` for a project-local install), merges
MCP and LSP into `opencode.json`, and rewrites `__TRACEDECAY_BIN__` to the
resolved absolute `tracedecay` executable path so OpenCode does not depend
on shell `PATH`.

Reload OpenCode or start a new session after installing, updating, or
removing the plugin. Use `tracedecay doctor` to inspect the registration.

## CLI fallback

Every MCP tool is also available from the shell as `tracedecay tool <name>`
(`tracedecay tool` lists tools; `tracedecay tool <name> --help` shows
parameters). Bundled skills and steering use that CLI fallback when MCP
transport errors or times out, instead of querying `.tracedecay` databases.
The CLI uses the same daemon authority and is not an availability guarantee;
neither client starts a missing or stopped service. If the daemon is
unavailable or intentionally held, report that state and use scoped native
tools without retrying or changing daemon lifecycle.

For literal strings, regexes, and config keys inside indexed code, use
`tracedecay_grep`; reserve `tracedecay_search` for symbol names and
`tracedecay_context` for concept-level discovery.
