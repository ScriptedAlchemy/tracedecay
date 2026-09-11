---
name: discovering-tracedecay
description: 'Find a TraceDecay capability or choose the supported operation for an explicit code-intelligence, memory, or administration task.'
---

# Discovering TraceDecay

Use live tool descriptions and `tracedecay tool <name> --help` for available
operations and arguments. Load a deferred schema when needed. This is capability
discovery, not a prerequisite for ordinary reads, local edits, or clarification.

Choose by the missing evidence: exploration locates code; tracing follows call
relationships; impact connects changes to dependents and tests; review evaluates
a diff; editing handles structural mutation. Durable facts and raw session
history have separate stores and retrieval workflows.

MCP and the generic CLI adapt the same daemon operations. A failed MCP transport
does not establish daemon failure; see `using-the-cli` when transport matters.

Project identity is registry-owned. Resolve project/store selectors through the
supported project and storage surfaces; a linked worktree retains its exact
snapshot while sharing the registered project identity. Cross-project reads must
select that project's store, never alias whichever project is active. Multi-root
queries use a saved scope set and its returned identity; replacing the set is a
compare-and-swap (`tracedecay_multi_root_scope_set_compare_and_swap`) against
the identity a read returned.

Configuration preview/apply has a separate authority.
Ordinary mutation is `tracedecay_configuration_set`,
`tracedecay_configuration_unset`, or `tracedecay_configuration_batch`; protected
and rollback changes consume their returned preview identity through
`tracedecay_configuration_protected_apply` and
`tracedecay_configuration_rollback_apply`. Never turn a read or a display
label into mutation authority.

Context Scout generation is daemon-owned. Pause and resume
(`tracedecay_context_scout_pause`, `tracedecay_context_scout_resume`) require
the exact configuration revision; cancel, claim, delivery, and feedback
(`tracedecay_context_scout_cancel`, `tracedecay_context_scout_claim`,
`tracedecay_context_scout_delivery`, `tracedecay_context_scout_feedback`)
consume the daemon-returned exact address and typed work, claim, or receipt.

For an incompatible sealed lexical cursor, use the supported synchronization
recovery for derived index staging. Storage reset is a different operation and
must preserve project identity, sessions, and configuration; do not substitute a
raw store deletion for either path.
