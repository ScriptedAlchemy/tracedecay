# TraceDecay ChatGPT Bundle

A portable Agent Plugins bundle exposing the local TraceDecay daemon's
semantic code intelligence to ChatGPT: a daemon-owned code-graph MCP server
(`graph`) and a code-explorer MCP app (`tracedecay-explorer`). Read-only:
every answer comes from the local TraceDecay daemon that already indexes
your checkout, with exact repository, worktree, branch, commit, and
generation provenance.

## Contents

- `plugin.json` / `mcp.json` — the portable Agent Plugins manifest pair.
- `chatgpt-extension/embedded/server.mjs` — self-contained Node MCP adapter
  (stdio by default, `--http [host:]port` for a loopback Streamable HTTP
  endpoint).
- `chatgpt-extension/embedded/app.html` — the shared MCP App resource
  (`ui://tracedecay/code-explorer`).
- `chatgpt-extension/assets/icon.svg` — manifest icon.

## Requirements

- `tracedecay` on `PATH` (the `graph` server) and Node.js (the explorer).
- The TraceDecay daemon (`tracedecay serve`) with at least one registered
  project.

## Installing

`tracedecay install --agent chatgpt` stages this bundle at
`~/.tracedecay/host-bundle-stage/chatgpt/tracedecay` and reports the
remaining host-side step. ChatGPT registers plugins and connectors only
through its own interactive surfaces — developer-mode connector setup or
the app's plugin flow — so TraceDecay cannot activate it for you.

Two ways to finish:

1. Point a connector at the bundle's MCP endpoint:
   `node <staged>/chatgpt-extension/embedded/server.mjs --http 127.0.0.1:8787`
   (wrap it with `tunnel-client --mcp-server-url` for ChatGPT cloud).
2. Install the staged bundle through ChatGPT's plugin flow, which launches
   `graph` and `tracedecay-explorer` from `mcp.json` over stdio.

`tracedecay uninstall --agent chatgpt` removes the staged bundle; remove the
connector or plugin inside ChatGPT yourself — the host-side step cannot be
observed or driven from here.
