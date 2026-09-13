---
name: managing-session-context
description: Recover prior-session messages or summaries, including Git- or time-scoped history, and inspect LCM state.
---

# Managing session context

Use message search (`tracedecay_message_search`) to locate an ingested
conversation, scoped temporal grep (`tracedecay_lcm_grep`) to narrow it, and
lossless session replay (`tracedecay_lcm_load_session`) for exact messages.
Durable decisions and facts belong to `project-memory`. Cross-project retrieval
must select the registered target store rather than implicitly searching the
active project.

Summary-DAG description (`tracedecay_lcm_describe`) locates a node without
opening its body; expansion (`tracedecay_lcm_expand`) opens its bounded sources.
Continue using the returned opaque `next_cursor` unchanged with the same target
and slice bounds. Never manufacture a cursor from row numbers. When a bounded
prompt expansion (`tracedecay_lcm_expand_query`) says `needs_synthesis`,
synthesize from its bounded context rather than presenting a direct answer as
authoritative.

Preserve `coverage`, `anchors`, watermarks, redaction, and hidden-content
notices. Partial coverage does not prove content never existed. Git-scoped
session relations (`tracedecay_sessions_for`) distinguish produced from observed
commits; workflow recovery reads `wf_*` session runs through
`tracedecay_workflows`, not the Workflow definition/run mutation surface.

For refresh lifecycle and read-only LCM health, see
[refresh and health](references/refresh-and-health.md). For Hermes native alias
schemas, see [Hermes aliases](references/hermes-aliases.md).
