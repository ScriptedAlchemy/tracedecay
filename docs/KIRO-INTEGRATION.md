# Kiro integration

This documents the defaults installed by:

```bash
tracedecay install --agent kiro
```

Kiro documents its MCP registry as plain JSON: user-level
`~/.kiro/settings/mcp.json` and workspace-level `.kiro/settings/mcp.json`
([Kiro MCP configuration](https://kiro.dev/docs/cli/mcp/configuration/)), and
reloads either file when it changes. TraceDecay edits those files directly and
never runs `kiro-cli`, whose `mcp` subcommands refuse to run until you sign in
(`You are not logged in, please log in with kiro-cli login`). Install, update,
uninstall, and `tracedecay doctor` therefore behave the same whether or not
`kiro-cli` is installed or signed in.

## Installed files

`tracedecay install --agent kiro` (profile-wide) writes:

| File | Purpose |
|---|---|
| `~/.kiro/settings/mcp.json` | Registers the global `tracedecay` MCP server with `command`, `args: ["serve"]`, and `disabled: false`. Other servers and the file's formatting are preserved. |
| `~/.kiro/steering/tracedecay-managed-skills.md` | Written by later refreshes when you have approved managed skills. The index points Kiro at `tracedecay_skill_list` and `tracedecay_skill_view`; full skill bodies remain in TraceDecay's managed skill store. |

`tracedecay install --local --agent kiro` writes the workspace equivalents in
the current project:

| File | Purpose |
|---|---|
| `.kiro/settings/mcp.json` | Registers the workspace `tracedecay` MCP server. |
| `.kiro/steering/tracedecay.md` | Workspace steering that tells Kiro sessions to prefer tracedecay MCP tools for codebase research. |
| `.kiro/steering/tracedecay-managed-skills.md` | The managed-skill index, when managed skills are approved. |
| `.kiro/agents/tracedecay.json` | The tracedecay-managed Kiro agent with `tools: ["*"]`, `allowedTools: ["@builtin", "@tracedecay"]`, one bounded prompt-admission hook, and an absolute `resources` entry for the steering file. The agent leaves `prompt` unset so Kiro's default prompt is used. |

If `.kiro/agents/tracedecay.json` already exists and is not the file
tracedecay writes, install and uninstall leave it untouched.

Uninstall removes exactly what install added: the `tracedecay` MCP server
entry (the rest of `mcp.json` is restored byte for byte), the steering block,
the managed-skill index, and the tracedecay-owned agent file. User-authored
steering around the installed block remains in place.

## Tool approval defaults

The tracedecay-owned Kiro agent is intentionally permissive:

```json
{
  "tools": ["*"],
  "allowedTools": [
    "@builtin",
    "@tracedecay"
  ]
}
```

`tools: ["*"]` keeps Kiro's built-in tools and configured MCP tools available.
`allowedTools` pre-approves Kiro's built-in tools and all tools served by the
`tracedecay` MCP server, including mutating tracedecay tools. This makes the
managed agent useful as a working example users can copy into their own Kiro
agents.

The `mcp.json` entries do not set MCP-level
`autoApprove`. Ordinary Kiro sessions or other agents that only inherit the
global MCP server keep Kiro's normal approval prompts unless users deliberately
merge the managed agent's `allowedTools` policy.

## Workspace overrides

Kiro can also load workspace MCP settings from `.kiro/settings/mcp.json`. A
workspace `mcpServers.tracedecay` entry takes precedence over the global
`~/.kiro/settings/mcp.json` entry installed by tracedecay.

`tracedecay doctor` checks the current workspace and all configured host integrations.
It reports a problem when the workspace entry disables tracedecay, omits the
`serve` argument, or points at a different command than the global install.

## Custom agents after setup

Users can create their own Kiro custom agents after running tracedecay setup.
Those agents can inherit the registered MCP server and the same
permissive tool policy by merging:

```json
{
  "includeMcpJson": true,
  "tools": ["*"],
  "allowedTools": [
    "@builtin",
    "@tracedecay"
  ]
}
```

For a custom agent, a steering file written by `install --local` can also be
referenced instead of copied. Use an absolute resource URI so it does not
resolve relative to the current project directory:

```json
{
  "resources": ["file:///Users/<you>/<project>/.kiro/steering/tracedecay.md"]
}
```

Other custom agents remain user-managed.

## Hooks

Kiro hooks are an agent-configuration field. `tracedecay install --local
--agent kiro` writes them into the tracedecay-owned agent file:

| Kiro hook | Matcher | Command | Purpose |
|---|---|---|---|
| `userPromptSubmit` | none | `tracedecay hook-kiro-prompt-submit` | Submits a bounded native prompt event to the daemon and returns. Any capture, indexing, or advisory work is daemon-owned. |

Kiro passes the native event on stdin. The adapter is fail-open and does not
make a guardrail decision, read a TraceDecay store, or run a follow-up command.

## Deliberate non-defaults

No shell post-hook or `stop` hook is installed. The managed agent's tool
approval policy is permissive, while hook execution remains limited to one
bounded daemon-admission path. Kiro's stop event is not enabled until a native
capture verifies its persisted session format.

Kiro-specific session accounting is also held back. Claude's stop hook parses
Claude session transcripts; Kiro does not share that transcript format, so
session accounting should only be added after Kiro's persisted session format is
verified.
