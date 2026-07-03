---
name: tracedecay-curate-memory
description: 'Use to curate, update, delete, or inspect TraceDecay memory facts and dashboard curation from an explicit slash workflow.'
---

# Curate memory

Use to curate, update, delete, or inspect TraceDecay memory facts, or to do dashboard curation.

Use `tracedecay:project-memory`.

- **Scope:** the fact, entity, query, or curation action to review. If none is given, ask what memory scope to curate before mutating anything.
- Start read-only with `tracedecay_fact_store` search/list/probe/reason/contradict or `tracedecay_memory_status`; open `tracedecay_dashboard` only when the user wants visual curation.
- Follow the hard-delete guardrail: confirm fact ids and reasons before `remove` unless the user already gave an exact deletion instruction.

Output: memory facts inspected or changed, confirmations requested, and the final verification search/list result.
