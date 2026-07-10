---
name: managing-session-context
description: 'Use when you need LCM, session search, transcript search, raw past-session replay, scoped/time grep, summary-DAG drill-down, branch/worktree/commit history, workflow recovery, or compaction recovery; and when driving host LCM lifecycle preflight/compress/boundary/repair.'
---

# Managing session context

One skill for both sides of the session store: **retrieval** (read-only, where
you start when you need past-session content) and the **LCM compression and
maintenance lifecycle** (the write/health side). Retrieval is cheap and safe;
lifecycle tools are **host-agent integration tools** — invoke them only when the
host is managing its own context window or the user explicitly asks to compress,
repair, or inspect the store, never casually during recall.

For durable *decisions and facts* (rather than raw conversation), start with
`tracedecay:project-memory` instead — it owns the FTS → fact lane of
`tracedecay_message_search`; this skill owns the FTS → LCM lane.

## Retrieval ladder (read-only, start here)

Climb cheapest-first; stop as soon as the question is answered. For
cross-project or sibling-repo session search, first resolve the target project
with `tracedecay_project_search`/`tracedecay_project_context`, then pass
`project_id`, `project_path`, or `project_selector` to
`tracedecay_message_search` instead of searching the active project by accident.

1. **Fast full-text recall → `tracedecay_message_search`** (`query`, optional
   `provider`, `scope`: `all`|`parents_only`|`subagents_only`, `limit`;
   optional `project_id`/`project_path`/`project_selector` for registered
   projects): FTS over ingested transcripts; returns messages with their
   session ids — the entry point into the ladder below.
2. **Scoped/filtered grep → `tracedecay_lcm_grep`** (`query`, `scope`:
   `current`|`session`|`all` — `current`/`session` require `session_id`; `role`,
   `source`, `start_time`/`end_time`, `sort`: `recency`|`relevance`|`hybrid`):
   bounded raw-message snippets plus summary text when recall needs
   role/time/session precision.
3. **Lossless replay → `tracedecay_lcm_load_session`** (`session_id`,
   `after_store_id` + `limit` for stable pagination, `roles`,
   `content_offset`/`content_limit`): ordered raw messages of one session; page
   with `next_cursor` instead of asking for everything at once.
4. **Summary-DAG drill-down:** `tracedecay_lcm_describe` (`session_id`) for the
   session's raw/summary shape; `tracedecay_lcm_expand` (`target.kind`:
   `raw_message`|`summary_node`|`external_payload`) to open one node, paging
   sources via `source_offset`/`source_limit`; `tracedecay_lcm_expand_query`
   (`query`) to assemble bounded retrieval context for a prompt in one call.
5. **Store inspection → `tracedecay_lcm_status`** (counts, token estimates, DAG
   depth/compression ratio) when you need to know what the store contains before
   searching it.
6. **Git-scoped session lookup → `tracedecay_sessions_for`** (`git_ref`:
   `branch`|`worktree`|`commit`, `value`, optional `since`/`until`, `limit`):
   find sessions active on a branch or worktree, or sessions that produced a
   commit; feed returned session ids back into grep/replay/drill-down above.
7. **Workflow-run recovery → `tracedecay_workflows`**: recover multi-agent
   workflow (`wf_*`) runs and their per-phase agents. List runs for a thread
   with `session_id`, or every run on a branch/worktree/commit with
   `branch`/`worktree`/`commit` (a run inherits its parent session's git
   spans). Show one run's result summary + phases + agent roster with
   `run_id`, then drill into a single agent with `run_id` + `agent_label`.
   To read that agent's messages, scope `tracedecay_message_search` with
   `workflow_run` (+ optional `workflow_agent`), or replay via rungs 3–4.

After a compaction, if prior-session context seems missing, run this ladder
before assuming the compacted summary is complete.

## Lifecycle tools (mutating — host/lifecycle intent only)

All take `--provider` and (except doctor/status) `--session-id`. They use the
active registered project's user-profile session store.

1. **Preflight → `tracedecay_lcm_preflight`** (`provider`, `session-id`, plus
   token knobs like `current-tokens`, `threshold-tokens`, `context-length`,
   `reserve-tokens-floor`, `max-assembly-tokens`, `fresh-tail-count`): decide
   *whether* compression should run before doing it. Read-only planning call.
2. **Compress → `tracedecay_lcm_compress`** (same core args plus
   `focus-topic`, `summarizer`, `expected-current-frontier-store-id` as an
   optimistic guard): advance the compression lifecycle. **Mutates** the store.
   Use `expected-current-frontier-store-id` to no-op safely if the frontier
   moved under you.
3. **Session boundary → `tracedecay_lcm_session_boundary`** (`provider`,
   `session-id`, `old-session-id`, `bound-session-id`, `boundary-reason`):
   report that the host crossed a compression boundary. A mismatch between the
   bound and old session skips carry-over and starts a short cooldown.
4. **Doctor → `tracedecay_lcm_doctor`** (`provider`, `mode`:
   `diagnose`|`repair`|`retention`|`clean`|`gc`, `apply`, optional
   `session-id`): bounded diagnostics and safe repairs. `diagnose`/`retention`
   are read-only; `repair`/`clean`/`gc` **mutate only with `apply: true`** and
   are further gated by safety flags/env for clean and gc.
5. **Status → `tracedecay_lcm_status`** (optional `provider`, `session-id`,
   `deep`): schema/message/summary/payload counts, token estimates, summary
   depth distribution + compression ratio, payload byte totals, and GC status.
   Read-only; `deep: true` adds an on-disk integrity sweep.

## Typical lifecycle flow

Preflight → (if it requests compression) compress → status to confirm the ratio
moved. On a real host session change, call session_boundary. If counts look
wrong (missing sessions, stale FTS, orphaned payloads) run doctor
`mode: "diagnose"` first, review, then repair/clean/gc with `apply: true` only
on explicit user intent.

## Guardrails

- Retrieval (steps 1–5 above), `preflight`, `status`, and doctor
  `diagnose`/`retention` are read-only (grep/status may touch access counters).
  `compress`, `session_boundary`, and doctor `repair`/`clean`/`gc` + `apply`
  **mutate** durable session state — run them only with clear lifecycle or user
  intent, never speculatively during recall.
- `provider` is required and `all` is rejected for the lifecycle tools; target
  one provider at a time.
- For multi-step recall, dispatch scoped read-only subagents by session id, time
  window, provider, role, or query variant. Subagents must not drive
  compression, boundaries, or repair; the parent agent validates cited
  messages/summaries and produces the final timeline.
- Keep token knobs conservative; over-aggressive compression loses the replay
  fidelity the retrieval ladder depends on.

## Handoff

- Durable decisions/facts and persisting new ones → `tracedecay:project-memory`.
- Dereferencing a truncated response handle → `tracedecay:using-the-cli`.
- CLI fallback when MCP transport fails → `tracedecay:using-the-cli`.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_message_search,tracedecay_lcm_grep,tracedecay_lcm_load_session,tracedecay_lcm_status,tracedecay_lcm_compress,tracedecay_sessions_for,tracedecay_workflows,tracedecay_project_search,tracedecay_project_context`
  (one batched call, add others needed) — then call normally.
- MCP error/timeout/disconnect: same tool, same args, via shell:
  `tracedecay tool <name>` (see `tracedecay:using-the-cli`). Never
  query `.tracedecay` databases directly; never abandon the graph over transport.

## Deliverable

Do not end this workflow without: (recall) the messages/summaries found with
session ids and timestamps, and which rung answered the question; or
(lifecycle) the action taken (preflight decision, compression result, boundary
outcome, or store counts), whether it was read-only or mutating, and the
resulting compression ratio / health signals. Report any `tracedecay_metrics:`
line to the user.
