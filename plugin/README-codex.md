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
  `hooks` field): `SessionStart`, `PostCompact`, and `Stop` submit bounded
  native lifecycle envelopes to the daemon. The host hook exits after the
  admission attempt; the daemon owns capture, preflight, compaction, and any
  model work.

The source `hooks/hooks-codex.json` is an empty seed for repo-local bundles.
Global Codex installs populate `hooks/hooks.json` from the managed hook table
at install time.

Codex skips newly installed or changed command hooks until they are trusted —
run `/hooks` in Codex to review and trust the tracedecay hooks.

Every MCP tool is also available from the shell as `tracedecay tool <name>`
(`tracedecay tool` lists tools; `tracedecay tool <name> --help` shows
parameters). The bundled `using-the-cli` skill and injected steering use that
CLI fallback when MCP transport errors or times out, instead of querying
`.tracedecay` databases.

For literal strings, regexes, and config keys inside indexed code, use
`tracedecay_grep`; reserve `tracedecay_search` for symbol names and
`tracedecay_context` for concept-level discovery.

Before running `cargo check`/`tsc`/`clippy` in the shell, or when shell output
shows compile errors, the injected steering routes the moment to tracedecay
diagnostics: paste captured output into `tracedecay_diagnose`, or run
`tracedecay_diagnostics` for fresh structured errors mapped to the enclosing
symbols and callers. The bundled `fixing-build-and-type-errors` skill covers
this workflow.

`PostCompact` is an admission-only request. The daemon schedules compaction
against its canonical session data and owns model execution, retries, and
results; the hook does not start a Codex child process or configure a model.
Codex does not expose an authenticated compacted payload through this hook,
so pressure-only compaction stays read-only: the daemon reports
native-summary publication as unavailable and does not mutate transcript
state.
