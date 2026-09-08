---
name: managing-session-context
description: 'Recover raw prior-session messages, temporal or Git-scoped history, summary sources, or post-compaction context; inspect read-only LCM health.'
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

Recall does not ingest or refresh. A `refresh_required` result needs authorized
lifecycle intent before `tracedecay_session_refresh_begin`. Preserve returned
project/profile scope and opaque handles through
`tracedecay_session_refresh_status` or `tracedecay_session_refresh_cancel`; only
receipt-backed success proves durable cancellation. A profile-root read uses
the compatibility `tracedecay_session_refresh` lifecycle with the same
selectors. Never route a profile refresh through an arbitrary active project or
reconstruct its authority from chat text.

Compression admission and session boundaries are authenticated daemon-owned
host operations, not agent-generated summaries or callable recall operations.
`tracedecay_lcm_status` and `tracedecay_lcm_doctor` are bounded read-only
diagnosis, with no repair or cleanup controls.

Hermes native LCM aliases (`lcm_grep`, `lcm_load_session`, `lcm_describe`,
`lcm_expand`, `lcm_expand_query`) have their own schemas (for example
session_scope and max_content_chars); do not mix alias fields with canonical
command fields or assume those aliases exist on another host.
