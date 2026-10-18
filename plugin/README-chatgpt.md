# TraceDecay ChatGPT bundle

This Agent Plugins bundle gives ChatGPT read-only access to the local
TraceDecay daemon through a code-graph MCP server, `graph`, and a
code-explorer MCP app, `tracedecay-explorer`. The daemon supplies every
answer from its index of your checkout. It reports project status separately
and includes relation generations in each response. Symbol and impact reads
do not report snapshot identity.

## Contents

- `plugin.json` and `mcp.json` are the portable Agent Plugins manifests.
- `chatgpt-extension/embedded/server.mjs` is the self-contained Node MCP adapter
  (stdio by default, `--http [host:]port` for a loopback Streamable HTTP
  endpoint).
- `chatgpt-extension/embedded/app.html` is the shared MCP App resource
  (`ui://tracedecay/code-explorer`).
- `chatgpt-extension/assets/icon.svg` is the manifest icon.

## Requirements

- TraceDecay installed and Node.js on `PATH`. The lifecycle pins the resolved TraceDecay binary for both MCP servers.
- The TraceDecay daemon (`tracedecay serve`) with at least one registered
  project.

## Installing

`tracedecay install --agent chatgpt` stages this bundle at
`~/.tracedecay/host-bundle-stage/chatgpt/tracedecay` and reports host
registration as unverifiable with exit code 0. ChatGPT requires you to
register plugins and connectors through its
developer-mode connector setup or plugin flow. TraceDecay cannot activate
the bundle for you.

Two ways to finish:

1. Point a connector at the bundle's MCP endpoint:
   `node <staged>/chatgpt-extension/embedded/server.mjs --binary /absolute/path/to/tracedecay --http 127.0.0.1:8787`
   (wrap it with `tunnel-client --mcp-server-url` for ChatGPT cloud).
2. Install the staged bundle through ChatGPT's plugin flow, which launches
   `graph` and `tracedecay-explorer` from `mcp.json` over stdio.

`tracedecay uninstall --agent chatgpt` removes the staged bundle; remove the
connector or plugin inside ChatGPT yourself. TraceDecay cannot perform or
verify that step.
