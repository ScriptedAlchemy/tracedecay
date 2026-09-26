# TraceDecay host bundles

TraceDecay host packaging has two independent layers:

- The MCP-free **core** contains host metadata, skills, hooks, and CLI/daemon
  bindings. A native feedback projection is included only when the host has a
  checked-in registration API and adapter.
- Optional **Context MCP** and **Operator MCP** companions register the same
  TraceDecay binary and daemon authority. Removing a companion does not remove
  core hooks, skills, receipts, or another companion.

Compiled `HostBundleManifestV1` values are the first-party installation
catalog. A dry run verifies schema, compatibility, and content identity and
reports owned paths, competing-extension claims, backups, and the
host-specific rollback seam without opening a writer. Apply,
update, repair, and uninstall require explicit confirmation and use atomic
backups, ownership receipts, and interrupted-operation recovery. Unrelated
host configuration is parsed with the host adapter's JSON/TOML library and is
never replaced wholesale.

## Host routes

- **Claude Code:** one root `.lsp.json` registers the configured-language
  `tracedecay lsp bridge`; hooks, MCP, and CLI remain independent fallbacks.
- **Cursor desktop:** the receipt-backed Agent component deploys the unpacked
  `tracedecay.cursor-native` VS Code extension under `.cursor/extensions/`.
  It starts the TraceDecay bridge through `vscode-languageclient`, forwards
  bounded diagnostics for one admitted workspace, and does not start or claim
  a second language analyzer. Hooks, MCP, CLI, and authorized feedback reads
  remain separate routes. The extension commits
  `plugin/cursor-native-extension/embedded/extension.js` so the Rust host
  bundle can embed it at compile time without Node; `pnpm run check:embedded`
  guards drift. That is an intentional offline-packaging exception, opposite
  the dashboard's gitignored `app-dist/` rebuilt by `build.rs`. See
  `plugin/cursor-native-extension/README.md`.
- **Cursor cloud, Codex, Hermes, and Kiro:** only their evidenced hook, MCP,
  and CLI routes are declared. Missing LSP/native-diagnostics APIs stay typed
  unavailable.
- **Cline:** the lifecycle owns only `mcpServers.tracedecay` in the user MCP
  registry `~/.cline/mcp.json`; it never writes the VS Code extension's
  `globalStorage`, which is read only for transcript ingest. Roo Code and
  Kilo are not inferred compatible from Cline branding or
  transcript shape; native edit/stop delivery remains typed unavailable until
  checked-in host fixtures exist.
- **Kimi Code:** the managed plugin manifest keeps skills, commands, and
  native `PostToolUse`/`Stop` hooks together; MCP is registered in Kimi
  session/user `mcp.json` so the host launches from the workspace. Hook
  commands submit bounded native events to the daemon. Capture, sync, and
  session work happen after daemon admission, never in the host adapter.
- **Factory Droid:** `droid mcp add|remove` owns `mcpServers.tracedecay` in
  `~/.factory/mcp.json`, so the ContextMcp component drives that CLI and
  never writes the file; without a `droid` binary the lifecycle refuses. The
  Core component merges `SessionStart` and `Stop` groups calling
  `tracedecay hook-droid-event` into `~/.factory/hooks.json` through the
  shared in-place JSON editor, and uninstall removes only those groups.
  Prompt, tool, and edit events stay typed unavailable until a fixture
  captures them.
- **OpenCode:** a typed `@opencode-ai/plugin` module under
  `~/.config/opencode/plugins/` (or `.opencode/plugins/` locally) consumes
  `file.edited`, `tool.execute.after`, and `session.idle`, schedules a bounded
  daemon admission child, and returns without waiting for it. Its MCP and
  custom TraceDecay LSP entries are merged with the existing JSON config.
  Upstream analyzer brokering is disabled by default so OpenCode's built-ins
  and TraceDecay never claim the same analyzer. A globally installed older
  TraceDecay binary may not recognize `lsp bridge --stdio`; Doctor validates
  the installed configuration without relabeling the packaged capability
  unavailable.

OpenCode command files are prompt-template command artifacts owned and
receipted by the optional **Agent** component. An agent-referenced prompt file
is owned by that same component. `AGENTS.md` remains Core instruction/rules
content managed by the existing prompt-rule reconciler; it is not a separate
prompt lifecycle product, and the registry never installs it through a second
prompt authority.

## Capability matrix

The first-party plugin hosts share one product skill tree and an MCP
`serve` route where the host admits MCP. Commands, agents, hooks, LSP, and
rules are host-specific. Gaps below are the current shipped state, not missed
file sync. Do not treat Codex commands/agents or Kimi LSP/agents as omitted
copies. Pi is the MCP-free host: its extension registers the catalog tools over
`tracedecay tool` and forwards its lifecycle events from inside the Pi process.

| Capability | Claude | Cursor | Codex | Kimi | OpenCode | Pi |
|---|---|---|---|---|---|---|
| **Skills** (`plugin/skills/`) | yes | yes (same set; the `skills/tracedecay-*` filter is a no-op guard) | yes | yes | yes (Agent component) | `pi/skill/SKILL.md` routing skill (Agent component) |
| **Commands** | yes (`plugin/commands/`) | overlay twins (`overlays/cursor/commands/`), independently authored | **no** (intentional: plugin deploy is manifest + skills + hooks + MCP) | yes (verbatim Claude command Markdown) | yes (Agent; shared command templates) | `/tracedecay{, -sync, -version}` registered by the extension |
| **Agents** (`plugin/agents/`) | yes (verbatim) | yes (derived Markdown) | generated TOML exists for automation export, **not** in the plugin deploy set (intentional) | **no** (intentional) | yes (schema-adapted, Agent) | **no** (intentional) |
| **Hooks** | `SessionStart`, `Stop`, `PostToolUse`, `PostCompact`, `SubagentStart` | `sessionStart`, `sessionEnd`, `stop`, `postToolUse`, `preCompact`, `afterFileEdit`, `afterShellExecution`, `workspaceOpen` | install-time table (`hooks-codex.json` seed is empty on purpose): `SessionStart`, `UserPromptSubmit`, `SubagentStart`, `PostToolUse`, `PostCompact`, `Stop` | inline `PostToolUse` + `Stop` in `.kimi-plugin/plugin.json` | `file.edited`, `lsp.updated`, `session.idle` / idle `session.status`, `tool.execute.after` | extension events: `session_start` and `agent_end` forwarded to `hook-pi-event`; `tool_result` runs a debounced `sync` |
| **MCP** | `.mcp.json` key `graph` | `mcp-cursor.json` key `tracedecay` | same `graph` key | session/user `mcp.json` key `tracedecay` (not in plugin manifest) | key `tracedecay` via `tracedecay-mcp.ts` + `opencode.registration.json` | **no** (typed unavailable; the extension bridges `tracedecay tool` over the daemon socket) |
| **LSP** | `.lsp.json` | native VS Code extension (not `.lsp.json`) | **no** (typed unavailable; intentional) | **no** (intentional) | custom LSP in `opencode.registration.json` | **no** (intentional) |
| **Rules** | **no** (intentional) | yes (`rules/tracedecay.mdc`) | **no** (intentional) | **no** (intentional) | `AGENTS.md` is Core instruction content, not a rules product | routing skill is Core instruction content |

Cursor CLI binaries exist for `hook-cursor-subagent-start` and
`hook-cursor-before-submit-prompt`; the Cursor bundle does not wire those
events. Treat them as typed-unavailable unless Cursor grows those adapter
events.

The feedback rollback switch is a first-party core-component transition. Dry run,
apply receipt, and restore all bind one exact host. Restoring replays the prior
compiled core manifest as a repair, so another host or MCP companion keeps
running and unrelated host configuration remains untouched.

## First-party catalog

Every registry artifact declares a class and owning component. Command prompt
templates, agent definitions, and agent-referenced prompts must belong to the
Agent component; MCP files belong to an MCP companion; plugin/hooks/LSP/skills
and instruction rules belong to Core. Apply rejects cross-component ownership.
Each entry also binds the exact TraceDecay release, host contract revision,
protocol range, and required capabilities before dry-run or mutation.

The catalog is compiled into the same TraceDecay executable as the installer
and host adapters. It accepts no external or third-party component path.
Content digests detect corruption and drive idempotency.
Future remote plugin distribution must define its own trust model.
