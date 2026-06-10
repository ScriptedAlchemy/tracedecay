---
name: searching-for-code
description: Find code by concept, symbol, signature, or qualified name in this repo using the tokensave code graph. Use when searching the codebase, locating a function/struct/trait/class/method, exploring how a feature works, or before grep/file-search when a .tokensave index exists.
---

# Searching for code

Use the tokensave code graph before Grep/Glob/file reads. Pick the cheapest tool that answers the question.

## Workflow

1. **Conceptual / "how does X work" / names unknown → `tokensave_context`.**
   - `task` = the question. Add `keywords` to expand synonyms (e.g. auth → `["login","session","token","credential"]`).
   - Set `include_code: true` only when you need snippets; use `mode: "plan"` when scoping an implementation.
   - **Respect the per-project call budget printed in the tool description** (it scales with graph size — stop at the stated max and synthesize from what you have). Pass prior `seen_node_ids` via `exclude_node_ids` to dedupe across calls.
2. **Exact name known → `tokensave_find_exact_symbol`** (cheapest index probe) or **`tokensave_body`** (name → full source in one shot; ranks matches when ambiguous).
3. **Relevance-ranked discovery by name/keyword → `tokensave_search`.**
4. **Half-remembered name → `tokensave_similar`** (fuzzy / substring).
5. **Stable cross-run identity → `tokensave_by_qualified_name`** (when content-hash node IDs changed).
6. **By shape, not name → `tokensave_signature_search`** (return type / param substring / `async` / path), e.g. "every fn returning `Result<_, MyError>`".
7. **Found it — inspect it cheaply:** follow the `tokensave:reading-code-cheaply` ladder (`tokensave_outline` → `tokensave_signature` → `tokensave_body` → `tokensave_read` slices; `tokensave_module_api` for a module's public surface) instead of full file reads.
8. **Type-level questions** (trait implementors, impl blocks, construction sites, field usage, derive-generated methods) → `tokensave:exploring-types-and-traits`.

## Guardrails

- All tools above are read-only and parallel-safe. Do not call mutating/editing tools from this skill.
- Only fall back to Grep/Glob/Read for non-indexed content (string literals, comments, config the graph does not cover) or after tokensave pinpoints exact files.
- Prefer one well-formed `tokensave_context` call over many narrow searches.
- About to write a new helper because the search came up empty? Run the `tokensave:finding-duplicate-logic` pre-write probe first.

## Output

- The file + symbol the user needs (path, qualified name, signature), and how you found it.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
