# Devin Local integration

TraceDecay supports Devin Local as an independent local agent integration.

`tracedecay install --agent devin` registers the local stdio server as
`mcpServers.tracedecay` in Devin Local's current user configuration:

```text
~/.config/devin/mcp_config.json
```

Use `tracedecay install --agent devin --local` from a repository to register
the same server in the shared project configuration:

```text
.devin/mcp_config.json
```

The entry runs the installed TraceDecay binary with `serve`, preserving other
MCP servers in the same document. Uninstall removes only the
`mcpServers.tracedecay` key.

Devin Local supports project, user, and local-override MCP scopes. TraceDecay
uses the documented user and project paths; personal secrets or local-only
overrides remain Devin-owned in `.devin/mcp_config.local.json`.

Devin Local prompts before MCP tools by default. Configure its permissions
separately if your organization wants to pre-approve TraceDecay tools; this
integration does not widen agent or MCP permissions implicitly.
