---
name: tracedecay-curate-memory
description: Curate, update, delete, or inspect TraceDecay memory facts and dashboard curation from an explicit slash workflow.
---

# /tracedecay-curate-memory

Use `tracedecay:project-memory`.

- **Args:** interpret `$ARGUMENTS` as the fact, entity, query, or curation action. For broad curation, resolve the active registered project and use the canonical `fact_store_curate` launcher; inspect an existing run without launching another when the request is read-only.
- Start read-only with `tracedecay_fact_store_search`, `tracedecay_fact_store_list`, `tracedecay_fact_store_probe`, `tracedecay_fact_store_reason`, or `tracedecay_fact_store_contradict`, or use `tracedecay_memory_status` for its read-only canonical fact/entity/trust/feedback/holographic-algebra status snapshot; open `tracedecay_dashboard` only when the user wants visual curation.
- Direct fact changes are for exact administrative instructions. Prefer `tracedecay_fact_store_supersede` (old `fact_id` + `superseded_by`) when a newer fact corrects an older one; the old fact leaves default results but stays readable by id.
- Follow the hard-delete guardrail: confirm fact ids and reasons before `tracedecay_fact_store_remove` unless the user already gave an exact deletion instruction.

Inspect the returned curation run and its advertised artifacts; report committed effects before considering a retry. Follow the `project-memory` curation reference for the launcher and terminal evidence.

Output: run identity and terminal status when applicable, facts inspected or changed, and the final verification result.
