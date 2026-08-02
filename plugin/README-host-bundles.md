# TraceDecay host bundles

TraceDecay host packaging has two independent layers:

- The MCP-free **core** contains host metadata, skills, hooks, and CLI/daemon
  bindings. A native feedback projection is included only when the host has a
  checked-in registration API and adapter.
- Optional **Context MCP** and **Operator MCP** companions register the same
  TraceDecay binary and daemon authority. Removing a companion does not remove
  core hooks, skills, receipts, or another companion.

Compiled `HostBundleManifestV1` values are the first-party installation
catalog. A dry run verifies schema, compatibility, and content identity and reports owned paths, competing-extension claims,
backups, and the host-specific rollback seam without opening a writer. Apply,
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
  remain separate routes.
- **Cursor cloud, Codex, Hermes, and Kiro:** only their evidenced hook, MCP,
  and CLI routes are declared. Missing LSP/native-diagnostics APIs stay typed
  unavailable.
- **Cline:** the evidenced user MCP path is
  `~/.cline/data/settings/cline_mcp_settings.json`, honoring
  `CLINE_DATA_DIR`. The legacy VS Code extension path is migration/removal
  only. Roo Code and Kilo are not inferred compatible from Cline branding or
  transcript shape; native edit/stop delivery remains typed unavailable until
  checked-in host fixtures exist.
- **Kimi Code:** the managed plugin manifest keeps MCP, skills, and commands
  together and registers native `PostToolUse` and `Stop` hooks. Hook commands
  consume no host payload: edit completion triggers incremental sync and stop
  triggers supported transcript ingest.
- **OpenCode:** a typed `@opencode-ai/plugin` module under
  `~/.config/opencode/plugins/` (or `.opencode/plugins/` locally) consumes
  `file.edited`, `tool.execute.after`, and `session.idle` without forwarding
  event content. Its MCP and custom TraceDecay LSP entries are merged with the
  existing JSON config. Upstream analyzer brokering is disabled by default so
  OpenCode's built-ins and TraceDecay never claim the same analyzer. A globally
  installed older TraceDecay binary may not recognize `lsp bridge --stdio`;
  Doctor validates the installed
  configuration without relabeling the packaged capability unavailable.

OpenCode command files are prompt-template command artifacts owned and
receipted by the optional **Agent** component. An agent-referenced prompt file
is owned by that same component. `AGENTS.md` remains Core instruction/rules
content managed by the existing prompt-rule reconciler; it is not a separate
prompt lifecycle product, and the registry never installs it through a second
prompt authority.

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
