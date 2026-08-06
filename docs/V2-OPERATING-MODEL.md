# TraceDecay Final V2 Operating Model

This is a concise operator and contributor summary of final-V2 storage, scope,
host ingestion, and retrieval. The
[V2 roadmap](plans/tracedecay-v2/00-plan-set-index.md) is the sole authority for
precedence, rejected mechanisms, delivery order, and acceptance; its numbered
plans own detailed behavior, and
[`NEXT.md`](plans/tracedecay-v2/NEXT.md) reports current delivery status.
Runtime status remains the truth for capabilities not yet delivered.

## Authorities

- `tracedecay-graph-db` is the only TraceDecay boundary for the workspace-pinned
  embedded Grafeo `=0.5.42` runtime. It exposes typed TraceDecay identities,
  operations, snapshots, generations, and outcomes; Grafeo handles and types
  do not escape it.
- Grafeo is in-process and embedded. It has no graph server, sidecar, network
  transport, or separately managed process. `petgraph` and other graph/vector
  stores are not product authorities.
- Grafeo is the sole persisted/query graph and vector projection: code symbol,
  file, and chunk nodes; relation and traversal indexes; admitted vectors;
  Git/evidence, session, memory, work, and workflow relation topology.
  Canonical events, facts, source content, and reconstruction manifests remain
  in their domain stores and are sufficient to rebuild every projection.
- SQLite is relational only: registry/configuration, source cursors, admission,
  idempotency, inbox/outbox, journals, leases, receipts, redaction, retention,
  raw content and exact evidence spans, manifests, and accounting. It neither
  shadows graph/vector data nor gains a graph-shaped compatibility table.
- Every datum has one canonical authority. A rebuildable projection records its
  source generation and watermark; it never becomes a second canonical copy.
  A canonical event plus complete replay input is durable before graph
  convergence starts. The graph watermark advances only after close/reopen and
  a full digest over recovered entities, relations, source generation, and
  watermark matches that input, so replay cannot manufacture a second event.

## Exact final stores

Every database, store, spool, file, journal, checkpoint, receipt, and
projection admits only its exact final V2 shape. Any other shape returns typed
`ResetRequired` before decoding, reading, writing, replaying, or projecting.
The operator may explicitly reset or recreate that exact target; preserved
bytes are inspection-only.

There is no V1 or earlier-V2 database reader, migration, conversion,
database-format backfill, census, fallback, shadow read, dual write, staging
state, cutover receipt, recovery route, or transition dashboard. Historical
host transcripts, repositories, and other source material remain ordinary V2
ingress. A released public wire/API contract may delegate to the canonical
operation only when independent release evidence proves it; it owns no storage
or lifecycle behavior.

One daemon authority owns a local mutable store; clients reach it through the
application boundary. Validation failures leave the prior verified graph
readable. Grafeo `0.5.42` does not surface every mutation WAL append failure,
so `WalSync` is a flush request, not a publication receipt. Observable
sync/close/recovery failures return typed `DurabilityUncertain` and close the
runtime. Successful convergence remains unservable until close/reopen and full
recovered-projection digest verification; mismatch resets and rebuilds the
derived projection from canonical input.
Cancellation, staleness, denial, unavailability, reset-required, corruption,
and budget exhaustion stay typed through every surface.

## Identity, scope, and host sources

`ProjectId`, `RepositoryId`, `WorktreeId`, and all domain identities are opaque
and non-interchangeable. A linked worktree or branch selects an exact code and
Git-revision snapshot only. Facts, sessions, messages, LCM data, and their
ownership remain project-wide: branches and worktrees never own, copy, merge,
or retire facts.

Cross-project or multi-root access requires an explicit, authorized frozen
scope with exact project/repository/worktree/generation provenance. A default,
path guess, host home, branch name, similar content, or collection label cannot
widen authority. Every terminal result reports truthful authorized coverage;
hidden or denied roots never become empty-complete results.

Historical agent-host sources remain valid ingress, not store conversion. Each
ingest is bounded by its declared host/source locator, exact project or user
profile identity, authorization, and durable cursor. It preserves source
provenance and advances only its admitted cursor; it neither enumerates
unbounded host history nor creates a host-, branch-, or worktree-local fact
store.

## Lossless bounded retrieval

One retrieval kernel serves MCP, CLI, dashboard, host integrations, LSP, API,
and SDK surfaces. Adapters translate and render; they do not add a second
ranking, cursor, hydration, or store-selection path.

1. Resolve authorization and freeze the request, scope, versions, and sorted
   source manifest before candidate generation.
2. Produce compact canonical candidates, resolve temporal authority and
   contradictions, then fuse, deduplicate, diversify, and deterministically
   rank them. Exact literals retain exact occurrence byte ranges; only explicit
   logical-copy evidence collapses independent occurrences.
3. Freeze ordered page membership and its continuation boundary before payload
   bytes are read. The canonical cursor binds scope, query/filter semantics,
   ranking/diversity configuration, and participating source watermarks; drift
   rejects continuation rather than changing membership.
4. Hydrate the frozen page late from each anchor's exact authorized source.
   Hydration may make an item typed-unavailable, but may not add, remove,
   reorder, promote, demote, replace, or backfill it from candidate metadata.
   An unavailable selected item retains its rank and omission reason.
5. Return source coverage, stable anchors, exact support and span provenance,
   summary lineage, typed omissions, and a continuation anchor. Budgeted
   delivery reports truncation rather than silently dropping evidence.

Summaries are immutable derived nodes with exact source anchors, source
horizon, and sanitization/configuration provenance. They never replace or hide
their sources. Expansion from a summary, span, burst, handle, or compact result
is lossless to every authorized occurrence and preserves unavailable members by
ordinal; retention, redaction, or authorization is reported as the typed reason
when exact payload cannot be supplied. No compact result may dump unrelated
transcript activity or silently imply complete coverage.

## Validation

All runtime validation uses isolated temporary home, profile, project, and
socket paths. Never install, dogfood, start, restart, or test a V2 daemon
against an operator's live TraceDecay profile. Acceptance is direct production
behavior: exact final-store admission, authoritative reset/recreation, bounded
host ingestion, project-wide fact scope, deterministic lossless retrieval, and
truthful truncation/omission outcomes.
