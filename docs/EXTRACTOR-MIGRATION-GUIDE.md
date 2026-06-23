# Extractor Traversal Helper Migration Guide

This guide captures the C/C++ pilot migration strategy for consolidating duplicated tree-sitter traversal helpers under `src/extraction/traversal.rs`.

## Final utility strategy

Keep the shared traversal module intentionally small and language-agnostic. The current shared helpers are:

- `find_direct_child_by_kind(node, kind)`: exact `Node::kind()` match over direct children only, preserving source-order traversal and including both named and anonymous children.
- `has_direct_child_kind(node, kind)`: boolean wrapper around `find_direct_child_by_kind`.
- `find_descendant_by_kind(node, kind)`: exact `Node::kind()` match with pre-order depth-first traversal over all children.

The pilot migrated only the C and C++ extractors to these helpers. That scope is deliberate: both extractors had duplicate local helpers with the same semantics, and the focused C/C++ tests exercise the direct-child and descendant paths through function-pointer typedef extraction.

## Safe consolidation patterns

Consolidate a local helper into `extraction::traversal` only when all of these are true:

1. The helper compares `node.kind()` to a caller-provided string exactly.
2. Traversal visits both named and anonymous children, not just named children.
3. Direct-child searches inspect only immediate children in source order.
4. Descendant searches are pre-order depth-first and return the first matching node.
5. The helper is stateless: it does not read extractor state, source bytes, language config, field names, or parent context.
6. Existing tests cover at least one representative call path in the migrating extractor.

The C/C++ pilot is the template for this shape: import helpers from `crate::extraction::traversal`, replace `Self::find_*_by_kind(...)` calls with the shared functions, remove the now-unused local helper definitions, and keep extractor behavior unchanged.

## Patterns that should stay local

Do not consolidate helpers that encode language-specific or extractor-specific behavior, including helpers that:

- Filter to named children only or intentionally skip anonymous nodes.
- Match tree-sitter field names, grammar aliases, supertypes, or language-specific node families.
- Collect multiple matches rather than returning the first match.
- Depend on source text, byte ranges, comments, docstrings, visibility state, or the extractor's node stack.
- Walk parents or siblings, or otherwise use traversal order different from the direct-child / pre-order descendant helpers.
- Special-case grammar quirks or disambiguate names differently per language.
- Are performance-sensitive enough to need caller-controlled cursors, caching, or a fused extraction pass.

Keeping these helpers local is preferable to hiding behavior differences behind a shared utility with a misleadingly generic name.

## Migration checklist

For each future extractor migration:

1. Compare the local helper body against `src/extraction/traversal.rs`, not just the helper name.
2. Add or identify tests that cover each shared helper path used by the extractor. At minimum, cover direct-child lookup and nested descendant lookup when both are used.
3. Run the focused extractor test file before and after migration. If behavior changes, either revert the migration or document and test the intended behavior change.
4. Run the shared traversal unit tests and `cargo check --lib` with the same feature profile used by the extractor tests.
5. Leave language-specific traversal helpers local until their semantics are proven identical.

Validation commands used for the C/C++ pilot:

```sh
export CARGO_TARGET_DIR="$PWD/.tracedecay/target/<task-or-lane>"
cargo nextest run --lib --no-default-features extraction::traversal::tests
cargo nextest run --test c_extraction_test --no-default-features
cargo nextest run --test cpp_extraction_test --no-default-features
cargo check --lib --no-default-features
```

The pilot validation passed with 3/3 traversal unit tests, 25/25 C extractor tests, 30/30 C++ extractor tests, and a clean library check.
