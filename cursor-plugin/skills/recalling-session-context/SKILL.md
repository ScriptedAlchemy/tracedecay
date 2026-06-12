---
name: recalling-session-context
description: Use when retrieving what happened in past agent sessions: full-text transcript recall, scoped/time-filtered grep, lossless session replay, summary-DAG drill-down, or compaction recovery.
---

# Recalling session context

Climb this ladder cheapest-first; stop as soon as the question is answered. For durable *decisions and facts* (rather than raw conversation), start with `tracedecay:recalling-project-memory` instead.

## Retrieval ladder

1. **Fast full-text recall → `tracedecay_message_search`** (`query`, optional `provider`, `scope`: `all`|`parents_only`|`subagents_only`, `limit`): FTS over ingested transcripts; returns messages with their session ids — the entry point for everything below.
2. **Scoped/filtered grep → `tracedecay_lcm_grep`** (`query`, `scope`: `current`|`session`|`all` — `current`/`session` require `session_id`; `role`, `source`, `start_time`/`end_time`, `sort`: `recency`|`relevance`|`hybrid`): bounded raw-message snippets plus summary text when FTS recall needs role/time/session precision.
3. **Lossless replay → `tracedecay_lcm_load_session`** (`session_id`, `after_store_id` + `limit` for stable pagination, `roles`, `content_offset`/`content_limit`): ordered raw messages of one session; page with `next_cursor` instead of asking for everything at once.
4. **Summary-DAG drill-down:** `tracedecay_lcm_describe` (`session_id`) for the session's raw/summary shape; `tracedecay_lcm_expand` (`target.kind`: `raw_message`|`summary_node`|`external_payload`) to open one node, paging sources via `source_offset`/`source_limit`; `tracedecay_lcm_expand_query` (`query`) to assemble bounded retrieval context for a prompt in one call.
5. **Store inspection → `tracedecay_lcm_status`** (counts, token estimates, DAG depth/compression ratio) when you need to know what the store contains before searching it.

## Guardrails

- Steps 1–5 are read-only. `tracedecay_lcm_compress`, `tracedecay_lcm_preflight`, and `tracedecay_lcm_session_boundary` are **lifecycle-integration tools for host agents** — never invoke them casually during recall.
- If the LCM store itself looks wrong (missing sessions, broken FTS, stale counts) → `tracedecay_lcm_doctor` (`mode: "diagnose"` first; `repair`/`clean` mutate and need explicit user intent).
- All LCM tools default to `storage_scope: "project_local"`; only pass `hermes_profile` (with an absolute `hermes_home`) when the user asks about a Hermes profile store.

## Handoff

- Durable decisions/facts and persisting new ones → `tracedecay:recalling-project-memory`.

## Output

- The recalled messages/summaries with session ids and timestamps, and which rung answered the question.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
