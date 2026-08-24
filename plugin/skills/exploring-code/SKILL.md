---
name: exploring-code
description: 'Use the moment the next action is Grep, Glob, rg, find, cat, or opening a source file to answer "where is X", "how does Y work", a type/trait question, or to check another git branch. Returns the enclosing symbol. Do NOT use for callers/callees (tracedecay:tracing-functions).'
---

# Exploring code

```
NO RAW GREP, GLOB, OR FULL-FILE READS OVER INDEXED CODE.
The graph answers first; plain tools are for what the index does not cover.
```

Announce: "Using tracedecay:exploring-code to <question>." Pick the cheapest
row below, run it, and stop when the question is answered.

## Route by what you are matching

| You are matching | Call | Not |
|---|---|---|
| Literal string / regex / config key / error text | `tracedecay_grep` (`fixed_strings` for plain literals) | raw Grep/rg |
| A code structure — call shape, argument order, expression form (e.g. `foo($$$)`, `if ($C) { $$$ }`) a text regex cannot pin down | `tracedecay_ast_grep_search` (in-process AST match; then `tracedecay_ast_grep_rewrite` via `tracedecay:editing-safely` to change matches) | multi-line regex guesses |
| A symbol by exact name | `tracedecay_find_exact_symbol`, else `tracedecay_search` | Glob + open |
| A concept / "how does X work" (names unknown) | `tracedecay_context` (`task` = the question, add `keywords` synonyms) | Explore agent |
| A file by role or path | `tracedecay_files` | find/ls -R |
| Half-remembered name | `tracedecay_similar` | guessing greps |
| Code by shape (return type, params, async) | `tracedecay_signature_search` | reading modules |
| Duplicate code / similar function bodies / repeated helper logic | `tracedecay_redundancy` (`path`, `min_lines`, `max_pairs`) | name-only search |
| A trait/interface/class's full type hierarchy (implementors + extenders, recursively) | `tracedecay_type_hierarchy` | manually grepping `impl X for` / `extends X` |

## Read cheaply — stop at the first rung that answers

1. Orient in a file → `tracedecay_outline` or application-surface
   `tracedecay_source_outline` (symbols + line numbers, no bodies).
2. API surface → `tracedecay_signature`; per-file bulk → `tracedecay_read` `mode:"signatures"`.
3. One symbol's source → `tracedecay_body` or `tracedecay_source_body` — never
   open a whole file for one function.
4. A line range → `tracedecay_read` / `tracedecay_source_lines`; whole file
   (last resort) → `tracedecay_read` `mode:"full"` (cross-session cached;
   prefer it over Read). File role/metadata only → `tracedecay_file_metadata`.
5. Module surface → `tracedecay_module_api`.

For types and traits — `tracedecay_implementations` / `tracedecay_impls` /
`tracedecay_type_hierarchy` / `tracedecay_derives` / `tracedecay_constructors`
/ `tracedecay_field_sites`, plus `tracedecay_by_qualified_name` /
`tracedecay_qualified_name` and `tracedecay_node` for stable identity — read
[references/types-and-traits.md](references/types-and-traits.md).
For other git branches without switching checkout — `tracedecay_branch_list` /
`tracedecay_branch_search` / `tracedecay_branch_diff` — read
[references/other-branches.md](references/other-branches.md).

For daemon-admitted, generation-bound code intelligence, use the production
`tracedecay_code_symbol_search`, `tracedecay_code_phrase_search`,
`tracedecay_code_signature_search`, and `tracedecay_code_exact_occurrence`
reads. Traverse only evidenced relationships with `tracedecay_code_callers`,
`tracedecay_code_callees`, `tracedecay_code_implementations`, and
`tracedecay_code_type_hierarchy`. From an exact occurrence, navigate with
`tracedecay_code_declaration`, `tracedecay_code_definition`,
`tracedecay_code_type_definition`, and `tracedecay_code_references`; summarize
the pinned generation with `tracedecay_code_facets` or
`tracedecay_code_timeline`. An unavailable generation stays unavailable instead
of falling back to a different project or worktree.

## Rules

- One well-formed `tracedecay_context` call beats many narrow searches. Pass
  prior `seen_node_ids` via `exclude_node_ids` on follow-ups; respect the call
  budget in the tool description.
- Truncated response with a `handle`? Narrow the query; call
  `tracedecay_retrieve` only if the omitted detail is needed. Never re-run broad.
- Empty or stale-looking results → check `tracedecay_status` BEFORE concluding
  the repo is unindexed; the index auto-syncs — do not run manual sync commands.
- Search came up empty and a new helper seems needed → run the
  `tracedecay:editing-safely` duplicate probe first.
- Similarity by functionality/body, not just by name → run
  `tracedecay_redundancy` before concluding no duplicate exists. Scoped hunt
  recipe: `path` to the suspect directory, `min_lines: 6`,
  `similarity_threshold: 0.55` — whole-repo scans at defaults favor precision
  and can miss real duplicates.
- "Who calls X / what does X call / what breaks" → hand off to
  `tracedecay:tracing-functions` / `tracedecay:assessing-impact` after resolving
  the symbol; do not grep for call sites.
- Fall back to plain Grep/Glob/Read only for non-indexed content (prose docs,
  vendored/skipped trees) or after the graph pinpoints exact files.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_context,tracedecay_search,tracedecay_grep,tracedecay_outline,tracedecay_body,tracedecay_read,tracedecay_redundancy`
  (one batched call, add others needed) — then call normally.
- MCP error/timeout/disconnect: same tool, same args, via shell:
  `tracedecay tool <name>` (see `tracedecay:using-the-cli`). Never
  query `.tracedecay` databases directly; never abandon the graph over transport.

## Deliverable

Do not end this workflow without: the file + symbol (path, qualified name,
signature), the snippet or outline that answers the question, and how it was
found. Budget: aim for ≤4 graph calls before synthesizing; if the answer is
still incomplete, say precisely what is missing rather than falling back to
grep. Report any `tracedecay_metrics:` savings line to the user.
