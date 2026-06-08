---
name: recalling-project-memory
description: Recall prior decisions, durable facts, and past agent conversations for this project before answering or planning. Use for "what did we decide about X", "did we discuss Y", "remember this decision", or to load durable project context.
---

# Recalling project memory

## Workflow

1. **Past conversations → `tokensave_message_search`** (`query`, optional `provider`, `limit`) over ingested Cursor/Codex/agent transcripts (project-local FTS index).
2. **Durable facts → `tokensave_fact_store`** with `action: "search"` (or `"probe"` / `"reason"`), plus `query` and `min_trust`.
3. **If results look stale/empty → `tokensave_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).
4. **After using a fact → `tokensave_fact_feedback`** (`helpful` / `unhelpful`) to tune its trust score.
5. **Persist a NEW durable decision → `tokensave_fact_store`** `action: "add"` (`content`, `category`, `tags`, `trust`). Use this when the user says "remember this" (aligns with the repo `AGENTS.md` preference to persist durable facts).

## Guardrails

- `tokensave_message_search` and `fact_store` searches are read-only. `fact_store` writes, `fact_feedback`, and `memory_status` mutate memory state — only use them to record feedback or a decision the user asked to keep.

## Output

- The relevant prior context/decisions found, with source.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
