---
name: tracedecay-cli
description: Use when a Pi task in a TraceDecay project explores code, traces calls, adds a helper or changes a signature, selects or runs tests, runs cargo check or Clippy, or reviews a diff. Skip when you can already name the file to read or the edit stays inside one file. Load this skill before the first tool call.
---

# TraceDecay in Pi

Pi registers one native `tracedecay_*` tool per TraceDecay catalog tool, plus
`/tracedecay` commands (see the `tracedecay` extension in
`~/.pi/agent/extensions/`). They bridge the same tool interface that the Codex
and Cursor plugins expose over MCP, through the supported CLI. Tools that
change project state ask for approval first and refuse an explicit project
selector. Do not read `.tracedecay` databases directly.

## Routes

Make the one call for the moment, then stop when it answers the question:

- Concept or architecture: `tracedecay_context`
- Symbol discovery: `tracedecay_search` or `tracedecay_find_exact_symbol`
- Literal or regular expression: `tracedecay_grep`
- File lookup: `tracedecay_files`
- Source detail: `tracedecay_source_outline`, then `tracedecay_source_body`
- Calls: `tracedecay_callers` or `tracedecay_callees`
- New helper or structural edit: `tracedecay_search`, then `tracedecay_impact`
- Tests: `tracedecay_affected` or `tracedecay_test_map`, before any broad test run
- Compiler work: `tracedecay_diagnostics` before a build; `tracedecay_diagnose` after an error
- Review: `tracedecay_diff_context` before a raw diff
- Index state: `tracedecay_status` and `tracedecay_active_project`

Reuse returned node IDs and continuation handles. Preserve exact
project/worktree selectors; cross-project operations use the selected
registered store, not whichever project is active. A preview or read does not
grant mutation authority: apply, cancellation, and rollback consume their
returned identities, and committed effects are reconciled before a retry.

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
`/tracedecay-version` shows the binary version. If no daemon is running, the
tools report TraceDecay as unavailable and do not start one: a stopped daemon
may be an intentional hold, and `tracedecay daemon status` shows the state.

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

**Report:** the answer with the file and symbol behind each claim, the coverage
when a result is partial, and the limitation when TraceDecay was unavailable.
Do not claim absence from an empty index result, dump broad files when a graph
result answers, or store secrets or transient progress as memory facts.
