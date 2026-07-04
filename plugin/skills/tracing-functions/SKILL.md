---
name: tracing-functions
description: 'Use when the task is call-relationship shaped: "who calls X", "what does X call", "trace this", references before a rename, shortest path, recursion, or hubs. Trigger before grepping a function name — grep misses dynamic dispatch. Do NOT use to locate a symbol (tracedecay:exploring-code).'
---

# Tracing functions

```
NO GREPPING FOR CALL SITES. The graph resolves dispatch; grep resolves text.
```

Announce: "Using tracedecay:tracing-functions to trace <symbol>."

## Workflow

| Question | Call |
|---|---|
| Resolve name → node ID first | `tracedecay_find_exact_symbol` / `tracedecay_search` (ladder: `tracedecay:exploring-code`) |
| Who calls X | `tracedecay_callers` (`max_depth` 1–2 first; widen only if unclear) |
| Many symbols at once | `tracedecay_callers_for` (`node_ids[]`, one round-trip) |
| What X calls | `tracedecay_callees` (resolves trait dispatch; note `dispatch_via_trait: true`) |
| Path from A to B | `tracedecay_call_chain` (`from_id`, `to_id`, `max_depth`) |
| Every implementor / every body of a method | `tracedecay_implementations` |
| Every reference (rename prep) | `tracedecay_rename_preview` (preview only — nothing renames) |
| Cycles / hubs | `tracedecay_recursion` / `tracedecay_hotspots` / `tracedecay_rank` |

## Rules

- Read-only and parallel-safe. Keep depth small first; widen deliberately.
- Truncated with a `handle`? Narrow depth/target set; `tracedecay_retrieve`
  only when the omitted chain is needed.
- Rename is the goal → hand the preview to `tracedecay:editing-safely`.
  "What breaks / which tests" → `tracedecay:assessing-impact`.
- For several independent symbols, scoped read-only subagents (one symbol or
  direction each, cited node ids); the parent owns the final trace.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_callers,tracedecay_callees,tracedecay_call_chain,tracedecay_find_exact_symbol,tracedecay_rename_preview`.
- MCP error: `tracedecay tool callers --node-id …` etc. (see
  `tracedecay:using-the-cli`). Never fall back to grepping call sites.

## Deliverable

Do not end without: the caller/callee tree or resolved path with dispatch
targets noted, and node IDs for any follow-up skill. Report any
`tracedecay_metrics:` line.
