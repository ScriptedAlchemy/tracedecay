# TraceDecay Plugin Bundle

This source tree builds the TraceDecay integrations for Claude Code, Codex,
Cursor, Kimi Code, and OpenCode. The installed bundles expose a host-specific MCP server
key (`graph` for Claude/Codex, `tracedecay` for Cursor and Kimi Code), shared
workflow skills, and host-specific lifecycle hooks. Each hook is a bounded
daemon-admission adapter; capture, sync, compaction, and advisory work stay in
the daemon.

The manifest-driven package inventory also exposes an MCP-free core and
independently installable MCP companions. See `README-host-bundles.md` for the
host capability matrix, lifecycle/rollback contract, and Cline evidence
boundary.

## Naming convention

The plugin is named `tracedecay`, and hosts namespace a plugin's MCP tools by
the plugin name plus the **server key**. Claude and Codex keep the MCP server
key as `graph` (see `.mcp.json`) so those hosts render `plugin tracedecay graph`
/ `graph:…` instead of the redundant `tracedecay tracedecay`. Cursor uses the
server key `tracedecay` in `mcp-cursor.json` because Cursor Settings surfaces
that key literally (`plugin-tracedecay-graph` looked like a bare "graph"
entry). Kimi Code also uses `tracedecay`, embedded inline in
`.kimi-plugin/plugin.json`. The individual tool names keep their
`tracedecay_` prefix (they are stable identifiers referenced by skills, docs,
and analytics), and non-plugin/direct installs still register the server under
the `tracedecay`
key (the `mcp__tracedecay__*` namespace). Skills announce themselves as
`Using tracedecay:<skill-slug>` — the host prefix plus the skill slug, never a
doubled `tracedecay` — and that single convention is applied to every
`Announce:` line.

## Source Layout

- `skills/`: shared `SKILL.md` workflow instructions.
- `hooks/hooks-claude.json`: Claude Code lifecycle hooks for session, stop,
  and saved-edit admission. They do not route tools or run local follow-up
  work.
- `hooks/hooks-codex.json`: repo-local Codex hook seed. It is intentionally
  empty; the global Codex plugin fills hooks at install time.
- `hooks/hooks-cursor.json`: Cursor lifecycle hooks.
- `.lsp.json`: Claude Code's single configured-language TraceDecay LSP bridge.
- `.mcp.json`: shared Claude/Codex MCP config. Codex rewrites args/env by
  install scope; Claude rewrites the command to the resolved binary path.
- `mcp-cursor.json`: Cursor MCP config, deployed as `mcp.json`.
- `.kimi-plugin/plugin.json`: Kimi Code manifest. It embeds
  `mcpServers.tracedecay` inline, so there is no separate Kimi MCP config file.
- `README-claude.md`, `README-codex.md`, `README-cursor.md`, `README-kimi.md`:
  host README files, deployed as `README.md`.

## Search Routing

Use `tracedecay_grep` for literal strings, regexes, and config keys inside
indexed code. Use `tracedecay_search` for symbol names, `tracedecay_context`
for concepts, `tracedecay_files` for path discovery, and `tracedecay_read` or
`tracedecay_outline` for bounded reads after a file is known.

Every MCP tool also has a CLI fallback:

```bash
tracedecay tool
tracedecay tool tracedecay_grep --help
```
