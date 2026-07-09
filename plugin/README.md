# TraceDecay Plugin Bundle

This source tree builds the TraceDecay integrations for Claude Code, Codex,
and Cursor. The installed bundles expose the `graph` MCP server, shared
workflow skills, and host-specific lifecycle hooks.

## Naming convention

The plugin is named `tracedecay`, and hosts namespace a plugin's MCP tools by
the plugin name plus the **server key**. To avoid a redundant display such as
`plugin tracedecay tracedecay`, the bundle's MCP server key is `graph` (see
`.mcp.json` / `mcp-cursor.json`), so Claude renders `plugin tracedecay graph`
and Codex/Cursor render `graph:…`. The individual tool names keep their
`tracedecay_` prefix (they are stable identifiers referenced by skills, docs,
and analytics), and non-plugin/direct installs still register the server under
the `tracedecay` key (the `mcp__tracedecay__*` namespace). Skills announce
themselves as `Using tracedecay:<skill-slug>` — the host prefix plus the skill
slug, never a doubled `tracedecay` — and that single convention is applied to
every `Announce:` line.

## Source Layout

- `skills/`: shared `SKILL.md` workflow instructions.
- `hooks/hooks-claude.json`: Claude Code lifecycle hooks. `PostToolUse`
  observes edit, shell, grep, glob, and read tools so the plugin can refresh
  the index and steer broad search toward TraceDecay.
- `hooks/hooks-codex.json`: repo-local Codex hook seed. It is intentionally
  empty; the global Codex plugin fills hooks at install time.
- `hooks/hooks-cursor.json`: Cursor lifecycle hooks.
- `.mcp.json`: shared Claude/Codex MCP config. Codex rewrites args/env by
  install scope; Claude rewrites the command to the resolved binary path.
- `mcp-cursor.json`: Cursor MCP config, deployed as `mcp.json`.
- `README-claude.md`, `README-codex.md`, `README-cursor.md`: host README
  files, deployed as `README.md`.

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
