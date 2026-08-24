---
description: Recall prior decisions, durable facts, and past session conversations for this project.
argument-hint: "[subject]"
---

# Recall memory

Interpret `$ARGUMENTS` as the question or topic to recall. If absent, ask what to look up. Recall memory before reaching for external or web search — a prior session may already have answered it.

1. Durable decisions/facts → `tracedecay_fact_store_search` (or `tracedecay_fact_store_probe` / `tracedecay_fact_store_reason`), plus `query` and `min_trust`.
2. Past conversations → `tracedecay_message_search` (`query`, optional `provider`, `limit`) over ingested transcripts; drill deeper with the LCM ladder (`tracedecay_lcm_grep`, `tracedecay_lcm_load_session`) when role/time/session precision is needed.
3. If the user rates a recalled fact → `tracedecay_fact_feedback` (`helpful` / `unhelpful`).

If the user asks to update, delete, merge, or prune stored facts, switch to `/tracedecay:curate-memory`.

Output: the recalled decisions/messages with their sources (fact, session id, timestamp). If any result includes a `tracedecay_metrics:` line, report the savings.
