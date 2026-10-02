# TraceDecay ChatGPT Extension

A ChatGPT plugin extension (MCP server + MCP App) that exposes the local
TraceDecay daemon's semantic code intelligence to ChatGPT. It is a thin
read-only bridge: every answer comes from the daemon's existing authority,
project registry, generated contracts, and storage. This package adds no new
DTOs, database, policy store, or write surface.

## What it does

From a ChatGPT conversation or the plugin's global entrypoint you can:

- Pick a project from the daemon's isolated profile registry.
- Search code symbols by name.
- Inspect a symbol with its callers, callees, impact, and a bounded call
  graph.
- See exact provenance on every answer: repository root, worktree, branch,
  commit, generation, and index freshness.
- Send the selected evidence back to the model as context or a message.
- Follow deep links into a specific symbol view and toggle fullscreen.

The shared MCP App (`ui://tracedecay/code-explorer`,
`text/html;profile=mcp-app`) renders loading, empty, stale, warming, partial,
denied, not-found, disconnected, and unavailable states truthfully. Wrong or
unregistered project ids surface the daemon's typed
not-found-or-not-authorized problem rather than an empty success.

## Architecture

`embedded/server.mjs` is one Node process offering two transports:

- **stdio** (default): what `plugin/mcp.json` launches for ChatGPT desktop
  and Codex plugin loading, and what `tunnel-client --mcp-command` can wrap.
- **`--http [host:]port`**: Streamable HTTP bound to a loopback address only
  (`127.0.0.1`, `localhost`, `::1`; rejected otherwise), with the allowed
  Host header pinned to the bound address. Every request must carry
  `Authorization: Bearer <token>`; pass `--token <value>` or read the
  generated token the server prints to stderr at startup. Intended for
  `tunnel-client --mcp-server-url` and browser testing.

Inside the server a `DaemonBridge` reads through `@tracedecay/sdk` (the
workspace's generated operation contracts, never hand-edited):

- MCP-transport operations (`project_list`, `search`, `node`, `impact`,
  `context`, `status`, `find_exact_symbol`) go through an `McpToolAdapter`
  that spawns `tracedecay serve` as an MCP client.
- HTTP operations (`code_symbol_search`, `code_callers`, `code_callees`,
  `code_declaration`) go through the daemon's authenticated
  `/projects/{id}/application` endpoint. The `daemon-authority.json` token
  under `TRACEDECAY_DATA_DIR` (or `<home>/.tracedecay`) is read server-side
  only; the app bundle and the model never see it.

`TRACEDECAY_BIN` or `--binary` selects the `tracedecay` binary; otherwise
`tracedecay` must be on `PATH`.

## Tools

- `tracedecay_workspace` (global UI entrypoint): daemon state + project list.
- `tracedecay_thread_panel` (thread/conversation UI entrypoint).
- `tracedecay_list_projects`: registered projects (app-only).
- `tracedecay_search_code`: symbol search, returns node ids, generation, and
  freshness.
- `tracedecay_inspect_symbol`: symbol detail with callers/callees/impact,
  bounded graph, provenance, and an `openai/deepLink` back into the app.
- `search_mentions`: `@`-mention search, where the host supports mentions.

## Packaging

`plugin/plugin.json` + `plugin/mcp.json` are the portable Agent Plugins
manifests. `mcp.json` launches `graph` (`tracedecay serve`) and
`tracedecay-explorer` (`node
${PLUGIN_ROOT}/chatgpt-extension/embedded/server.mjs`). The
`.codex-plugin/plugin.json` overlay stays as the compatibility fallback for
hosts that do not read the portable manifest.

`embedded/` is the sole checked-in compiled output, matching the Cursor
native extension's offline-packaging exception. `pnpm run build` regenerates
`embedded/app.html` and `embedded/server.mjs`; `pnpm run check:embedded`
performs a byte-for-byte drift check. `dist/` and `test-results/` stay
ignored. The lifecycle stamps the installed `tracedecay` binary's resolved
path into the staged `mcp.json` `graph.command` at deploy time, so the
staged bundle runs against the exact binary that installed it.

## Verification

- `pnpm typecheck` — `tsc --noEmit` over server, app, and tests.
- `pnpm test` — vitest integration suite against a real daemon: 10 tests
  covering project registration, search, symbol inspection, callers/callees,
  denied/not-found/disconnected states, and the authority token never
  leaving the server. Requires `target/debug/tracedecay`
  (`cargo build -p tracedecay-cli --bin tracedecay`) or `TRACEDECAY_BIN`.
- `pnpm test:browser` — Playwright suite driving the real `embedded` app
  inside a sandboxed iframe on the maintained `AppBridge` host: connect
  project → search → symbol → graph → evidence to chat → fullscreen, deep
  links, and denied/not-found/disconnected rendering. Requires
  `npx playwright install chromium` once.

## Lifecycle

`tracedecay install --agent chatgpt` deploys the portable bundle
(`plugin.json`, `mcp.json`, this README, `embedded/server.mjs`,
`embedded/app.html`, `assets/icon.svg`) as the staged plugin source at
`~/.tracedecay/host-bundle-stage/chatgpt/tracedecay` under the shared
receipt-backed lifecycle; `update-plugin` and `update` refresh it and
`uninstall --agent chatgpt` removes exactly the receipt-owned bytes.
ChatGPT exposes no host CLI or local registry, so every lifecycle command
reports a pending operator step instead of a completed activation: install
the staged bundle inside ChatGPT through its interactive plugin flow, or
point a connector at `node <staged>/chatgpt-extension/embedded/server.mjs
--http 127.0.0.1:8787`. `tracedecay doctor` verifies the staged bundle
(intact manifest, `extensions.com.openai` mapping, every declared file
present) and repeats the pending step; it never claims a registration it
cannot observe.

## Host support: verified vs. external prerequisite

Verified locally: MCP initialize/tools/resources discovery and the full
user journey over stdio and over the loopback Streamable HTTP transport,
plus the packaged `embedded/server.mjs` entrypoint `plugin/mcp.json` launches.

Externally blocked here, documented not claimed: reaching ChatGPT cloud
requires either developer mode with a Secure MCP Tunnel
(`tunnel-client --mcp-command` for the stdio server or `--mcp-server-url`
for the loopback HTTP server) or a public HTTPS endpoint for plugin
submission. Both need OpenAI account/workspace credentials that are not
available in this environment, so that leg is an explicit deployment
prerequisite rather than a verified path. Desktop/local plugin loading uses
the same portable `plugin.json` + `mcp.json` pair that is verified here.
