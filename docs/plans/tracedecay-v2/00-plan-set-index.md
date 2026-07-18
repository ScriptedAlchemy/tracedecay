# TraceDecay V2 roadmap

Status: active product rewrite. PR7 is complete and PR8 implementation is
active.

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

PR6 delivered:

- one complete host-neutral catalog and provider observation path for the
  supported Claude, Codex, Cursor, Hermes, Kiro, and Cline-family sources;
- bounded checksummed daemon host admission for non-replayable events, fair
  bounded scheduling for replayable sources, and typed failure/backpressure;
- atomic projection with staged bounded rebuild, provider-native identity and
  relation preservation, typed hook telemetry, and executable native host
  fixtures;
- an executable multi-provider benchmark harness and clean attested acceptance
  evidence recorded by commit `05da230e`.

PR7 delivered the canonical project/profile memory and fact path, evidence and
provenance, corrections and trust, curation, migration, deletion lineage,
dogfood hardening, and accepted aggregate evidence. PR8 now owns the active
Session/LCM temporal-retrieval slice.

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
- Git intelligence is evidence-first and user-directed. TraceDecay never
  autonomously mutates branches, worktrees, refs, or published history.
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
| PR6 (complete) | Provider coverage and event normalization: remaining hosts/sources, daemon host-admission spool for non-replayable events, identities, dedupe, partial input, backpressure, and canonical event relations. |
| PR7 (complete) | Memory, facts, and provenance: project/profile ownership, evidence, corrections, trust, curation, migration, deletion lineage, and generation-bound repository provenance anchors. |
| PR8 (active) | Session/LCM temporal retrieval: occurrences, copies, summaries, supersession, current/as-of/evolution retrieval, and stable context assembly. |
| PR9 | Code intelligence and lexical retrieval: deterministic extraction, generations, lineage, generation-bound managed diagnostics/tests, exact/phrase/BM25 search, V1 parity, and typed read-only Git status/diff/history/blame/hunk intelligence enriched by graph impact. |
| PR10 | Native semantic retrieval and ranking: gated FastEmbed artifacts, immutable vector generations, hybrid ranking, redundancy augmentation, evaluation, and lexical fallback. |
| PR11 | Policy, application, catalog, and configuration core: typed use cases, grants, routing, replay, operations, capabilities, analyzer policy/settings, one runtime configuration authority, daemon-serialized `stage_hunks`/`unstage_hunks`/`commit_index` transactions with `HunkRef` compare-and-swap and receipts, and the typed branch-aware feedback-cycle request/result and orchestration ([Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)) — first pillar of the PR11–PR13 read-only/advisory milestone (post-edit diagnostics and impact). |
| PR12 | CLI, MCP, HTTP API, LSP gateway, and output convergence: one schema registry, dispatcher, and binding taxonomy; stable errors/cursors, compact Markdown, canonical JSON, SSE, cancellation, managed diagnostics, surface parity, shared Git preview/apply bindings, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s PR12 slice — [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway triggers plus the explicit diagnostics-call trigger/surface bound once through [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) — completing the post-edit diagnostics-and-impact pillar for LSP/MCP/CLI surfaces. |
| PR13 | Hooks, Context Scout, and host bundles: bounded hook ingestion, asynchronous suggestions, Codex/Claude/Cursor/Hermes/Kiro projections, one TraceDecay semantic/diagnostic contract delivered per host (Claude Code LSP plugin; Cursor desktop native-diagnostics adapter with duplicate-analyzer avoidance; hook/MCP/CLI capability paths for Cursor cloud, Codex, and other supported hosts as applicable), install/repair, stock-host conformance, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s PR13 slice — first availability of GitHub review-comment ingestion/surfacing, CI-failure localization, and tiered concurrent-agent proximity adapters through hooks/MCP/CLI, completing the PR11–PR13 read-only/advisory milestone with all four pillars (post-edit diagnostics+impact, CI localization, GitHub thread ingestion/surfacing, proximity). TraceDecay never posts, updates, resolves, replies to, or dismisses GitHub comments. |
| PR14 | Dashboard, Doctor, observability, and configuration operations: Brain/Explorer/Loom foundations, one truthful health/recovery kernel, metrics/SLOs, Settings, direct remediation, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) dashboard/Doctor consumption of the typed feedback-cycle, GitHub-ingested review-thread, CI-localization, and proximity state shipped at PR13. |
| PR15 | Cross-project, repository, and worktree behavior: canonical scope resolution, federation, globally routable evidence, graph/query/LSP workspace coverage, and multi-repository workflows. |
| PR16 | Remote shared Brain: enrolled nodes, one fenced authority per shard, remote offline-capture spool, verified caches/replicas, node-local LSP overlays/analyzers, Git correlation, backup, restore, and failover. |
| PR17 | Canonical product task/work graph, adaptive task intelligence, and typed workflows ([Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md), [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)): versioned user task/ticket DAG state, evidence/history relations, Kanban/DAG/timeline/causal/workload projections, task-shape and calibrated-size assessment, reviewed parent/child decomposition, model-capability profiles and explained routing, independent grades/outcomes/calibration, live split/merge/resize/re-route proposals that never auto-apply, and graph-native typed auxiliary-attempt requests; plus daemon-owned definitions, deterministic replay, and one shared runtime-clock/scheduler/history/lease/attempt/effect/artifact kernel that executes explicitly admitted task steps through negotiated provider adapters. Claude-designated work uses native Claude Code CLI, never Hermes Anthropic. Codex app-server is preferred; a distinct Codex CLI fallback is explicit, policy/configuration-bounded, and never hidden. Typed workflow steps may compose [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s already-shipped read-only advisory operations (feedback-cycle findings, GitHub-ingested review-thread surfacing, CI localization, proximity) — not first availability of those capabilities and no GitHub writes. Plan 24 owns work-graph/proposal/auxiliary-request plus task-domain ready-node/decomposition/sizing/model-backend recommendation semantics; Plan 06 owns pure evaluator/policy-decision mechanics; Plan 26 owns observations/metrics; Plan 32 alone owns runtime clocks, provider-adapter execution, scheduling, leases, attempts, effects, retries, cancellation, and runtime receipts. [Plan 36](36-git-aware-change-context-and-index-transactions.md) Git/PR snapshot identity and Plan 32 effect/audit/receipt contracts apply to workflow-owned effects, not outbound GitHub comment actions. |
| PR18 | Official API stabilization and SDKs: freeze names/schemas for the accepted PR17 graph, task-intelligence, and workflow semantics; publish OpenAPI/schema and first-party Rust/TypeScript/Python SDKs, docs, and conformance without moving scoring or runtime logic into clients. |
| PR19 | Compatibility migration, defragmentation, cutover, and deletion: resumable backfill, shadow parity, bounded cutovers, rollback window, V2 default, and removal of every superseded V1 path. |
| PR20 | End-to-end performance optimization: measured database, synchronization, projection, indexing, cache/generation, query, task-intelligence evidence/calibration/proposal paths, and repository-controlled developer-build improvements with Linux/Windows and crash/restart regression gates. |

PR #421 stays open through PR20. It merges only after PR20 and the aggregate
Linux, Windows, migration, recovery, privacy, performance, and deletion gates
are stable.

## Component-plan ownership

- Plans 01–04 and 18: PR5–PR7 capture, storage, privacy, identity, projection,
  recovery, and migration boundaries.
- Plans 05, 15, 23, 25, and 31: PR8–PR10 temporal, lexical, code, semantic,
  ranking, and evaluation behavior.
- Plan 24's PR17 vertical extends Plans 01–04 domain, owner-shard store, and
  generic projector infrastructure for product work entities and relations.
  Plan 24 owns typed work requests and projection semantics; Plan 05 supplies
  only shared execution primitives. PR17 creates no task-specific crate,
  database, projector runtime, board query DSL, or universal query AST.
- Plans 06, 08–10, 17, 20, 21, and
  [34](34-workspace-refactoring-and-api-migration.md): PR11–PR12 application,
  policy, configuration, catalog, transport, presentation, public contracts, and
  safe workspace refactoring. Plans 06, 08–10, 20, and 21 receive their Plan
  24/32 task-routing, configuration, or task/work extensions in PR17; Plan 17's SDK
  stabilization remains PR18.
- Plans 07 and 27: PR6 host/hook baseline and canonical integration model, then
  PR13 daemon cutover, host bundles, lifecycle, and conformance, then PR17
  addressed task-step execution bindings. Plan 22 owns PR13 Context Scout
  behavior.
- Plan [35](35-daemon-lsp-gateway-and-universal-diagnostics.md): PR9,
  PR11–PR13, PR14, PR15, and PR16 generation-bound diagnostics, analyzer
  policy, daemon LSP gateway, gateway-specific finding/state schema consumed by
  dashboard/Doctor, multi-root scope, and remote-node behavior.
- Plan [36](36-git-aware-change-context-and-index-transactions.md): PR7
  provenance anchors, PR9 read-only semantic Git evidence, PR11 safe
  daemon-serialized index/commit transactions, and PR12 CLI/MCP/HTTP bindings.
- Plan [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md):
  the architectural center for the PR11–PR17 branch-aware semantic
  feedback cycle, read-only GitHub review-comment ingestion/surfacing, CI-failure
  localization, and tiered concurrent-agent proximity. PR11–PR13 is the first
  coherent milestone with all four read-only/advisory pillars; PR13 is first
  availability of GitHub/CI/proximity adapters; PR14 dashboard/Doctor; PR15
  multi-root; PR16 remote; PR17 composes already-shipped advisory operations
  into [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) workflows without
  GitHub writes. Composes Plans 05/09/13/16/21/22/23/26/27/32/35/36 without
  owning their contracts.
- Plans 11, 14, and 26: PR14 product UI, Doctor, observability, regression, and
  operational quality, plus PR17 Work UI, task/model observations, and direct
  task-graph/runtime regressions.
- Plan 16: PR15 canonical scope. Plan 24: PR17 canonical product task/work
  graph, model-routing review, and Kanban/DAG/timeline/causal/workload
  projections; it is not a parser or executor for this developer roadmap.
- Plan 28: PR16 remote topology and authority.
- Plan 32: PR17 typed dynamic-workflow product and the sole runtime clock,
  scheduler, history, lease, attempt, effect, and artifact authority for
  executable Plan 24 task steps.
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

Plan 24 work graphs and PR17 workflows are explicit product data handled
through typed daemon operations. They cannot parse this roadmap, dispatch its
PRs, track rewrite completion, or act as a developer-plan executor.

## Delivery gate

For each PR: implement the smallest coherent vertical slice, run focused direct
tests, independently review the integrated diff, run the relevant broader stock
Cargo and cross-platform gates, and delete replaced paths when the rollback gate
permits it. From PR7 onward, include the developer-feedback measurements above
when the slice materially changes Rust compilation scope. Passing code and tests
in Git are the completion record.
