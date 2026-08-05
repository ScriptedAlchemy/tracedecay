# LCM compression behavior

TraceDecay's lossless-context-memory (LCM) session authority ingests active
conversation messages before evaluating compression. Raw messages, summary
nodes, payload identity, lifecycle state, and replay ordering remain in the
same daemon-owned authority; a host or operator does not write a summary or
database row directly.

## When compression runs

The context engine evaluates the effective assembly budget after active-message
ingest. It preserves the fresh message tail and historical system/developer
anchors, then considers the older unsummarized backlog.

- Ignored or stateless sessions do not enter durable compression.
- A recently recorded compression boundary applies a short cooldown so a host
  rotation cannot trigger duplicate compression; lossless ingest still occurs.
- Backlog reaches compression eligibility only when its content meets the
  active budget and chunk rules. Forced overflow and maintenance debt can
  require earlier work.
- If Hermes needs an auxiliary summary, the daemon returns a typed summary
  request. It does not invent or persist a summary until the context engine
  provides one.
- A supplied summary is admitted with its source range, route metadata, and
  lifecycle frontier in one transaction. If it is not sufficiently smaller
  than its source, the response records the fallback/rescue outcome instead of
  silently claiming a useful compaction.

## Replay and recovery

Replay is ordered by source/store position, with summary blocks preceding raw
messages at the same position. Historical system and developer anchors before
the fresh tail are not summarized. The assembly retains the newest viable
contiguous raw tail under the budget and can preserve the latest user objective
as a system scaffold when a tool-heavy tail would otherwise lose it.

Summaries are a drill-down index, not a replacement for the source archive.
Use the native Hermes aliases or matching TraceDecay LCM operations to search,
load, describe, and expand a session. Their coverage, source ranges, opaque
continuations, anchors, and redaction state bound what a replay proves.

## Hermes operations and diagnostics

On Hermes, the current context engine exposes `lcm_grep`, `lcm_load_session`,
`lcm_describe`, `lcm_expand`, `lcm_expand_query`, `lcm_status`, and
`lcm_doctor`. Other hosts use the matching `tracedecay_lcm_*` operations. Use
the schema of the surface you called; do not mix native alias fields with MCP
fields.

Start session diagnosis with `lcm_status` and `lcm_doctor`. They are read-only
and report session identity, retention, payload/provenance, coverage, and
compression state. `needs_summary`, `warming`, `partial`, `unavailable`,
`denied`, and `refresh_required` are explicit outcomes. They do not authorize a
host to re-ingest, repair, delete, or rebuild session storage. Any retention or
repair effect is a separate authorized daemon operation with its own preview
and receipt.
