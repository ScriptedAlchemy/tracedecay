---
name: editing-safely
description: 'Perform a TraceDecay structural rename, symbol move, signature change, or atomic edit across related sites. Ordinary local text edits do not require this workflow.'
---

# Editing safely

Resolve the exact symbol and inspect the affected references before a structural
mutation. Signature changes need callers; field changes need constructor and
write sites. Graph results can miss public consumers, macros, generated code,
and string-keyed dispatch, so inspect those boundaries when relevant.

A rename preview is evidence, not an applied rename. Apply against the accepted
identity and expected state returned by the operation; refuse ambiguous symbols
rather than choosing a same-named declaration. Use live schemas for mutation
arguments and preview/apply behavior.

Anchored replacement requires a unique match. Multi-replacement is all-or-nothing;
do not emulate it with a partially applied sequence. Symbol moves preserve
attached docs and attributes, but imports are automatic only when unambiguous.
Inspect visibility and module dependencies; reported callers are not necessarily
rewritten by the move.

Rollback uses retained preimages and the committed expected state. Consume the
returned operation identity, not a reconstructed path or inverse semantic move.
If an interrupted operation has committed effects, reconcile its state before
retrying. Preserve peers' changes when the expected state no longer matches.

For consolidation, body similarity is evidence; a similar name is not. Verify
likely or vector-only duplicate matches before replacing an implementation.
Structural rewrite uses external ast-grep where advertised; its availability is
separate from in-process structural search. Verify the actual changed behavior
and use `assessing-impact` for structural test selection.
