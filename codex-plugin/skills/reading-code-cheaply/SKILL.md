---
name: reading-code-cheaply
description: Inspect code at the cheapest sufficient depth using TraceDecay outlines, signatures, symbol bodies, line slices, and module API surfaces before reading whole files.
---

# Reading Code Cheaply

Climb this ladder and stop at the first rung that answers the question.

1. Orient in a file with `tracedecay_outline`.
2. Read API surface with `tracedecay_signature` or `tracedecay_read` in `signatures` mode.
3. Read one symbol with `tracedecay_body` or `tracedecay_node`.
4. Read a specific region with `tracedecay_read` in `lines` mode.
5. Read a whole file with `tracedecay_read` in `full` mode only when narrower calls are insufficient.
6. Map a directory or module with `tracedecay_module_api` and `tracedecay_files`.

Check `tracedecay_status` before falling back to raw file reads when results
look stale or empty.
