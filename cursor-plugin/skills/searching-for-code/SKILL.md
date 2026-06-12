---
name: searching-for-code
description: Find code by concept, symbol, signature, or qualified name in this repo using the TraceDecay code graph. Use when searching the codebase, locating a function/struct/trait/class/method, exploring how a feature works, or before grep/file-search when a .tracedecay index exists.
---

# Searching for code

Use the TraceDecay code graph before Grep/Glob/file reads. Pick the cheapest tool that answers the question.

## Workflow

1. **Conceptual / "how does X work" / names unknown → `tracedecay_context`.**
   - `task` = the question. Add `keywords` to expand synonyms (e.g. auth → `["login","session","token","credential"]`).
   - Set `include_code: true` only when you need snippets; use `mode: "plan"` when scoping an implementation.
   - **Respect the per-project call budget printed in the tool description** (it scales with graph size — stop at the stated max and synthesize from what you have). Pass prior `seen_node_ids` via `exclude_node_ids` to dedupe across calls.
2. **Exact name known → `tracedecay_find_exact_symbol`** (cheapest index probe) or **`tracedecay_body`** (name → full source in one shot; ranks matches when ambiguous).
3. **Relevance-ranked discovery by name/keyword → `tracedecay_search`.**
4. **Half-remembered name → `tracedecay_similar`** (fuzzy / substring).
5. **Stable cross-run identity → `tracedecay_by_qualified_name`** (when content-hash node IDs changed).
6. **By shape, not name → `tracedecay_signature_search`** (return type / param substring / `async` / path), e.g. "every fn returning `Result<_, MyError>`".
7. **Found it — inspect it cheaply:** follow the `tracedecay:reading-code-cheaply` ladder (`tracedecay_outline` → `tracedecay_signature` → `tracedecay_body` → `tracedecay_read` slices; `tracedecay_module_api` for a module's public surface) instead of full file reads.
8. **Type-level questions** (trait implementors, impl blocks, construction sites, field usage, derive-generated methods) → `tracedecay:exploring-types-and-traits`.

## Guardrails

- All tools above are read-only and parallel-safe. Do not call mutating/editing tools from this skill.
- Only fall back to Grep/Glob/Read for non-indexed content (string literals, comments, config the graph does not cover) or after TraceDecay pinpoints exact files.
- Prefer one well-formed `tracedecay_context` call over many narrow searches.
- If a response is truncated and includes a `handle`, narrow the query/result set first when possible; call `tracedecay_retrieve` with that `handle` only when the omitted details are needed.
- About to write a new helper because the search came up empty? Run the `tracedecay:finding-duplicate-logic` pre-write probe first.

## Output

- The file + symbol the user needs (path, qualified name, signature), and how you found it.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
