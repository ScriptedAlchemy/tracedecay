# Types & traits

Type-level exploration for the `tracedecay:exploring-code` skill.

- Implementations, impl blocks, and hierarchies
- Derives and synthesized methods
- Construction sites and field usage

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

## Guardrails

- `tracedecay_constructors` is best-effort for Rust (ignores `match` arms);
  `tracedecay_field_sites` pattern-matches `.<field>`, so prefer the
  `Struct::field` form to narrow.
