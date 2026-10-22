# Mutation operations

Read the live schema of each operation for its arguments and its preview and
apply fields.

- **Rename.** `tracedecay_rename_preview` returns the references and an accepted
  identity with its expected state. `tracedecay_rename_symbol` applies against
  exactly that identity and state.
- **Anchored replacement.** `tracedecay_str_replace`,
  `tracedecay_replace_symbol`, `tracedecay_insert_at`, and
  `tracedecay_insert_at_symbol` require a unique match.
- **Multi-replacement.** `tracedecay_multi_str_replace` is all-or-nothing. Do
  not emulate it with a partially applied sequence.
- **Move.** `tracedecay_move_symbol` preserves attached docs and attributes, but
  imports are automatic only when unambiguous. Inspect visibility and module
  dependencies; reported callers are not necessarily rewritten by the move.
  A partial failure can leave published files in place.
- **Structural rewrite.** `tracedecay_ast_grep_rewrite` uses the same
  in-process ast-grep engine and grammars as structural search.
- **Reconcile.** After an interrupted edit, call
  `tracedecay_source_edit_reconcile` with `disposition: "inspect"` to obtain
  candidate paths and `observed_state`. Pass the original receipt's `operation`
  as `original_operation`. Inspect those files, then confirm committed state
  with `committed_state`, confirm unchanged state, or acknowledge a partial
  state with the returned `observed_state`. Use a fresh `reconciliation_key`
  for each fresh inspection or conclusion; exact retries replay the prior
  attempt. Reconciliation checks the live digest and never writes source files.

For consolidation, compare candidate bodies and behavior directly; a similar
name does not justify replacing an implementation.
