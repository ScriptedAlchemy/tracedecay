---
name: reading-code-cheaply
description: Inspect code at the cheapest sufficient depth — file outlines, cached signatures, single-symbol bodies, line slices, and module API surfaces instead of full-file reads. Use when about to read or open a source file, when a file is large, for "what's in this file", "what's the signature of X", or "what does this module export".
---

# Reading code cheaply

Climb this ladder and stop at the first rung that answers the question. Most "read the file" impulses are satisfied by rungs 1–3.

## Ladder

1. **Orient in a file → `tokensave_outline`** (`path`, optional `kinds`): every top-level symbol with line numbers, no bodies — the table of contents.
2. **API surface only → `tokensave_signature`** (qualified name): visibility, generics, params, return type, docstring, async flag — no bodies, no file bytes. Bulk per-file variant: `tokensave_read` with `mode: "signatures"`.
3. **One symbol's source → `tokensave_body`** (name → ranked full source in one call) or `tokensave_node` (by node ID, with metadata) — instead of opening the whole file for one function.
4. **A specific region → `tokensave_read`** (`mode: "lines"`, e.g. `"120-180"`): slice the file instead of reading all of it.
5. **Whole file (last resort) → `tokensave_read`** (`mode: "full"`): cross-session cached — re-reading an unchanged file returns a tiny `unchanged: true` stub instead of repeat bytes, so prefer it over the plain Read tool.
6. **Module/directory surface → `tokensave_module_api`** (all `pub` symbols sorted by file and line); enumerate files with `tokensave_files` (`path?`, `pattern?`).

## Guardrails

- All read-only and parallel-safe. Don't chain rungs you don't need — outline + one body beats a full read of a 2000-line file.
- The graph covers symbols, not prose: comments, string literals, and config bodies need Grep/Read (or `tokensave_config` for TOML/JSON keys). If results look empty or stale, check `tokensave_status` (index freshness, branch-fallback warning) before falling back to raw reads.
- If a tokensave read response is truncated and includes a `handle`, prefer a narrower symbol/range request first; call `tokensave_retrieve` with that `handle` when the omitted content is needed.

## Handoff

- Don't know where the code lives yet → `tokensave:searching-for-code`. Type-level questions (implementors, construction sites, derives) → `tokensave:exploring-types-and-traits`.

## Output

- The outline/signature/snippet that answers the question, and which rung produced it.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
