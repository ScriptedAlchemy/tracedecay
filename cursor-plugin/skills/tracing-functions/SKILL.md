---
name: tracing-functions
description: Trace call relationships — who calls a function, what it calls, the shortest path between two symbols, trait/interface implementors, and all references. Use for "who calls X", "what does X call", call chains, dynamic dispatch, and type hierarchies.
---

# Tracing functions

## Workflow

1. **Resolve symbol(s) → node ID(s)** with `tokensave_search` / `tokensave_find_exact_symbol` / `tokensave_by_qualified_name`.
2. **Upstream (callers) → `tokensave_callers`** (`node_id`, `max_depth` 1–2 first). For many symbols at once → `tokensave_callers_for` (`node_ids[]`, one round-trip).
3. **Downstream (callees) → `tokensave_callees`** (resolves trait dispatch; watch for `dispatch_via_trait: true` / `dispatch_from`). Pass `resolve_dispatch: false` for direct edges only.
4. **Path between two symbols → `tokensave_call_chain`** (`from_id`, `to_id`, `max_depth`).
5. **Polymorphism:** `tokensave_implementations` (types implementing a trait, or every body of a method name), `tokensave_type_hierarchy` (recursive implementors/extenders), `tokensave_impls` (impl blocks by trait/type), `tokensave_inheritance_depth`.
6. **All references (rename prep) → `tokensave_rename_preview`** (`node_id`): every edge where the node is source or target.
7. **Cycles / hubs:** `tokensave_recursion`, `tokensave_hotspots`, `tokensave_rank`.

## Guardrails

- Read-only and parallel-safe. Keep `max_depth` small (1–2) first; widen only when the chain is not yet clear. `tokensave_rename_preview` only previews references — it does not rename.

## Output

- The caller/callee tree or the resolved path, with dispatch targets noted.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
