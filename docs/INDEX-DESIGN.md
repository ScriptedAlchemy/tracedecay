# Code-intelligence index design

TraceDecay turns an exact repository/worktree snapshot into immutable,
queryable code generations.

## Pipeline

1. The daemon resolves the registered project and captures an exact repository,
   worktree, ref, commit/index/worktree snapshot and configuration/privacy
   identity.
2. Language extractors parse sanitized source and emit typed files, symbols,
   chunks, occurrences, unresolved references, diagnostics, and relation
   evidence.
3. Resolution binds only evidenced targets and preserves ambiguity,
   abstention, authority, and coverage.
4. Publication validates the complete generation, writes its graph/vector
   projection to the daemon-owned embedded Grafeo store, persists relational
   manifests/receipts/content locators in SQLite, then advances the canonical
   watermark.
5. Query/application services freeze the selected generation and return exact,
   lexical, graph, semantic, diagnostic, and Git-enriched evidence with stable
   ordering, cursors, provenance, and coverage.

## Authorities

- `tracedecay-code-extraction` owns parsing and typed extracted evidence.
- `tracedecay-code-index` owns generation planning and code graph/vector
  publication.
- `tracedecay-graph-db` is the sole embedded Grafeo dependency and durable
  graph/vector storage boundary.
- SQLite owns relational manifests, source/content locators, publication
  journals, receipts, configuration, and fencing.
- The application layer owns authorization, generation selection, retrieval
  composition, hydration, and output semantics.
- MCP, CLI, HTTP/dashboard, LSP, SDK, hooks, hosts, and workers are clients of
  the daemon/application route and never open index stores.

No SQLite node/edge/vector schema, branch database, direct query-to-Grafeo
dependency, sidecar, or in-memory production graph is authoritative.

## Identity and incrementality

Entity, occurrence, relation, chunk, model, and generation identities are typed
TraceDecay values. Grafeo handles are storage-local. Content-addressed
parse/chunk artifacts may be reused only when source bytes, grammar/extractor,
privacy, configuration, and model identities match. Worktree/ref/snapshot
provenance is never reused.

Change signals are bounded hints. The daemon reconciles against repository
truth, coalesces watcher bursts, and classifies added/changed/deleted/renamed
content. A no-op publishes nothing. A failed or cancelled build leaves the
prior complete generation readable.

## Retrieval

Exact identifier/path/quoted/error matches form a non-demotable tier. Lexical,
graph, semantic, Git, diagnostics, and affected-test lanes report their own
coverage and contributions. Ranking occurs before bounded hydration; cursors
pin query, authorization, generation, ranking/profile, and continuation state.
Missing graph/vector/model authority is typed unavailable or partial, never a
successful empty result or heuristic substitute.

## Freshness and retention

Reads never trigger sync. They return the exact generation/watermark and a
typed stale/refresh-required state. Explicit daemon refresh and host change
signals schedule capture.

Retention uses reachability from active generations, receipts, evidence,
facts, sessions, rollback floors, and authorized holds. Worktree/ref deletion
may collect only unreachable derived generations. Project-wide facts and source
evidence do not belong to a branch.

Final V2 stores are created at the final shape; incompatible old TraceDecay
indexes return `ResetRequired`. Historical repository/host data may be newly
captured, never imported from a legacy TraceDecay database.
