---
name: managing-session-context
description: 'Recover raw prior-session messages, temporal or Git-scoped history, summary sources, or post-compaction context; inspect read-only LCM health.'
---

# Managing session context

Use message search to locate an ingested conversation, scoped temporal grep to
narrow it, and lossless session replay for exact messages. Durable decisions and
facts belong to `project-memory`. Cross-project retrieval must select the
registered target store rather than implicitly searching the active project.

Summary-DAG description locates a node without opening its body; expansion opens
its bounded sources. Continue using the returned opaque cursor unchanged with
the same target and slice bounds. Never manufacture a cursor from row numbers.
When expansion says `needs_synthesis`, synthesize from its bounded context rather
than presenting a direct answer as authoritative.

Preserve coverage, anchors, watermarks, redaction, and hidden-content notices.
Partial coverage does not prove content never existed. Git-scoped session
relations distinguish produced from observed commits; workflow recovery reads
`wf_*` session runs, not the Workflow definition/run mutation surface.

Recall does not ingest or refresh. A `refresh_required` result needs authorized
lifecycle intent before refresh begin. Preserve returned project/profile scope
and opaque handles through status or cancellation; only receipt-backed success
proves durable cancellation. Never route a profile refresh through an arbitrary
active project or reconstruct its authority from chat text.

Compression admission and session boundaries are authenticated daemon-owned
host operations, not agent-generated summaries or callable recall operations.
LCM doctor is bounded read-only diagnosis, with no repair or cleanup controls.

Hermes native LCM aliases have their own schemas (for example session_scope and
max_content_chars); do not mix alias fields with canonical command fields or
assume those aliases exist on another host.
