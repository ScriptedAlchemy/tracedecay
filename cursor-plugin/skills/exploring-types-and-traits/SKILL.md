---
name: exploring-types-and-traits
description: Navigate type-level structure — every implementor of a trait, impl blocks, full type hierarchies, inheritance depth, derive-generated methods, struct construction sites, and field read/write sites. Use for "what implements trait X", "which traits does this type satisfy", "where is this struct constructed", "who reads/writes this field", or "where does .clone() come from".
---

# Exploring types & traits

Call-graph questions ("who calls X") belong in `tokensave:tracing-functions`; this skill answers the type-level questions around them.

## Workflow

1. **Resolve the type/trait first** with `tokensave_search` / `tokensave_find_exact_symbol` (full resolver ladder: `tokensave:searching-for-code`).
2. **Who implements a trait / every body of a method → `tokensave_implementations`** (`trait` form: each implementing type plus its impl-block methods; `method` form: every function named X across the project, grouped by enclosing type, with bodies).
3. **Impl blocks by trait, type, or both → `tokensave_impls`** (short or qualified names): which traits a type implements, which types satisfy a trait, dispatch targets. Avoid the no-filter form — it returns every impl in the graph.
4. **Recursive hierarchy → `tokensave_type_hierarchy`** (all implementors/extenders, transitively); deepest extends-chains → `tokensave_inheritance_depth`.
5. **"Where does this method come from?" → `tokensave_derives`**: the `#[derive(...)]` macros on a type and the trait + method names each one synthesizes — check here before concluding `.clone()` / `.eq()` / `.hash()` has no definition.
6. **Construction sites → `tokensave_constructors`** (struct name): every struct-literal site with its present and missing fields — after adding a required field, the missing-fields list is the exact to-do list, before cargo even runs.
7. **Field usage → `tokensave_field_sites`** (`field` or `Struct::field`): every read site and write site with file, line, and enclosing symbol — the blast radius for renaming/removing a field or adding an invariant (write sites are what enforce it).

## Guardrails

- All read-only and parallel-safe. `tokensave_constructors` is best-effort for Rust (ignores `match` arms and `if let` patterns); `tokensave_field_sites` pattern-matches `.<field>` references, so same-named fields on other types can appear — prefer the `Struct::field` form to narrow. Unknown proc-macro derives surface with `well_known: false` (name only, no synthesized-method info).
- This skill maps types; it does not edit. Hand renames/edits to `tokensave:refactoring-safely` / `tokensave:atomic-code-edits`.

## Output

- The implementor/hierarchy map or the site list (file, line, enclosing symbol) the question asked for.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
