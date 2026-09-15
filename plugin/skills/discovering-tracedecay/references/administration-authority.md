# Administration authority

Project identity is registry-owned. Resolve project and store selectors through
the supported project and storage surfaces. A linked worktree retains its exact
snapshot while sharing the registered project identity. Cross-project reads use
the selected project's store. Multi-root queries use a saved scope set; replace
it with `tracedecay_multi_root_scope_set_compare_and_swap` against the identity
returned by the prior read.

Configuration preview/apply has separate authority. Ordinary mutation uses
`tracedecay_configuration_set`, `tracedecay_configuration_unset`, or
`tracedecay_configuration_batch`. Protected and rollback changes consume their
returned preview identity through `tracedecay_configuration_protected_apply`
and `tracedecay_configuration_rollback_apply`. A display label or read does not
grant mutation authority.

Context Scout pause and resume consume the exact configuration revision.
Cancel, claim, delivery, and feedback consume the exact daemon-returned address
and typed work, claim, or receipt. Use the live operation schema for the current
tool names and arguments.

For an incompatible sealed lexical cursor, use synchronization recovery for
derived index staging. Storage reset is a different operation and must preserve
project identity, sessions, and configuration. Raw store deletion is not a
substitute for either path.
