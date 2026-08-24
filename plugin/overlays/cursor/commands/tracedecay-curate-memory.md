---
name: tracedecay-curate-memory
description: Curate, update, delete, or inspect TraceDecay memory facts and dashboard curation from an explicit slash workflow.
---

# /tracedecay-curate-memory

Use `tracedecay:project-memory`.

- **Args:** interpret `$ARGUMENTS` as the fact, entity, query, or curation action to review; if absent, ask what memory scope to curate before mutating anything.
- Start read-only with `tracedecay_fact_store_search`, `tracedecay_fact_store_list`, `tracedecay_fact_store_probe`, `tracedecay_fact_store_reason`, or `tracedecay_fact_store_contradict`, or use `tracedecay_memory_status` for its read-only canonical fact/entity/trust/feedback/holographic-algebra status snapshot; open `tracedecay_dashboard` only when the user wants visual curation.
- Follow the hard-delete guardrail: confirm fact ids and reasons before `tracedecay_fact_store_remove` unless the user already gave an exact deletion instruction.

Output: memory facts inspected or changed, confirmations requested, and the final verification search/list result.
