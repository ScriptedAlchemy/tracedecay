# TraceDecay V2 roadmap

Status: active product rewrite. PR4 is complete. PR5 is next.

This file owns delivery order. The master and numbered plans define product
requirements and component boundaries; they are not independent queues and do
not require one crate-first pull request per document.

## Product outcome

TraceDecay V2 converges capture, sessions, memory, code intelligence, search,
policy, automation, tools, APIs, integrations, observability, and the dashboard
into one local-first Brain. One daemon is the physical database authority.
Clients, hooks, MCP servers, dashboard handlers, workers, and remote nodes use
typed daemon/application operations; none opens a fallback writable database.

## Completed foundation

PR4 delivered:

- canonical V2 domain and store boundaries;
- daemon-owned `GlobalDb` connection and transaction authority;
- atomic transcript batch, projection, cursor, and offset updates;
- restart catch-up, replay, and fail-closed project/user-store resolution;
- project-wide session/LCM storage shared across branches and worktrees;
- RAII rollback for database changes and external payload files;
- direct Claude, Cursor, Cline-like, concurrency, recovery, and Windows tests.

PR1–PR3 planning/evidence machinery is superseded. It is not unfinished product
work and must not be rebuilt.

## Delivery invariants

- Every PR ships executable product behavior through a tested vertical slice.
- Component plans define contracts and ownership. They do not force standalone
  crates, generators, registries, or PRs unless production boundaries justify
  them.
- Each product mechanism has one typed kernel. Surface names and compatibility
  aliases are bindings only; they never acquire their own query, edit, storage,
  rendering, scheduling, or health logic.
- The daemon remains the sole mutable SQLite authority. Producers send typed
  commands or observations; readers use daemon/application APIs.
- Project facts and project sessions are project-wide. User activity is
  profile-wide. Only code indexes vary by branch/worktree/snapshot.
- Missing authority, scope, privacy state, or recovery proof fails closed.
- Use stock Cargo commands. Local cache wrappers are developer conveniences and
  never enter repository source, tests, documentation requirements, or CI.
- Direct behavior, fault, restart, concurrency, cross-platform, and deletion
  tests are delivery evidence. Planning-artifact validation is not.
- Retained obligations are assigned below. None is silently deferred or
  skipped; optional features may remain disabled only until their stated
  product acceptance gate passes.

## Authoritative PR sequence

| PR | Product delivery |
|---|---|
| PR5 | Sanitized observation vertical: one real provider from parse through sanitizer, daemon-owned persistence, replay, and restart. |
| PR6 | Provider coverage and event normalization: remaining hosts/sources, durable spools, identities, dedupe, partial input, backpressure, and canonical event relations. |
| PR7 | Memory, facts, and provenance: project/profile ownership, evidence, corrections, trust, curation, migration, and deletion lineage. |
| PR8 | Session/LCM temporal retrieval: occurrences, copies, summaries, supersession, current/as-of/evolution retrieval, and stable context assembly. |
| PR9 | Code intelligence and lexical retrieval: deterministic extraction, generations, lineage, diagnostics/tests, exact/phrase/BM25 search, and V1 parity. |
| PR10 | Native semantic retrieval and ranking: gated FastEmbed artifacts, immutable vector generations, hybrid ranking, redundancy augmentation, evaluation, and lexical fallback. |
| PR11 | Policy, application, catalog, and configuration core: typed use cases, grants, routing, replay, operations, capabilities, settings, and one runtime configuration authority. |
| PR12 | CLI, MCP, HTTP API, and output convergence: one schema registry, dispatcher, and binding taxonomy; stable errors/cursors, compact Markdown, canonical JSON, SSE, cancellation, and surface parity. |
| PR13 | Hooks, Context Scout, and host bundles: bounded hook ingestion, asynchronous suggestions, Codex/Claude/Cursor/Hermes projections, install/repair, and stock-host conformance. |
| PR14 | Dashboard, Doctor, observability, and configuration operations: Brain/Explorer/Loom foundations, one truthful health/recovery kernel, metrics/SLOs, Settings, and direct remediation. |
| PR15 | Cross-project, repository, and worktree behavior: canonical scope resolution, federation, globally routable evidence, graph/query coverage, and multi-repository workflows. |
| PR16 | Remote shared Brain: enrolled nodes, one fenced authority per shard, offline sanitized capture, verified caches/replicas, Git correlation, backup, restore, and failover. |
| PR17 | Real typed dynamic workflows and automations: daemon-owned definitions, deterministic replay, and one shared scheduler/history/lease/effect/artifact kernel. |
| PR18 | Official API stabilization and SDKs: frozen public contract, OpenAPI/schema publication, first-party Rust/TypeScript/Python SDKs, docs, and conformance. |
| PR19 | Compatibility migration, defragmentation, cutover, and deletion: resumable backfill, shadow parity, bounded cutovers, rollback window, V2 default, and removal of every superseded V1 path. |
| PR20 | End-to-end performance optimization: measured database, synchronization, projection, indexing, cache/generation, and query improvements with Linux/Windows and crash/restart regression gates. |

PR #421 stays open through PR20. It merges only after PR20 and the aggregate
Linux, Windows, migration, recovery, privacy, performance, and deletion gates
are stable.

## Component-plan ownership

- Plans 01–04 and 18: PR5–PR7 capture, storage, privacy, identity, projection,
  recovery, and migration boundaries.
- Plans 05, 15, 23, 25, and 31: PR8–PR10 temporal, lexical, code, semantic,
  ranking, and evaluation behavior.
- Plans 06, 08–10, 17, 20, and 21: PR11–PR12 application, policy,
  configuration, catalog, transport, presentation, and public contracts.
- Plans 07, 22, and 27: PR13 hook, Scout, host integration, bundle, and
  conformance behavior.
- Plans 11, 14, 19, and 26: PR14 product UI, Doctor, observability, regression,
  convergence, and operational quality.
- Plan 16: PR15 canonical scope. Plan 24 is a permanent tombstone for the
  removed task-plan parser, tracker, and multi-agent executor product.
- Plan 28: PR16 remote topology and authority.
- Plan 32: PR17 typed dynamic-workflow product.
- Plans 12, 13, 17, 19, and every component migration section: PR18–PR19
  publication, provenance, compatibility, cutover, and deletion.
- Plan 33: PR20 end-to-end database, synchronization, indexing, and query
  performance optimization. Owning slices provide instrumentation and baselines.
- Plans 29–30 are deleted review artifacts. Any still-valid behavior belongs in
  the owning product plan and its direct regression tests.

## Rejected rewrite machinery

Do not restore:

- plan Markdown parsers, PR-ID grammars, slice DAGs, completion ledgers,
  progress trackers, next-ready controllers, or rewrite executors;
- compatibility or architecture inventories used to model implementation;
- generated plan views, owner maps, baseline packets, receipts, or CI gates;
- Claude workflow JavaScript or any host-specific workflow that executes this
  roadmap;
- a second metadata model that generates product declarations from YAML,
  JSON, Markdown, or checked-in snapshots.

PR17 workflows are product data executed through typed daemon operations. They
cannot parse this roadmap, dispatch its PRs, track rewrite completion, or act as
a developer-plan executor.

## Delivery gate

For each PR: implement the smallest coherent vertical slice, run focused direct
tests, independently review the integrated diff, run the relevant broader stock
Cargo and cross-platform gates, and delete replaced paths when the rollback gate
permits it. Passing code and tests in Git are the completion record.
