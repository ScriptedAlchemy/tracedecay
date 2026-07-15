# TraceDecay V2 roadmap

Status: active product rewrite. PR5 is complete. PR6 is next.

This file owns delivery order. The master and numbered plans define product
requirements and component boundaries; they are not independent queues and do
not require one crate-first pull request per document.

## Product outcome

TraceDecay V2 converges capture, sessions, memory, code intelligence, search,
policy, automation, tools, APIs, integrations, observability, and the dashboard
into one local-first Brain. Before remote delivery, one local daemon is the
physical database authority; PR16 generalizes this to exactly one fenced daemon
authority per mutable shard. Clients, hooks, MCP servers, dashboard handlers,
workers, and remote nodes use typed daemon/application operations; none opens a
fallback writable database.

## Completed foundation

PR4 delivered:

- canonical V2 domain and store boundaries;
- daemon-owned `GlobalDb` connection and transaction authority;
- atomic transcript batch, projection, cursor, and offset updates;
- restart catch-up, replay, and fail-closed project/user-store resolution;
- project-wide session/LCM storage shared across branches and worktrees;
- RAII rollback for database changes and external payload files;
- direct Claude, Cursor, Cline-like, concurrency, recovery, and Windows tests.

PR5 delivered:

- the production Claude parser through mandatory structured sanitization;
- path-independent observation, source, cursor, receipt, and payload contracts;
- atomic observation, receipt, cursor, projection-enqueue, and checkpoint state;
- deterministic projection into the existing searchable V1 session/message view;
- bounded replay, restart, duplicate, collision, partial-input, cancellation,
  stale-authority, migration, consolidation, and crash/retry coverage;
- a clean-commit production benchmark with 30 measured repetitions and a
  verified exact no-op replay that performs no writes or durable work.

The removed planning/evidence machinery is not unfinished product work and must
not be rebuilt.

## Delivery invariants

- Every PR ships executable product behavior through a tested vertical slice.
- Component plans define contracts and ownership. They do not force standalone
  crates, generators, registries, or PRs unless production boundaries justify
  them.
- Each product mechanism has one typed kernel. Surface names and compatibility
  aliases are bindings only; they never acquire their own query, edit, storage,
  rendering, scheduling, or health logic.
- Exactly one fenced daemon remains the sole mutable SQLite authority for each
  shard. Producers send typed commands or observations; readers use
  daemon/application APIs.
- Project facts and project sessions are project-wide. User activity is
  profile-wide. Only code indexes vary by branch/worktree/snapshot.
- Missing authority, scope, privacy state, or recovery proof fails closed.
- Product, contributor, and CI behavior uses stock Cargo semantics.
  Machine-local wrappers may be documented only in explicitly scoped workspace
  guidance; they never become product behavior, repository tests, public setup
  requirements, or hosted-CI dependencies.
- Beginning with PR7, a slice that materially changes crate boundaries,
  dependency fan-in, feature activation, build-script inputs, or test-target
  topology records same-host baseline and candidate developer-feedback evidence.
  Measure a warm incremental or no-op check plus a representative touched test
  target; report wall time, rebuilt units, and available CPU/peak-memory data
  with visible variance. Absolute machine-specific timings are diagnostic, not
  portable acceptance thresholds.
- Developer-build work may change portable repository Cargo manifests,
  configuration, profiles, features, build settings, and build scripts when
  same-workload evidence shows a benefit and stock-Cargo contributor, CI,
  release, and publication behavior remains valid. Rust Analyzer ownership,
  local Cargo wrappers, machine-specific concurrency lanes, absolute target
  locations, and local cache placement remain outside this roadmap.
- Direct behavior, fault, restart, concurrency, cross-platform, and deletion
  tests are delivery evidence. Planning-artifact validation is not.
- Retained obligations are assigned below. None is silently deferred or
  skipped; optional features may remain disabled only until their stated
  product acceptance gate passes.

## Authoritative PR sequence

| PR | Product delivery |
|---|---|
| PR5 (complete) | Sanitized observation vertical: one real provider from parse through sanitizer, daemon-owned persistence, replay, and restart. |
| PR6 | Provider coverage and event normalization: remaining hosts/sources, durable spools, identities, dedupe, partial input, backpressure, and canonical event relations. |
| PR7 | Memory, facts, and provenance: project/profile ownership, evidence, corrections, trust, curation, migration, and deletion lineage. |
| PR8 | Session/LCM temporal retrieval: occurrences, copies, summaries, supersession, current/as-of/evolution retrieval, and stable context assembly. |
| PR9 | Code intelligence and lexical retrieval: deterministic extraction, generations, lineage, generation-bound managed diagnostics/tests, exact/phrase/BM25 search, and V1 parity. |
| PR10 | Native semantic retrieval and ranking: gated FastEmbed artifacts, immutable vector generations, hybrid ranking, redundancy augmentation, evaluation, and lexical fallback. |
| PR11 | Policy, application, catalog, and configuration core: typed use cases, grants, routing, replay, operations, capabilities, analyzer policy/settings, and one runtime configuration authority. |
| PR12 | CLI, MCP, HTTP API, LSP gateway, and output convergence: one schema registry, dispatcher, and binding taxonomy; stable errors/cursors, compact Markdown, canonical JSON, SSE, cancellation, managed diagnostics, and surface parity. |
| PR13 | Hooks, Context Scout, and host bundles: bounded hook ingestion, asynchronous suggestions, Codex/Claude/Cursor/Hermes/Kiro projections, universal TraceDecay LSP registration, install/repair, and stock-host conformance. |
| PR14 | Dashboard, Doctor, observability, and configuration operations: Brain/Explorer/Loom foundations, one truthful health/recovery kernel, metrics/SLOs, Settings, and direct remediation. |
| PR15 | Cross-project, repository, and worktree behavior: canonical scope resolution, federation, globally routable evidence, graph/query/LSP workspace coverage, and multi-repository workflows. |
| PR16 | Remote shared Brain: enrolled nodes, one fenced authority per shard, offline sanitized capture, verified caches/replicas, node-local LSP overlays/analyzers, Git correlation, backup, restore, and failover. |
| PR17 | Real typed dynamic workflows and automations: daemon-owned definitions, deterministic replay, and one shared scheduler/history/lease/effect/artifact kernel. |
| PR18 | Official API stabilization and SDKs: frozen public contract, OpenAPI/schema publication, first-party Rust/TypeScript/Python SDKs, docs, and conformance. |
| PR19 | Compatibility migration, defragmentation, cutover, and deletion: resumable backfill, shadow parity, bounded cutovers, rollback window, V2 default, and removal of every superseded V1 path. |
| PR20 | End-to-end performance optimization: measured database, synchronization, projection, indexing, cache/generation, query, and repository-controlled developer-build improvements with Linux/Windows and crash/restart regression gates. |

PR #421 stays open through PR20. It merges only after PR20 and the aggregate
Linux, Windows, migration, recovery, privacy, performance, and deletion gates
are stable.

## Component-plan ownership

- Plans 01–04 and 18: PR5–PR7 capture, storage, privacy, identity, projection,
  recovery, and migration boundaries.
- Plans 05, 15, 23, 25, and 31: PR8–PR10 temporal, lexical, code, semantic,
  ranking, and evaluation behavior.
- Plans 06, 08–10, 17, 20, 21, and
  [34](34-workspace-refactoring-and-api-migration.md): PR11–PR12 application,
  policy, configuration, catalog, transport, presentation, public contracts, and
  safe workspace refactoring.
- Plans 07 and 27: PR6 host/hook baseline and canonical integration model, then
  PR13 daemon cutover, host bundles, lifecycle, and conformance. Plan 22 owns
  PR13 Context Scout behavior.
- Plan [35](35-daemon-lsp-gateway-and-universal-diagnostics.md): PR9,
  PR11–PR13, PR15, and PR16 generation-bound diagnostics, analyzer policy,
  daemon LSP gateway, universal host plugin, multi-root scope, and remote-node
  behavior.
- Plans 11, 14, and 26: PR14 product UI, Doctor, observability, regression, and
  operational quality.
- Plan 16: PR15 canonical scope. Plan 24 is a permanent tombstone for the
  removed task-plan parser, tracker, and multi-agent executor product.
- Plan 28: PR16 remote topology and authority.
- Plan 32: PR17 typed dynamic-workflow product.
- Plans 08, 12, 13, 17, 19, and every component migration section: PR18–PR19
  SDK binding, publication, provenance, compatibility, cutover, and deletion.
- Plan 33: PR20 end-to-end database, synchronization, indexing, query, and
  repository-controlled developer-build performance optimization. Owning slices
  provide instrumentation and baselines.
- The retired Plans 29–30 review artifacts are deleted. Any still-valid behavior belongs in
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
permits it. From PR7 onward, include the developer-feedback measurements above
when the slice materially changes Rust compilation scope. Passing code and tests
in Git are the completion record.
