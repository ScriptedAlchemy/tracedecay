---
name: exploring-types-and-traits
description: 'Use when answering type-level questions: trait implementors/impls, type hierarchies, struct construction sites, field read/write sites, derive-generated methods, or method bodies; maps to tracedecay_implementations, tracedecay_impls, tracedecay_constructors, and tracedecay_field_sites.'
---

# Exploring types & traits

Call-graph questions ("who calls X") belong in `tracedecay:tracing-functions`; this skill answers the type-level questions around them.

## Workflow

1. **Resolve the type/trait first** with `tracedecay_search` / `tracedecay_find_exact_symbol` (full resolver ladder: `tracedecay:searching-for-code`).
2. **Who implements a trait / every body of a method → `tracedecay_implementations`** (`trait` form: each implementing type plus its impl-block methods; `method` form: every function named X across the project, grouped by enclosing type, with bodies).
3. **Impl blocks by trait, type, or both → `tracedecay_impls`** (short or qualified names): which traits a type implements, which types satisfy a trait, dispatch targets. Avoid the no-filter form — it returns every impl in the graph.
4. **Recursive hierarchy → `tracedecay_type_hierarchy`** (all implementors/extenders, transitively); deepest extends-chains → `tracedecay_inheritance_depth`.
5. **"Where does this method come from?" → `tracedecay_derives`**: the `#[derive(...)]` macros on a type and the trait + method names each one synthesizes — check here before concluding `.clone()` / `.eq()` / `.hash()` has no definition.
6. **Construction sites → `tracedecay_constructors`** (struct name): every struct-literal site with its present and missing fields — after adding a required field, the missing-fields list is the exact to-do list, before cargo even runs.
7. **Field usage → `tracedecay_field_sites`** (`field` or `Struct::field`): every read site and write site with file, line, and enclosing symbol — the blast radius for renaming/removing a field or adding an invariant (write sites are what enforce it).

## Guardrails

- All read-only and parallel-safe. `tracedecay_constructors` is best-effort for Rust (ignores `match` arms and `if let` patterns); `tracedecay_field_sites` pattern-matches `.<field>` references, so same-named fields on other types can appear — prefer the `Struct::field` form to narrow. Unknown proc-macro derives surface with `well_known: false` (name only, no synthesized-method info).
- For several independent type/trait questions, use scoped read-only subagents per type, trait, field, or implementor cluster. Require exact symbol ids/qualified names and tool evidence; the parent agent synthesizes the type-level answer.
- This skill maps types; it does not edit. Hand renames/edits to `tracedecay:refactoring-safely` / `tracedecay:atomic-code-edits`.

## Output

- The implementor/hierarchy map or the site list (file, line, enclosing symbol) the question asked for.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
