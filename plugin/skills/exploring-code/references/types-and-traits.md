# Types & traits

Use these tools when the `tracedecay:exploring-code` skill needs information
about a type, its implementations, or its uses.

## Types & traits

1. Use `tracedecay_implementations` with `selector: {"selector": "trait", "name": "X"}`
   to get the body of each impl or class block that implements the trait. Use
   `selector: {"selector": "method", "name": "X"}` to get the bodies of all
   functions and methods named X.
2. Use `tracedecay_type_hierarchy` for the recursive hierarchy or the traits a
   type implements. Use `tracedecay_inheritance_depth` for the deepest
   inheritance chains.
3. Use `tracedecay_derives` to find the `#[derive(...)]` macros on a type and
   the methods each macro generates. Check before concluding that `.clone()`
   or `.eq()` has no definition.
4. Use `tracedecay_constructors` to list struct literals and the fields they
   include or omit. Use `tracedecay_field_sites` with `field` or `Struct::field`
   to find reads and writes, with the file, line, and enclosing symbol for each
   site.

## Guardrails

- `tracedecay_constructors` is best-effort for Rust and ignores `match` arms.
- `tracedecay_field_sites` matches `.<field>`. Prefer `Struct::field` to narrow
  the search.
