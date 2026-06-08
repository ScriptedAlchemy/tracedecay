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
7. **Orient in a file → `tokensave_outline`** (cheap table of contents), then zoom with `tokensave_node` / `tokensave_body` / `tokensave_read` (`mode:"lines"` for slices); for just the API surface of a known symbol, `tokensave_signature` (no bodies).
8. **Public surface of a module → `tokensave_module_api`**; list files with `tokensave_files`.
9. **Type details:** `tokensave_constructors` (struct-literal sites), `tokensave_field_sites` (field reads/writes), `tokensave_derives` (avoid dead-end searches for derive-generated methods), `tokensave_impls`.

## Guardrails

- All tools above are read-only and parallel-safe. Do not call mutating/editing tools from this skill.
- Only fall back to Grep/Glob/Read for non-indexed content (string literals, comments, config the graph does not cover) or after tokensave pinpoints exact files.
- Prefer one well-formed `tokensave_context` call over many narrow searches.

## Output

- The file + symbol the user needs (path, qualified name, signature), and how you found it.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
