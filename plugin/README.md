# TraceDecay Plugin Bundle

This source tree builds the TraceDecay integrations for Claude Code, Codex,
Cursor, Kimi Code, OpenCode, and Pi. The installed bundles expose a host-specific MCP server
key where the host admits MCP (`graph` for Claude/Codex, `tracedecay` for Cursor, Kimi Code, and OpenCode),
shared workflow skills, and host-specific lifecycle hooks. Pi is the MCP-free host: its
installed extension registers the graph tools over the `tracedecay tool` CLI bridge instead.
Each hook is a bounded daemon-admission adapter; capture, sync, compaction, and advisory work stay in
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
entry). Kimi Code also uses `tracedecay`, registered in session/user
`mcp.json` (not the plugin manifest). The individual tool names keep their
`tracedecay_` prefix (they are stable identifiers referenced by skills, docs,
and analytics), and non-plugin/direct installs still register the server under
the `tracedecay` key (the `mcp__tracedecay__*` namespace). Skills are
referenced as `tracedecay:<skill-slug>`, the host prefix plus the skill slug,
never a doubled `tracedecay`.

## Source layout

- `skills/`: shared `SKILL.md` workflow instructions.
- `commands/`: Claude/shared slash-command sources (numbered tool steps).
- `overlays/cursor/commands/`: independently authored Cursor slash commands
  (`tracedecay-*`). Install maps them into Cursor's command directory. They are
  skill-handoff wrappers with Cursor approval notes, not generated copies of
  `commands/`, and they may differ on purpose (for example Claude
  `review-diff.md` hands off to `/tracedecay:test-changes` while the Cursor
  twin hands off to `tracedecay:assessing-impact` and omits the metrics line).
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
- `.kimi-plugin/plugin.json`: Kimi Code manifest (skills, commands, hooks).
  MCP is registered in Kimi session/user `mcp.json`, not the plugin manifest.
- `opencode/`: OpenCode native plugin (`tracedecay.ts`), MCP companion
  (`tracedecay-mcp.ts`), and registration JSON (`opencode.registration.json`).
  OpenCode has no `plugin.json`; the host discovers the TypeScript module.
- `pi/`: the Pi extension (`index.ts`) plus its `package.json` and the
  `tracedecay-cli` routing skill. Pi has no MCP route: the extension registers
  the `tracedecay_*` graph tools and lifecycle hooks inside the Pi process
  over the CLI bridge, so the installer renders the resolved binary path into
  the extension source exactly like the OpenCode plugin renderer.
- `README-claude.md`, `README-codex.md`, `README-cursor.md`, `README-kimi.md`:
  host README files, deployed as `README.md`.
- `README-opencode.md`: OpenCode host README. It is source documentation;
  OpenCode has no plugin-manifest README deploy slot.
- `README-host-bundles.md`: catalog/lifecycle contract and the host
  capability matrix.

## Search Routing

Use `tracedecay_grep` for literal strings, regexes, and config keys inside
indexed code. Use `tracedecay_search` for symbol names, `tracedecay_context`
for concepts, `tracedecay_files` for path discovery, and
`tracedecay_source_outline` or `tracedecay_source_body` for bounded reads after
a file or symbol is known.

Every MCP tool also has a CLI transport:

```bash
tracedecay tool
tracedecay tool tracedecay_grep --help
```

The CLI uses the same daemon authority as MCP and is not an availability
guarantee. Neither transport starts a missing or stopped daemon service. If the
daemon is unavailable or intentionally held, report that state and use scoped
native tools; do not retry or change daemon lifecycle unless the operator asks.
