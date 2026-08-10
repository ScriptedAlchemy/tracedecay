---
description: Curate, update, delete, or inspect TraceDecay memory facts and dashboard curation from an explicit slash workflow.
argument-hint: "[subject]"
---

# Curate memory

Interpret `$ARGUMENTS` as the fact, entity, query, or curation action to review. If absent, ask what memory scope to curate before mutating anything.

1. Resolve scope: confirm the active project root/store with `tracedecay_active_project` before touching memory.
2. Start read-only with `tracedecay_fact_store` (`action`: `search` / `list` / `get` / `probe` / `related` / `reason` / `contradict`) or `tracedecay_memory_status` when the user asks for counts, health, or the daemon-owned repair backlog. Open `tracedecay_dashboard` (`action: "start"`) only when the user wants visual curation.
3. Inventory candidates into add, update, merge/dedupe, stale, contradiction, secret-like, and possible-delete buckets, keeping fact ids, source, trust, tags, and evidence with each.
4. Apply narrowly with `tracedecay_fact_store` `action: "add"` / `"update"`. Prefer update/merge over removal when useful provenance should survive.
5. Hard-delete guardrail: require explicit approval immediately before every `action: "remove"` or dashboard hard delete, showing fact id, content/source summary, reason, and a permanent-delete warning — unless the user already gave an exact deletion instruction. Deletion is permanent; there is no undo. Never store secrets, credentials, or PII.
6. Verify read-only: re-run search/list/probe/get and report final facts changed, skipped, or still needing judgment.

Output: memory facts inspected or changed, confirmations requested, and the final verification search/list result. If any result includes a `tracedecay_metrics:` line, report the savings.
