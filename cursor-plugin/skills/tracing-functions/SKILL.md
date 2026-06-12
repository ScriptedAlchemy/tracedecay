---
name: tracing-functions
description: Use when tracing call relationships: who calls a function, what it calls, shortest call paths between symbols, references for rename prep, recursion, hubs, or dynamic dispatch.
---

# Tracing functions

## Workflow

1. **Resolve symbol(s) → node ID(s)** with `tracedecay_search` / `tracedecay_find_exact_symbol` / `tracedecay_by_qualified_name` (see `tracedecay:searching-for-code` for the full resolver ladder).
2. **Upstream (callers) → `tracedecay_callers`** (`node_id`, `max_depth` 1–2 first). For many symbols at once → `tracedecay_callers_for` (`node_ids[]`, one round-trip).
3. **Downstream (callees) → `tracedecay_callees`** (resolves trait dispatch; watch for `dispatch_via_trait: true` / `dispatch_from`). Pass `resolve_dispatch: false` for direct edges only.
4. **Path between two symbols → `tracedecay_call_chain`** (`from_id`, `to_id`, `max_depth`).
5. **Polymorphism → `tracedecay_implementations`** for a quick "every implementor / every body of this method"; the full type-level toolkit (impl blocks, hierarchies, derives, construction/field sites) is `tracedecay:exploring-types-and-traits`.
6. **All references (rename prep) → `tracedecay_rename_preview`** (`node_id`): every edge where the node is source or target. For the full recon-then-edit rename workflow, use `tracedecay:refactoring-safely`.
7. **Cycles / hubs:** `tracedecay_recursion`, `tracedecay_hotspots`, `tracedecay_rank`.

## Guardrails

- Read-only and parallel-safe. Keep `max_depth` small (1–2) first; widen only when the chain is not yet clear. `tracedecay_rename_preview` only previews references — it does not rename.
- If a trace response is truncated and includes a `handle`, narrow depth or target set first when possible; call `tracedecay_retrieve` with that `handle` when the omitted chain details are needed.

## Output

- The caller/callee tree or the resolved path, with dispatch targets noted.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
