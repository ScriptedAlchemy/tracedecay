---
name: tokensave-curate-memory
description: Curate, update, delete, or inspect tokensave memory facts and dashboard curation from an explicit slash workflow.
disable-model-invocation: true
---

# /tokensave-curate-memory

Apply the `tokensave:curating-project-memory` skill.

- **Args:** interpret the text after the command as the fact, entity, query, or curation action to review; if absent, ask what memory scope to curate before mutating anything.
- Start read-only with `tokensave_fact_store` search/list/probe/reason/contradict or `tokensave_memory_status`; open `tokensave_dashboard` only when the user wants visual curation.
- Follow the hard-delete guardrail: confirm fact ids and reasons before `remove` unless the user already gave an exact deletion instruction.

Output: memory facts inspected or changed, confirmations requested, and the final verification search/list result.
