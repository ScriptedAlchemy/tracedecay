---
name: recalling-project-memory
description: Recall prior decisions, durable facts, and past agent conversations for this project before answering or planning. Use for "what did we decide about X", "did we discuss Y", "remember this decision", or to load durable project context.
---

# Recalling project memory

## Workflow

1. **Past conversations → `tracedecay_message_search`** (`query`, optional `provider`, `limit`) over ingested Cursor/Codex/agent transcripts (project-local FTS index).
2. **Durable facts → `tracedecay_fact_store`** with `action: "search"` (or `"probe"` / `"reason"`), plus `query` and `min_trust`.
3. **If the user asks to inspect or repair memory health → `tracedecay_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).
4. **If the user rates a recalled fact → `tracedecay_fact_feedback`** (`helpful` / `unhelpful`) to tune its trust score.
5. **Persist a new durable decision → `tracedecay_fact_store`** `action: "add"` (`content`, `category`, `tags`, `trust`) only when the user asks to remember it.

## Guardrails

- `tracedecay_message_search` and `fact_store` searches are read-only. `fact_store` adds, `fact_feedback`, and `memory_status` mutate memory state; use them only for explicit user requests or ratings.

## Handoff

- For raw conversation recall beyond FTS — scoped/role/time-filtered grep, lossless session replay, or summary-DAG drill-down — use `tracedecay:recalling-session-context`.

## Output

- The relevant prior context/decisions found, with source.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
