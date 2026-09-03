---
name: diagnosing-analytics
description: 'Use when checking TraceDecay adoption or telemetry health — usage analytics, silent hooks, stale hook logs, or empty analytics tables. Uses the analytics, doctor, and sessions CLI, never private store files.'
---

# Diagnosing TraceDecay analytics

TraceDecay records three telemetry streams: durable `analytics_events` in the
user-level global store (`mcp_tool_call` rows from the MCP server,
`hook_route` rows from daemon routing, and `hook_<agent>` rows bridged from
hook logs), append-only `hook_analytics.jsonl` files (one per project store
plus a user-level fallback for hooks that could not resolve a project), and
the per-project session store used for message-level usage. Adoption and
health questions are answered from the durable table — never by opening
store databases directly.

## Commands

1. **`tracedecay analytics diagnostics`** — the one-stop summary. Imports any
   new hook-log rows into the durable table first, then prints event, tool,
   and hook counts, the TraceDecay tool-call count, per-tool/per-hook rollups,
   `hook_sources` with per-file row counts, and usage ratios. Flags:
   `--all` (every project, not just the current one), `--no-sync` (skip the
   import pass; read-only).
   Use the read-only MCP twin **`tracedecay_analytics`** when the answer should
   be returned inside an agent tool call instead of a shell command.
2. **`tracedecay analytics sync`** — run only the hook-log import and report
   how many rows each source contributed.
3. **`tracedecay doctor`** — per-agent install health: hook registration, MCP
   config, binary staleness. Run this first when a hook seems silent
   (Codex hooks may exist but stay skipped until trusted in Codex's own
   hooks approval prompt).
4. **`tracedecay tool lcm_status --provider all --json`** and
   **`tracedecay tool lcm_doctor --provider codex --json`** —
   session-store ingest and compression health per provider.
5. **`tracedecay sessions search "mcp__tracedecay" --provider all`** — find
   tool usage evidence inside ingested transcripts across providers.
6. **`tracedecay gain`** / **`tracedecay cost`** — token savings ledger and
   spend rollups.
7. **Dashboard**: `tracedecay dashboard` serves the same summaries at
   `/api/plugins/analytics/overview|diagnostics|usage|hints`.
8. **Observatory / costs model → `tracedecay_observatory_read`**: the
   project's canonical Observatory and Costs models. Read-only; use this
   when the question is the retained observatory snapshot rather than the
   analytics diagnostics rollup.

## Reading the diagnostics output

- `source: "analytics_events"` means durable data answered; a
  `session_messages_and_hook_analytics` source means no durable events matched
  and the summary fell back to hook logs only.
- `hook_sources` lists each hook-log file with `rows_total` vs
  `rows_included`. A large gap means rows written before project attribution
  existed; they are visible under `--all` but cannot be assigned to a project.
  `rows_malformed` / `first_malformed_line` show JSONL corruption that the
  reader skipped and should usually be investigated as a concurrent append bug.
- `project_id: null` means the command ran outside an indexed project and
  reported globally. Run from an indexed project root for per-project scope,
  or pass `--all` deliberately.
- `import.imported` greater than zero means a hook-log backlog was just
  bridged — re-read the counts, they include it.
- A high hook-call count with a low TraceDecay tool-call count is an adoption gap:
  hooks fire, but sessions are not using tracedecay tools. Check
  `by_prompt_category` to see which task types miss it.

## Guardrails

- A zero-row `analytics_events` table inside a project store is not evidence
  that telemetry is broken — durable events live in the user-level store. Run
  `tracedecay analytics diagnostics` before concluding anything is dead.
- Never inspect private store files directly; their schemas are internal. The
  diagnostics JSON already merges every relevant source.
- If the MCP transport is down, every command above still works — they are
  plain CLI subcommands (see `tracedecay:using-the-cli`).

## If tools are deferred or MCP fails

- This skill is already CLI-first: the `tracedecay analytics` / `doctor` /
  `sessions` subcommands run over shell and need no MCP transport.
- The `tracedecay tool lcm_status` / `lcm_doctor` calls also work via MCP; if
  those tools are deferred, load once with ToolSearch —
  `select:tracedecay_lcm_status,tracedecay_lcm_doctor,tracedecay_observatory_read,tracedecay_analytics` — or just use the CLI
  form shown above (see `tracedecay:using-the-cli`).

## Deliverable

Do not end this workflow without: headline counts (events, MCP tool calls,
tracedecay calls, hook calls) with the exact command used, plus any gaps
found: unattributed hook rows, stale or missing sources, providers with hooks
firing but zero tool usage. Report any `tracedecay_metrics:` line to the user.
