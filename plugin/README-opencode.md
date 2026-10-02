# TraceDecay for OpenCode

This plugin targets OpenCode 2. It bundles the TraceDecay MCP server, two
native V2 TypeScript plugins, and, through the optional Agent component, the
shared workflow skills, command prompt templates, and schema-adapted
subagents. OpenCode 1 plugins do not load in OpenCode 2 and vice versa;
upgrade the host before installing this bundle.

## What it ships

- **Native plugin** (`opencode/tracedecay.ts`, id `tracedecay-hooks`):
  OpenCode discovers direct `plugins/*.{ts,js}` files under the global config
  directory and every `.opencode/` directory. The definition registers an
  `execute.after` tool hook and subscribes to the server's public event
  stream for the durable `session.execution.succeeded` / `failed` /
  `interrupted` boundaries. Each callback schedules a bounded daemon-admission
  child (`hook-opencode-tool-after` or `hook-opencode-event`) in the plugin's
  own location directory and returns without waiting for it; the daemon owns
  capture and indexing. Because a shared OpenCode server loads one instance
  per location but publishes every location's events to each, boundaries are
  attributed through the located session events that precede them and
  dispatched once, by the owning instance.
- **Guidance delivery**: OpenCode 2 server plugins have no client UI channel,
  so daemon guidance returned by a hook child is held for the owning session
  and injected as system context at that session's next model request
  (`ctx.session.hook("context")`).
- **MCP companion** (`opencode/tracedecay-mcp.ts`, id `tracedecay-mcp`, and
  `opencode/opencode.registration.json`): registers the `tracedecay` stdio
  server (`tracedecay serve`) through `ctx.mcp.transform`. The installer also
  merges the same server into the native `mcp.servers` map of `opencode.json`
  and removes a V1-era `mcp.tracedecay` entry it finds there.
- **Skills, commands, and agents**: the Agent component deploys the shared
  `skills/` tree, the shared command prompt templates from `commands/`, and
  OpenCode-schema agent definitions derived from `agents/` (V2 `permissions`
  frontmatter denying `edit` and `shell`). `AGENTS.md` remains Core
  instruction content managed by the prompt-rule reconciler; it is not a
  separate rules product.

OpenCode 2 runs no language servers, so no LSP bridge is registered.
TraceDecay does not drive `opencode plugin add`: that command installs npm
and Git packages and records them under the host-owned `plugins` key, which
does not apply to a local plugin file the host already discovers.

## Install

Install the plugin (and merge its MCP registration) with:

```
tracedecay install --agent opencode
```

The installer writes the plugin to `~/.config/opencode/plugins/tracedecay.ts`
(or `.opencode/plugins/tracedecay.ts` for a project-local install), merges
`mcp.servers.tracedecay` into `opencode.json`, and rewrites
`__TRACEDECAY_BIN__` to the resolved absolute `tracedecay` executable path so
OpenCode does not depend on shell `PATH`. The plugins import only types from
`@opencode/plugin`, so no package install is required in the config
directory.

OpenCode reloads watched config directories automatically; run
`opencode service restart` if a change is not picked up. Use
`tracedecay doctor` to inspect the registration.

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
