---
name: managing-session-context
description: 'Use when you need LCM, session search, transcript search, raw past-session replay, scoped/time grep, summary-DAG drill-down, branch/worktree/commit history, workflow recovery, compaction recovery, or read-only LCM diagnosis; and when driving host LCM lifecycle preflight/compress/boundary.'
---

# Managing session context

One skill for both sides of the session store: **retrieval** (read-only, where
you start when you need past-session content) and the **LCM compression and
maintenance lifecycle** (the write/health side). Retrieval is cheap and safe;
lifecycle tools are **host-agent integration tools** — invoke them only when the
host is managing its own context window or the user explicitly asks to compress
or establish a boundary. Doctor is separately safe for diagnosis, never casual
recall.

For durable *decisions and facts* (rather than raw conversation), start with
`tracedecay:project-memory` instead — it owns the FTS → fact lane of
`tracedecay_message_search`; this skill owns the FTS → LCM lane.

## Retrieval ladder (read-only, start here)

Climb cheapest-first; stop as soon as the question is answered. For
cross-project or sibling-repo session search, first resolve the target project
with `tracedecay_project_search`/`tracedecay_project_context`, then pass
`project_id`, `project_path`, or `project_selector` to
`tracedecay_message_search` instead of searching the active project by accident.

1. **Fast full-text recall → `tracedecay_message_search`:** FTS over ingested
   transcripts, returning messages and session ids. Its defaults are
   `provider=all`, `include_subagents=true`, `scope=all`, `message_type=all`,
   `limit=10`, and `catch_up=false`; it never ingests or refreshes data.
2. **Scoped temporal grep → `tracedecay_lcm_grep`:** bounded raw-message
   snippets with `query`, `scope` (`current`|`session`|`all`), `session_id`,
   role/source/time filters, and an opaque cursor. It defaults to
   `temporal_mode=current`, `relationship_scope=all`, `message_type=all`,
   `include_summaries=false`, and `sort=relevance`.
3. **Lossless temporal replay → `tracedecay_lcm_load_session`:** ordered raw
   messages for one `session_id`, with `roles` and bounded
   `content_offset`/`content_limit` slices. It defaults to
   `temporal_mode=forensic`. Continue only with the returned opaque
   `next_cursor` unchanged; never manufacture a continuation from an offset or
   row number.
4. **Summary-DAG drill-down:** use `tracedecay_lcm_describe` (`provider`,
   `session_id`, optional target) to inspect a session or node without opening
   its body, then `tracedecay_lcm_expand` (`provider`, `session_id`, target) to
   open one raw message, summary node, or external payload. Page immediate
   summary sources with `source_offset`/`source_limit`. Continue only with the
   returned opaque `next_cursor` unchanged with the same target, source limit, and content slice;
   changing a bound continuation input is denied. For a bounded prompt
   context, `tracedecay_lcm_expand_query` takes `provider`, `session_id`, and
   `prompt`: when it returns `needs_synthesis=true`, the host must synthesize
   from the bounded context; use its direct answer only when synthesis is not
   needed.
5. **Read temporal bounds:** inspect every response's `coverage`, `anchors`,
   watermarks, and explanations. Partial, hidden, or redacted coverage is not
   evidence that content never existed; retain anchors when citing or drilling
   further.
6. **Git-scoped session lookup → `tracedecay_sessions_for` /
   `tracedecay_session_lookup`:** use `git_ref`
   (`branch`|`worktree`|`commit`) and `value`, optionally `since`/`until`.
   Commit queries default to `relation=produced` and `limit=20`; feed returned
   session ids back into rungs 2–4.
7. **Workflow-run recovery → `tracedecay_workflows`:** recover multi-agent
   `wf_*` runs and their per-phase agents. List a parent thread with
   `session_id`, list by `branch`/`worktree`/`commit`, inspect a `run_id`, or
   drill into `run_id` + `agent_label`; its default is `limit=20`. Then scope
   `tracedecay_message_search` with `workflow_run` and optional
   `workflow_agent`, or replay with rungs 3–4.

Use `tracedecay_lcm_status` to inspect counts, token estimates, DAG
depth/compression ratio, and GC state before making a lifecycle decision.

On Hermes, the context engine exposes native aliases `lcm_grep`,
`lcm_load_session`, `lcm_describe`, `lcm_expand`, `lcm_expand_query`,
`lcm_status`, and `lcm_doctor` for their matching TraceDecay LCM commands.
Use the native alias's schema when it is offered: for example,
`lcm_grep` uses `session_scope` and `time_from`/`time_to`, while
`lcm_load_session` uses `max_content_chars`. Do not mix those host aliases with
canonical command fields, and do not assume the aliases exist in another host.

After a compaction, if prior-session context seems missing, run this ladder
before assuming the compacted summary is complete.

## Freshness is explicit

Recall never performs catch-up. If a read returns `refresh_required`, get clear
host or user lifecycle intent before invoking `tracedecay_session_refresh`.
Its actions are `begin`, `status`, and `cancel`: `begin` returns an opaque
handle that `status` and `cancel` require. Use the authoritative selectors
provided by the host/runtime; do not reconstruct refresh identity from chat
text or a filesystem path.

## Lifecycle and diagnostic tools

All take `--provider` and (except doctor/status) `--session-id`. They use the
active registered project's user-profile session store. Compression and boundary
calls mutate only with host/lifecycle intent; Doctor and Status are read-only.

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
4. **Doctor → `tracedecay_lcm_doctor`** (`provider`, optional `session-id`):
   bounded read-only diagnostics. It reports integrity findings, placeholders,
   and retention or cleanup candidates without payload bodies; daemon-owned
   maintenance owns any later action.
5. **Status → `tracedecay_lcm_status`** (optional `provider`, `session-id`,
   `deep`): schema/message/summary/payload counts, token estimates, summary
   depth distribution + compression ratio, payload byte totals, and GC status.
   Read-only; `deep: true` adds an on-disk integrity sweep.

## Typical lifecycle flow

Preflight → (if it requests compression) compress → status to confirm the ratio
moved. On a real host session change, call session_boundary. If counts look
wrong (missing sessions, stale FTS, orphaned payloads), run Doctor and use its
evidence to identify the daemon-owned maintenance boundary.

## Guardrails

- Retrieval rungs above, `preflight`, `status`, and Doctor are read-only
  (grep/status may touch access counters). `compress` and `session_boundary`
  **mutate** durable session state — run them only with clear lifecycle or user
  intent, never speculatively during recall.
- `provider` is required and `all` is rejected for the lifecycle tools; target
  one provider at a time.
- For multi-step recall, dispatch scoped read-only subagents by session id, time
  window, provider, role, or query variant. Subagents must not drive
  compression or boundaries; the parent agent validates cited
  messages/summaries and produces the final timeline.
- Keep token knobs conservative; over-aggressive compression loses the replay
  fidelity the retrieval ladder depends on.

## Handoff

- Durable decisions/facts and persisting new ones → `tracedecay:project-memory`.
- Dereferencing a truncated response handle → `tracedecay:using-the-cli`.
- CLI fallback when MCP transport fails → `tracedecay:using-the-cli`.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_message_search,tracedecay_lcm_grep,tracedecay_lcm_load_session,tracedecay_lcm_describe,tracedecay_lcm_expand,tracedecay_lcm_expand_query,tracedecay_lcm_status,tracedecay_lcm_compress,tracedecay_sessions_for,tracedecay_workflows,tracedecay_session_refresh,tracedecay_project_search,tracedecay_project_context`
  (one batched call, add only the rungs needed) — then call normally.
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
