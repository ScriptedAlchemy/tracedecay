---
name: recalling-project-memory
description: 'Use when recalling prior decisions, durable facts, user/project preferences, or past project context before answering or planning; use curating-project-memory for updating or deleting stored facts.'
---

# Recalling project memory

Prefer TraceDecay-native registered-project selectors whenever a recall spans or targets a project other than the active checkout. Codex skill guidance may describe how to choose selectors, but selector support should live progressively in TraceDecay MCP and CLI tools themselves.


Recall memory **before** reaching for external or web search — prior sessions often already answered the question, and a memory hit is cheaper and project-specific.

## Workflow

1. **Past conversations → `tracedecay_message_search`** (`query`, optional `provider`, `limit`) over ingested Cursor/Codex/agent transcripts (active project FTS index).
2. **Durable facts → `tracedecay_fact_store`** with `action: "search"` (or `"probe"` / `"reason"`), plus `query` and `min_trust`.
3. **If the user asks to inspect or repair memory health → `tracedecay_memory_status`** (repairs derived vectors/banks; returns fact/entity counts + trust distribution).
4. **If the user rates a recalled fact → `tracedecay_fact_feedback`** (`helpful` / `unhelpful`) to tune its trust score.
5. **Persist a new durable decision → `tracedecay_fact_store`** `action: "add"` (`content`, `category`, `tags`, `trust`) proactively whenever a durable decision, user preference, correction, or pitfall surfaces — do not wait for the user to ask. The add path already rejects secrets and reports near-duplicates/conflicts.

## Guardrails

- `tracedecay_message_search` and `fact_store` searches are read-only. `fact_feedback` and `memory_status` mutate memory state; use them for explicit user ratings or health checks.
- Do NOT capture: secrets/credentials, transient errors, environment-specific failures, one-off narratives, task progress, or soon-stale session outcomes — recover those from transcripts instead.

## Handoff

- For raw conversation recall beyond FTS — scoped/role/time-filtered grep, lossless session replay, or summary-DAG drill-down — use `tracedecay:recalling-session-context`.
- For stale, contradictory, duplicate, or user-requested fact updates/deletes — use `tracedecay:curating-project-memory`.

## Output

- The relevant prior context/decisions found, with source.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
