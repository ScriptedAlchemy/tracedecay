---
name: recalling-project-memory
description: Use when recalling prior decisions, durable facts, user/project preferences, or past project context before answering or planning; use curating-project-memory for updating or deleting stored facts.
---

# Recalling project memory

Recall memory **before** reaching for external or web search — prior sessions often already answered the question, and a memory hit is cheaper and project-specific.

## Workflow

1. **Past conversations → `tokensave_message_search`** (`query`, optional `provider`, `limit`) over ingested Cursor/Codex/agent transcripts (project-local FTS index).
2. **Durable facts → `tokensave_fact_store`** with `action: "search"` (or `"probe"` / `"reason"`), plus `query` and `min_trust`.
3. **If the user asks to inspect or repair memory health → `tokensave_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).
4. **If the user rates a recalled fact → `tokensave_fact_feedback`** (`helpful` / `unhelpful`) to tune its trust score.
5. **Persist a new durable decision → `tokensave_fact_store`** `action: "add"` (`content`, `category`, `tags`, `trust`) only when the user asks to remember it.

## Guardrails

- `tokensave_message_search` and `fact_store` searches are read-only. `fact_store` adds, `fact_feedback`, and `memory_status` mutate memory state; use them only for explicit user requests or ratings.

## Handoff

- For raw conversation recall beyond FTS — scoped/role/time-filtered grep, lossless session replay, or summary-DAG drill-down — use `tokensave:recalling-session-context`.
- For stale, contradictory, duplicate, or user-requested fact updates/deletes — use `tokensave:curating-project-memory`.

## Output

- The relevant prior context/decisions found, with source.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
