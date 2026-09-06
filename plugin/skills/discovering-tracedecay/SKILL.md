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
queries use a saved scope set and its returned identity.

Configuration preview/apply and credential management have separate authorities.
Use returned preview identities and the designated credential write operation;
never turn a read or a display label into mutation authority.

For an incompatible sealed lexical cursor, use the supported synchronization
recovery for derived index staging. Storage reset is a different operation and
must preserve project identity, sessions, and configuration; do not substitute a
raw store deletion for either path.
