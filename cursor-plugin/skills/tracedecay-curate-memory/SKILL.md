---
name: tracedecay-curate-memory
description: Curate, update, delete, or inspect tracedecay memory facts and dashboard curation from an explicit slash workflow.
disable-model-invocation: true
---

# /tracedecay-curate-memory

Apply the `tracedecay:curating-project-memory` skill.

- **Args:** interpret the text after the command as the fact, entity, query, or curation action to review; if absent, ask what memory scope to curate before mutating anything.
- Start read-only with `tracedecay_fact_store` search/list/probe/reason/contradict or `tracedecay_memory_status`; open `tracedecay_dashboard` only when the user wants visual curation.
- Follow the hard-delete guardrail: confirm fact ids and reasons before `remove` unless the user already gave an exact deletion instruction.

Output: memory facts inspected or changed, confirmations requested, and the final verification search/list result.
