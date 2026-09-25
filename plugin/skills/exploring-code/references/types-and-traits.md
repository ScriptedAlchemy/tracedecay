# Types & traits

Type-level exploration for the `tracedecay:exploring-code` skill.

- Implementations, impl blocks, and hierarchies
- Derives and synthesized methods
- Construction sites and field usage

## Types & traits

1. **Who implements a trait / every body of a method → `tracedecay_implementations`**
   (`selector: {"selector": "trait", "name": "X"}`: each implementing impl or
   class block with its body; `selector: {"selector": "method", "name": "X"}`:
   every function or method named X, with bodies).
2. **Recursive hierarchy, or which traits a type implements →
   `tracedecay_type_hierarchy`**; deepest extends-chains →
   `tracedecay_inheritance_depth`.
3. **"Where does this method come from?" → `tracedecay_derives`**: the
   `#[derive(...)]` macros on a type and the methods each synthesizes. Check
   before concluding `.clone()` / `.eq()` has no definition.
4. **Construction sites → `tracedecay_constructors`** (every struct-literal
   site with present and missing fields); **field usage →
   `tracedecay_field_sites`** (`field` or `Struct::field`): every read/write
   site with file, line, and enclosing symbol.

## Guardrails

- `tracedecay_constructors` is best-effort for Rust (ignores `match` arms);
  `tracedecay_field_sites` pattern-matches `.<field>`, so prefer the
  `Struct::field` form to narrow.
