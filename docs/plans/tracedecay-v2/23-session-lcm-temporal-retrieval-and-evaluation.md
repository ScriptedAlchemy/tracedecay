# TraceDecay V2 Session and LCM Temporal Retrieval

**Delivery:** PR 8

**Status:** planned product work
**Depends on:** [01 domain](01-domain-crate.md), [02 store](02-store-crate.md), [03 capture](03-capture-crate.md), [04 projectors](04-projectors-crate.md), [05 query](05-query-crate.md), [09 application](09-application-crate.md), [13 anchors](13-research-provenance-and-context-anchors.md), and [18 privacy](18-secret-detection-redaction-and-private-data-safety.md). PR8 ships against explicitly resolved current-project/single-root scope and address contracts available by then; [16 scope](16-cross-project-repository-worktree-scope.md) later composes this retrieval with canonical cross-project/repository/worktree resolution in PR15 and is not a PR8 implementation prerequisite.

## Outcome

PR 8 replaces fragmented message search and LCM lookup with one temporally correct retrieval path for messages, Turns, sessions, threads, agents, and summaries. It returns the smallest useful context while preserving exact text, history, provenance, privacy, and stable anchors.

This is product retrieval work. It does not implement task filtering, plan execution, a benchmark bureaucracy, or a Search Quality Lab.

## Evidence authority boundary

LCM external payloads and the summary DAG are canonical only for session-linked
narrative and tool-output context: messages, Turns, sessions, threads, agents,
and derived summaries over that evidence.

They may reference [Plan 13](13-research-provenance-and-context-anchors.md)
`RetrievalAnchorId` values and provide bounded drill-down to exact retained
evidence, but they never become canonical authority or durable storage for:

- GitHub review threads, comments, or replies;
- CI runs, logs, or artifact excerpts;
- diagnostics or provider findings;
- Git snapshots, `HunkRef`, or mutation receipts; or
- workflow/effect receipts.

A summary cannot replace or hide exact evidence. When a query needs GitHub, CI,
diagnostic, or Git evidence, resolution goes through Plan 13 anchors and the
owning store for that evidence class.

Transport `rh_` response handles from
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) are 24-hour,
project-local output recovery for truncated MCP/CLI responses. They are not
durable evidence identity and must not be stored as canonical LCM or summary
sources. [Plan 05](05-query-crate.md) opaque cursors page typed collections only.

## Source truth

- Every provider observation is an immutable message occurrence with source identity, order, ingest time, valid time when known, scope, and sanitization receipt.
- Logical copies are versioned evidence-backed relations. Content hashes, timestamps, titles, or embeddings alone never collapse messages.
- Corrections and supersession append typed assertions. They do not overwrite prior occurrences.
- Turns and threads are first-class retrieval grains rather than labels inferred at render time.
- Raw occurrences remain addressable subject to authorization, retention, redaction, and deletion.

## Temporal modes

The shared query accepts four explicit modes:

- `current`: prefer supported current assertions and show material conflicts;
- `as_of`: evaluate only evidence known and valid at the requested cutoff;
- `evolution`: show the ordered correction and supersession chain;
- `forensic`: preserve all authorized occurrences and uncertainty.

Recency is bounded evidence, not a truth rule. A newer weak mention does not erase an older authoritative decision, and an old exact match does not silently override a supported correction.

## Retrieval pipeline

This is the sole temporal retrieval kernel. Legacy `message_search` and
`lcm_grep`/load/describe/expand/query bindings translate into this request and
delegate; they do not keep separate ranking, hydration, context, pagination, or
freshness logic. Workflow recovery consumes session evidence through this
kernel; the term workflow otherwise belongs to the PR17 product.

1. Resolve the exact authorized profile/project/repository/worktree/ref/provider/session scope.
2. Pin store, projection, graph, index, and configuration watermarks.
3. Generate bounded lexical, phrase, fuzzy, entity, summary, graph, time, and configured semantic candidates.
4. Fuse candidates without comparing uncalibrated shard-local scores directly.
5. resolve copies, temporal assertions, authority, contradiction, and requested answer mode;
6. diversify by logical message, Turn, session, source, and evidence role;
7. hydrate only the selected anchors under current authorization;
8. assemble a compact context bundle under an exact byte/token budget.

Exact identifiers, errors, paths, symbols, commands, and quoted phrases remain first-class. Optional semantic or model-assisted stages cannot bypass lexical exactness, privacy, temporal safety, or abstention.

## Summary DAG

LCM summaries are immutable derived nodes with exact source anchors, source horizon, model/configuration route, creation watermark, and sanitization receipt. A summary cannot replace or hide its source.

Publishing a summary atomically commits the node, source edges, content, and anchor manifest. Missing, stale, deleted, redacted, unauthorized, cyclic, or unverifiable sources make the node unavailable for current answers. Corrections publish successor lineage and stale affected descendants; they never rewrite history.

Context assembly may use a summary only when its horizon covers the selected evidence. Exact source text remains retrievable when required by the query or budget permits it. Summary drill-down may follow Plan 13 anchors to GitHub, CI, diagnostic, or Git evidence, but the summary node itself never becomes the durable store for those classes.

## Plan 37 reuse without a parallel kernel

[Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) may reuse
this plan's session context expansion, temporal modes, ranking fusion, and compact
context assembly for branch-aware feedback capsules and advisory proximity context.
That reuse delegates to this sole temporal retrieval kernel through typed
application requests. Plan 37 must not add a parallel LCM engine, summary store, or
second hydration path for GitHub, CI, diagnostic, or Git evidence. Those products
resolve through Plan 13 anchors and their owning stores; Plan 37 binds cycle
results and capsules to those references instead of copying durable evidence into
session payloads.

PR13 read-only GitHub and CI ingress does not require
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md). Plan 32 at PR17 may
optionally compose already-shipped read-only operations in workflows through
[Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md); it
never enables a GitHub write path. LCM and summary payloads remain
session-narrative authority only with no write-side GitHub path.

## Side-effect-free reads and freshness

Search, LCM expansion, hydration, and replay never ingest provider history, repair a store, open a writable fallback, or advance a cursor. They report the watermark and truthful fresh/stale/partial/unavailable coverage they observed.

Freshness is an explicit daemon operation. Equivalent refresh requests join one durable operation keyed by source frontier and target watermark. The daemon scans each source once, commits sanitized observations and source progress atomically, resumes after restart, and returns a typed coverage receipt. Cancellation returns the last committed frontier.

## Result and context contract

Each result includes a stable retrieval anchor, logical occurrence/cluster identity, Turn/session/thread identity, timestamp and temporal state, safe snippet, evidence/authority class, score explanation, source coverage, and hydration availability.

Pages use stable ordering and a cursor bound to query, scope, temporal mode, and watermarks. Empty results distinguish no relevant evidence from stale, partial, wrong-scope, retained, locked, redacted, or unavailable sources.

Compact context contains only the selected Turns, exact supporting evidence, summary lineage when used, conflicts, omissions, and continuation anchors. It never dumps a transcript or unrelated agent activity.

## Direct verification

- copied prompts collapse only with origin evidence; independent repetition remains distinct;
- current, as-of, evolution, and forensic fixtures handle correction and conflict correctly;
- summary lineage survives restart and exposes exact sources;
- exact technical queries are not displaced by generic semantic neighbors;
- within the PR8 single-root scope, results hydrate without CWD or store switching; PR15 extends this to canonical cross-project targets;
- partial/conflicted shards remain visible and never fabricate an empty-complete answer;
- reads create no files, rows, cursors, repairs, or writable connections;
- concurrent refresh callers share one operation and one terminal receipt;
- stable pagination rejects changed-watermark cursors;
- deletion, redaction, retention, authorization, and prompt-injection fixtures fail closed;
- compact output stays within its budget and preserves anchors and coverage;
- GitHub, CI, diagnostic, and Git evidence referenced from session context resolve only
  through Plan 13 anchors, never through LCM payloads, summary nodes, or `rh_` handles;
- Plan 37 session-context reuse exercises this kernel without a second retrieval engine.

## PR 8 deliverables

- occurrence, logical-copy, Turn/thread, temporal-assertion, and summary-lineage contracts;
- rebuildable projections and indexes;
- unified temporal retrieval and context assembly;
- stable anchors, pagination, coverage, explanations, and abstention;
- daemon-owned durable refresh operation;
- migration from existing message/LCM sources with idempotent receipts;
- focused correctness, concurrency, privacy, restart, and resource tests.

## Done

- One retrieval engine serves message, Turn, session, thread, agent, and LCM context.
- Raw evidence and summary lineage remain recoverable and temporally correct.
- Reads are side-effect free; refresh is explicit and daemon-owned.
- Every result and context bundle is compact, anchored, scoped, and coverage-aware.
- LCM payloads and summaries remain session-narrative authority only; GitHub, CI,
  diagnostic, Git, and receipt evidence stay on Plan 13 anchors and owning stores.
- Plan 37 reuses this kernel for session expansion without a parallel retrieval path.
- No task-plan filtering, executor dependency, evaluation bureaucracy, or parallel LCM engine remains.
