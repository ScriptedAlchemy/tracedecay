---
name: managing-session-context
description: 'Use when you need LCM, session search, transcript search, raw past-session replay, scoped/time grep, summary-DAG drill-down, branch/worktree/commit history, workflow recovery, or post-compaction context recovery.'
---

# Managing session context

This is the read-only session retrieval and health workflow. Compression
admission and session-boundary writes are daemon-owned host integrations, not
agent-callable tools. Never reconstruct those operations from chat content or
substitute an agent-generated summary for an authenticated host payload.

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
depth/compression ratio, and payload health.

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

## LCM status

Compression admission and session boundaries are daemon-owned
host lifecycle operations. They are not callable MCP tools and must not be
reconstructed from chat content. Use **Status → `tracedecay_lcm_status`**
(optional `provider`, `session-id`,
   `deep`): schema/message/summary/payload counts, token estimates, summary
   depth distribution + compression ratio, payload byte totals, and GC status.
   Read-only; `deep: true` adds an on-disk integrity sweep.
Use `tracedecay_lcm_doctor` for the bounded read-only health report. Doctor has
no repair, clean, garbage-collection, or apply controls.

## Guardrails

- Every callable LCM tool in this workflow is read-only. Grep/status may touch
  access counters, but no tool here compacts, repairs, cleans, or applies state.
- When a read supports `provider`, use an exact provider for provider-local
  evidence or `all` only where its schema permits aggregation.
- For multi-step recall, dispatch scoped read-only subagents by session id, time
  window, provider, role, or query variant. The parent agent validates cited
  messages/summaries and produces the final timeline.

## Handoff

- Durable decisions/facts and persisting new ones → `tracedecay:project-memory`.
- Dereferencing a truncated response handle → `tracedecay:using-the-cli`.
- CLI fallback when MCP transport fails → `tracedecay:using-the-cli`.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_message_search,tracedecay_lcm_grep,tracedecay_lcm_load_session,tracedecay_lcm_describe,tracedecay_lcm_expand,tracedecay_lcm_expand_query,tracedecay_lcm_status,tracedecay_sessions_for,tracedecay_workflows,tracedecay_session_refresh,tracedecay_project_search,tracedecay_project_context`
  (one batched call, add only the rungs needed) — then call normally.
- MCP error/timeout/disconnect: same tool, same args, via shell:
  `tracedecay tool <name>` (see `tracedecay:using-the-cli`). Never
  query `.tracedecay` databases directly; never abandon the graph over transport.

## Deliverable

Do not end this workflow without: (recall) the messages/summaries found with
session ids and timestamps, and which rung answered the question; or
(health) the store counts, compression ratio, and bounded health signals.
Report any `tracedecay_metrics:` line to the user.
