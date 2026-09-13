---
name: editing-safely
description: Make structural code edits with TraceDecay preview and mutation operations.
---

# Editing safely

Resolve the exact symbol and inspect the affected references before a structural
mutation. Signature changes need callers; field changes need constructor and
write sites. Graph results can miss public consumers, macros, generated code,
and string-keyed dispatch, so inspect those boundaries when relevant.

A rename preview is evidence, not an applied rename. Apply
`tracedecay_rename_symbol` against the accepted identity and expected state
returned by the preview; refuse ambiguous symbols rather than choosing a
same-named declaration. Use live schemas for mutation arguments and
preview/apply behavior.

Anchored replacement (`tracedecay_str_replace`, `tracedecay_replace_symbol`,
`tracedecay_insert_at`, `tracedecay_insert_at_symbol`) requires a unique match.
Multi-replacement (`tracedecay_multi_str_replace`) is all-or-nothing; do not
emulate it with a partially applied sequence. Symbol moves
(`tracedecay_move_symbol`) preserve attached docs and attributes, but imports
are automatic only when unambiguous. Inspect visibility and module dependencies;
reported callers are not necessarily rewritten by the move.

Rollback (`tracedecay_source_edit_rollback`) uses retained preimages and the
committed expected state. Consume the returned operation identity, not a
reconstructed path or inverse semantic move. If an interrupted operation has
committed effects, reconcile its state (`tracedecay_source_edit_reconcile`)
before retrying. Preserve peers' changes when the expected state no longer
matches.

For consolidation, body similarity is evidence; a similar name is not. Verify
likely or vector-only duplicate matches before replacing an implementation.
Structural rewrite (`tracedecay_ast_grep_rewrite`) uses external ast-grep where
advertised; its availability is separate from in-process structural search.
Verify the actual changed behavior and use `assessing-impact` for structural
test selection.
