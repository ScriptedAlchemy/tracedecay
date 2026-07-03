---
name: exploring-code
description: 'Use when searching the codebase, locating a symbol, exploring how a feature works, reading or opening any source file, answering type/trait questions, or checking code on another git branch — the graph answers before Grep/Glob/Read in an indexed project.'
---

# Exploring code

Use the TraceDecay code graph before Grep/Glob/file reads. Pick the cheapest
tool that answers the question and stop. If the task says "trace", "find
callers", or "what depends on X", switch to `tracedecay:tracing-functions`
after resolving the symbol.

## Finding it

1. **Conceptual / "how does X work" / names unknown → `tracedecay_context`.**
   `task` = the question; add `keywords` to expand synonyms (auth →
   `["login","session","token"]`). Set `include_code: true` only when you need
   snippets; `mode: "plan"` when scoping an implementation. Pass prior
   `seen_node_ids` via `exclude_node_ids` to dedupe across calls.
2. **Exact name known → `tracedecay_find_exact_symbol`** (cheapest probe) or
   **`tracedecay_body`** (name → full source in one shot; ranks matches when
   ambiguous).
3. **Ranked discovery by name/keyword → `tracedecay_search`.**
4. **Half-remembered name → `tracedecay_similar`** (fuzzy/substring);
   **stable cross-run identity → `tracedecay_by_qualified_name`**.
5. **By shape, not name → `tracedecay_signature_search`** (return type /
   param substring / `async` / path), e.g. "every fn returning `Result<_, MyError>`".

## Reading it cheaply

Climb this ladder and stop at the first rung that answers the question:

1. **Orient in a file → `tracedecay_outline`** (`path`, optional `kinds`):
   every top-level symbol with line numbers, no bodies.
2. **API surface only → `tracedecay_signature`** (qualified name); bulk
   per-file variant: `tracedecay_read` with `mode: "signatures"`.
3. **One symbol's source → `tracedecay_body`** or `tracedecay_node` (by node
   ID, with metadata) — never open a whole file for one function.
4. **A specific region → `tracedecay_read`** (`mode: "lines"`, e.g. `"120-180"`).
5. **Whole file (last resort) → `tracedecay_read`** (`mode: "full"`):
   cross-session cached — unchanged files return a tiny `unchanged: true`
   stub, so prefer it over the plain Read tool.
6. **Module/directory surface → `tracedecay_module_api`** (all `pub` symbols);
   enumerate files with `tracedecay_files` (`path?`, `pattern?`).

## Types & traits

1. **Who implements a trait / every body of a method → `tracedecay_implementations`**
   (`trait` form: implementing types + impl-block methods; `method` form:
   every function named X grouped by enclosing type, with bodies).
2. **Impl blocks by trait, type, or both → `tracedecay_impls`** (avoid the
   no-filter form — it returns every impl in the graph).
3. **Recursive hierarchy → `tracedecay_type_hierarchy`**; deepest
   extends-chains → `tracedecay_inheritance_depth`.
4. **"Where does this method come from?" → `tracedecay_derives`**: the
   `#[derive(...)]` macros on a type and the methods each synthesizes — check
   before concluding `.clone()` / `.eq()` has no definition.
5. **Construction sites → `tracedecay_constructors`** (every struct-literal
   site with present and missing fields); **field usage →
   `tracedecay_field_sites`** (`field` or `Struct::field`): every read/write
   site with file, line, and enclosing symbol.

## Other branches

1. **What's tracked → `tracedecay_branch_list`**; **search another branch →
   `tracedecay_branch_search`** (`branch`, `query`); **compare branches →
   `tracedecay_branch_diff`** (`base?`, `head?`, `file?`, `kind?`) — all
   read-only, never touching your checkout.
2. Branch tracking is opt-in per branch (`tracedecay branch add <branch>` in
   the terminal; the hooks auto-track branches you visit). A branch-fallback
   `WARNING` prefix means results came from the nearest tracked ancestor —
   surface that to the user.

## Guardrails

- Everything here is read-only and parallel-safe.
- Only fall back to Grep/Glob/Read for non-indexed content (string literals,
  comments, prose, config bodies — or `tracedecay_config` for TOML/JSON keys)
  or after TraceDecay pinpoints exact files. If results look empty or stale,
  check `tracedecay_status` before falling back to raw reads.
- Prefer one well-formed `tracedecay_context` call over many narrow searches.
- `tracedecay_constructors` is best-effort for Rust (ignores `match` arms);
  `tracedecay_field_sites` pattern-matches `.<field>`, so prefer the
  `Struct::field` form to narrow.
- For several independent questions, use scoped read-only subagents with one
  bounded target each and a strict no-writes instruction; require cited
  file/symbol ids and tool names, and synthesize in the parent agent.
- If a response is truncated with a `handle`, narrow the query first; call
  `tracedecay_retrieve` with the `handle` only when the omitted details are
  needed.
- About to write a new helper because the search came up empty? Run the
  `tracedecay:editing-safely` pre-write duplicate probe first.

## Output

- The file + symbol the user needs (path, qualified name, signature), the
  outline/snippet that answers the question, and how you found it.
- If any result includes a `tracedecay_metrics:` line, report the savings to
  the user.
