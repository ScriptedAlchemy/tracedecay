# Devin integration

TraceDecay supports Devin as an independent agent integration. Devin's local
terminal and Desktop client stores MCP registrations in a user, project, or
local-project scope.

`tracedecay install --agent devin` registers `tracedecay serve` as the
`mcpServers.tracedecay` stdio server in the Devin user registry:

```text
~/.config/devin/mcp_config.json
```

Use `tracedecay install --agent devin --local` from a repository to register
the same server in the shared project registry:

```text
.devin/mcp_config.json
```

These entries are compatible with Devin's own MCP CLI. The equivalent native
commands are:

```sh
devin mcp add --scope user tracedecay -- tracedecay serve
devin mcp add --scope project tracedecay -- tracedecay serve
```

Devin also has a non-committed local scope at
`.devin/mcp_config.local.json`; TraceDecay leaves that personal configuration
alone. Uninstall removes only the `mcpServers.tracedecay` key and preserves
other MCP servers and Devin settings.

Devin controls tool approval through its own permission modes. TraceDecay does
not change those permissions implicitly.
