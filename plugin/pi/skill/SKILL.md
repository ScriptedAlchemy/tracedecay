---
name: tracedecay-cli
description: Use the shared TraceDecay code graph from Pi for repository exploration, symbol lookup, call tracing, impact analysis, affected-test selection, diagnostics, and change review. Use before grep, broad file reads, new helpers, signature changes, test runs, cargo check, Clippy, or raw diff review in a TraceDecay project.
---

# TraceDecay in Pi

Pi registers native `tracedecay_*` tools and `/tracedecay` commands (see the
`tracedecay` extension in `~/.pi/agent/extensions/`). They bridge the same MCP
tool surface that the Codex and Cursor plugins expose, through the supported
CLI. Do not read `.tracedecay` databases directly.

## Preferred routes

Native tools first, one per task:

- Concept or architecture: `tracedecay_context`
- Symbol discovery: `tracedecay_search` or `tracedecay_find_exact_symbol`
- Literal or regular expression: `tracedecay_grep`
- File lookup: `tracedecay_files`
- Source detail: `tracedecay_source_outline`, then `tracedecay_source_body`
- Calls: `tracedecay_callers` or `tracedecay_callees`
- New helper or structural edit: `tracedecay_search`, then `tracedecay_impact`
- Tests: `tracedecay_affected` or `tracedecay_test_map`
- Compiler work: `tracedecay_diagnostics` before a build; `tracedecay_diagnose` after an error
- Review: `tracedecay_diff_context` before a raw diff
- Index state: `tracedecay_status` and `tracedecay_active_project`
- Any other graph tool: `tracedecay_tool` with the exact tool name and JSON args

Reuse returned node IDs and continuation handles. An empty index result does
not prove absence; report partial coverage instead of assuming.

## CLI fallback

When a native tool is unavailable or the task needs shell scripting, use the
CLI with the same tool names and JSON arguments:

```sh
tracedecay status --json
tracedecay tool context --args - <<'JSON'
{"task":"managed runtime install recovery and ownership checks","max_tokens":2500}
JSON
```

List all tools with `tracedecay tool`. Read a schema with
`tracedecay tool <name> --help`. The `/tracedecay` command shows project
status, `/tracedecay-sync` forces an incremental sync, and
`/tracedecay-version` shows the binary version.

## Worktrees

Use the nearest initialized project when it exists. For a new linked worktree
that has no local TraceDecay store, use the source project for pre-edit context:

```sh
tracedecay tool context --project /path/to/source-project --args - <<'JSON'
{"task":"the exact delegated task","max_tokens":2500}
JSON
```

Create branch indexes only when a long-lived worktree has diverged enough that
source-project context is stale. Do not run manual sync after each edit. The
Pi extension hooks keep an initialized project current.

## Operational boundaries

Use TraceDecay when its graph, durable memory, or session evidence answers the
task. Known-file reads, ordinary local edits, nonindexed material, and
clarification questions can use the native workflow directly.

Preserve exact project/worktree selectors and opaque continuation or mutation
identities. Cross-project operations must use the selected registered store,
not whichever project is active. Preview and read results do not grant
mutation authority. Apply, cancellation, and rollback consume their returned
identities; reconcile committed effects before retrying an interrupted
operation.

## Limits

- Do not dump broad files into prompts when graph results answer the question.
- Do not run broad test suites before affected-test selection.
- Do not expose secrets or store transient task progress as memory facts.
- If a tool fails, read its corrective error and retry with the stated schema.
- If the daemon is down, the tools start it once automatically; otherwise run
  `tracedecay daemon start` and report the limitation.
