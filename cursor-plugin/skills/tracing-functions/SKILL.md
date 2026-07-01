---
name: tracing-functions
description: 'Use when tracing call relationships: find callers/callees, who calls a function, what it calls, what depends on a symbol or fixture/helper, shortest call paths, references for rename prep, recursion, hubs, or dynamic dispatch. Use before grep/file reads for "trace this function" tasks.'
---

# Tracing functions

## Workflow

1. **Resolve symbol(s) → node ID(s)** with `tracedecay_find_exact_symbol` for exact names, `tracedecay_search` for ranked discovery, or `tracedecay_by_qualified_name` for stable identities (see `tracedecay:searching-for-code` for the full resolver ladder).
2. **Upstream (callers) → `tracedecay_callers`** (`node_id`, `max_depth` 1–2 first). For many symbols at once → `tracedecay_callers_for` (`node_ids[]`, one round-trip).
3. **Downstream (callees) → `tracedecay_callees`** (resolves trait dispatch; watch for `dispatch_via_trait: true` / `dispatch_from`). Pass `resolve_dispatch: false` for direct edges only.
4. **Path between two symbols → `tracedecay_call_chain`** (`from_id`, `to_id`, `max_depth`).
5. **Polymorphism → `tracedecay_implementations`** for a quick "every implementor / every body of this method"; the full type-level toolkit (impl blocks, hierarchies, derives, construction/field sites) is `tracedecay:exploring-types-and-traits`.
6. **All references (rename prep) → `tracedecay_rename_preview`** (`node_id`): every edge where the node is source or target. For the full recon-then-edit rename workflow, use `tracedecay:refactoring-safely`.
7. **Cycles / hubs:** `tracedecay_recursion`, `tracedecay_hotspots`, `tracedecay_rank`.

## Guardrails

- Read-only and parallel-safe. For tasks like "find callers of setup_project", "which tests still depend on this fixture", or "trace this function", resolve the symbol and call `tracedecay_callers` / `tracedecay_callees` before running grep or opening files. Keep `max_depth` small (1–2) first; widen only when the chain is not yet clear. `tracedecay_rename_preview` only previews references — it does not rename.
- For several independent symbols or call paths, use scoped read-only subagents per symbol, direction, or path hypothesis. Require node ids, depth/tool parameters, and dispatch notes; the parent agent owns the final trace.
- If a trace response is truncated and includes a `handle`, narrow depth or target set first when possible; call `tracedecay_retrieve` with that `handle` when the omitted chain details are needed.

## Output

- The caller/callee tree or the resolved path, with dispatch targets noted.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
