# TraceDecay for Codex

This plugin bundles the TraceDecay MCP server, a suite of workflow skills, and
lifecycle hooks for code-graph, impact, recall, and context-saving workflows in
Codex.

## What it ships

- **MCP server** (`.mcp.json`): the `tracedecay` stdio server exposing the code
  graph, search, call-graph, impact, memory, and session-recall tools.
- **Skills** (`skills/`): one skill per common workflow — searching for code,
  reading code cheaply, mapping architecture, impact analysis, reviewing diffs,
  recalling project memory and session context, and more. Codex auto-discovers
  each `SKILL.md` by its `name`/`description` frontmatter and loads the body
  only when the workflow matches. These mirror the model-invocable Cursor skills
  so both hosts steer agents toward the same tracedecay tools.
- **Lifecycle hooks** (`hooks/hooks.json`, referenced from the manifest's
  `hooks` field): `SessionStart`, `UserPromptSubmit`, `SubagentStart`, and
  `PostToolUse` handlers that inject index status and tool-routing steering and
  keep the graph and session store warm.

Codex skips newly installed or changed command hooks until they are trusted —
run `/hooks` in Codex to review and trust the tracedecay hooks.

Codex has no always-applied rule surface (unlike Cursor's `rules/`), so the
tool-routing steering Cursor places in a rule is injected through the
`SessionStart`/`UserPromptSubmit` hooks instead.
