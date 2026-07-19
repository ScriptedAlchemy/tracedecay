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
- A consumer may compose an owning plan's typed result, but it may not duplicate
  that plan's identity, state machine, ranking, scheduling, policy, storage,
  health, remediation, measurement, or configuration semantics. Plan 05 owns
  shared query execution, Plan 23 temporal session/LCM retrieval, Plan 14 the
  Doctor/health kernel, Plan 20 configuration resolution, Plan 26 measurements
  and labels, and Plan 32 executable workflow runtime.
- High-confidence architecture and rejection findings are normative.
  Medium-confidence models, algorithms, topology choices, renderers, ranking
  profiles, calibration methods, and performance mechanisms are versioned
  measured candidates. Low-confidence or causal product-effect claims are not
  requirements or acceptance criteria without direct TraceDecay intervention
  evidence.
- Every opaque Plan 24 `TaskId` is an authorized retrieval root whose compact
  context remains losslessly expandable through Plan 23 narrative retrieval,
  Plan 13 anchors, Plan 25 code generations, and owning Git, CI, diagnostic,
  review, artifact, and runtime stores. Summaries and presentation projections
  never replace exact evidence or widen authority.
- Quantifiers preserve raw value/unit, denominator, coverage, cohort, temporal
  delta, provenance, uncertainty kind, and calibration validity. No universal
  quality, health, reward, readiness, or performance score is product truth;
  raw similarity, rank, centrality, and heuristic values are not probabilities.
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
| PR9 | Code intelligence and lexical retrieval: deterministic extraction with typed edge authority and coverage, exact occurrence identity plus evidenced/abstaining lineage, generation-bound managed diagnostics/tests, a non-demotable exact/phrase/BM25 tier, typed quantifier inputs, V1 parity, and typed read-only Git status/diff/history/blame/hunk intelligence enriched by graph impact. |
| PR10 | Native semantic retrieval and ranking: gated FastEmbed artifacts, immutable vector generations, exact flat-vector baseline/oracle, measured hybrid/reranking candidates, calibrated abstention, redundancy augmentation, and byte-stable lexical fallback; ANN, late interaction, and quantization are optional only after locked admission evidence. |
| PR11 | Policy, application, catalog, and configuration core: typed use cases, grants, routing, replay, operations, capabilities, analyzer policy/settings, one runtime configuration authority, daemon-serialized `stage_hunks`/`unstage_hunks`/`commit_index` transactions with `HunkRef` compare-and-swap and receipts, and the typed branch-aware feedback-cycle request/result and orchestration ([Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)) — first pillar of the PR11–PR13 read-only/advisory milestone (post-edit diagnostics and impact). |
| PR12 | CLI, MCP, HTTP API, LSP gateway, and output convergence: one revisioned schema registry, dispatcher, binding taxonomy, semantic problem model, capability intersection, and executable lifecycle/stream/cancellation contract; stable errors/cursors, compact Markdown, canonical JSON, managed diagnostics, semantic surface parity, shared Git preview/apply bindings, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s PR12 slice — [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway triggers plus the explicit diagnostics-call trigger/surface bound once through [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) — completing the post-edit diagnostics-and-impact pillar for LSP/MCP/CLI surfaces. |
| PR13 | Hooks, Context Scout, and host bundles: bounded hook ingestion, asynchronous suggestions, Codex/Claude/Cursor/Hermes/Kiro projections, one TraceDecay semantic/diagnostic contract delivered per host (Claude Code LSP plugin; Cursor desktop native-diagnostics adapter with duplicate-analyzer avoidance; hook/MCP/CLI capability paths for Cursor cloud, Codex, and other supported hosts as applicable), install/repair, stock-host conformance, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s PR13 slice — first availability of GitHub review-comment ingestion/surfacing, CI-failure localization, and tiered concurrent-agent proximity adapters through hooks/MCP/CLI, completing the PR11–PR13 read-only/advisory milestone with all four pillars (post-edit diagnostics+impact, CI localization, GitHub thread ingestion/surfacing, proximity). TraceDecay never posts, updates, resolves, replies to, or dismisses GitHub comments. |
| PR14 | Flagship dashboard, Doctor, observability, and configuration operations: renderer-neutral Brain/Explorer/Loom foundations with a permissive default renderer, accessible progressive disclosure, one Plan 14-owned truthful Doctor/health/remediation kernel, Plan 26-owned typed measurements/SLOs, Settings, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) dashboard/Doctor consumption of the typed feedback-cycle, GitHub-ingested review-thread, CI-localization, and proximity state shipped at PR13. The dashboard never computes a second health grade. |
| PR15 | Cross-project, repository, worktree, and local branch-topology behavior: canonical authorized scope-set resolution, frozen per-shard snapshot/continuation vectors, deterministic federation with typed coverage and rank fallback for incomparable scores, globally routable anchored evidence, graph/query/LSP workspace coverage, native worktree/local-stack inventory without path identity, Plan 36 clean authorized policy-approved fast-forward/merge/cherry-pick preflight/apply/receipts, central daemon signal fanout, and the optional private-preview GitHub Stacked PR read adapter (`Unavailable | PrivatePreviewDisabled | Enabled | Degraded`) with mandatory standard-Git/other-forge fallback. |
| PR16 | Remote shared Brain: enrolled nodes, one sink-enforced fenced authority per shard, duplicate-tolerant remote offline capture with exactly-once admitted effects/receipts, authenticated epoch-bound manifests, staged restore with deletion/privacy replay, verified caches/replicas, node-local LSP overlays/analyzers, Git correlation, backup, restore, and failover; CRDT or replicated-SQLite convergence is not canonical authority. |
| PR17 | Canonical host-neutral product task/work graph, lossless TaskId-rooted evidence retrieval, adaptive task intelligence, and typed workflows ([Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md), [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)): versioned user task/ticket DAG state, evidence/history relations, Kanban/DAG/timeline/causal/workload projections, task-shape/topology/calibrated-size assessment, reviewed parent/child decomposition and minimal repair, selective escalation, governed experience recall, typed handoff, model-capability profiles and explained routing, isolated independent review, outcomes/calibration, live proposals that never auto-apply, and graph-native typed auxiliary-attempt requests; plus daemon-owned definitions, deterministic replay, and one shared runtime-clock/scheduler/history/lease/attempt/effect/artifact kernel that executes explicitly admitted task steps through negotiated provider adapters. TaskId/Kanban never require Git, worktrees, branches, PRs, GitHub, or stacks. Execution placement, branch topology, review topology, and integration strategy are independent: no-Git tasks, unbranched/independent/locally stacked worktrees without PRs, and PR stacks without managed worktrees remain valid. Claude-designated work uses native Claude Code CLI, never Hermes Anthropic. Codex app-server is preferred; a distinct Codex CLI fallback is explicit, policy/configuration-bounded, and never hidden. Typed workflow steps may compose [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s already-shipped read-only advisory operations (feedback-cycle findings, GitHub-ingested review-thread surfacing, CI localization, proximity) — not first availability of those capabilities and no GitHub review-content writes. Plan 24 owns work-graph/proposal/auxiliary-request plus task-domain ready-node/decomposition/sizing/model-backend recommendation semantics; Plan 06 owns pure evaluator/policy-decision mechanics; Plan 26 owns observations/metrics; Plan 32 alone owns runtime clocks, provider-adapter execution, scheduling, leases, attempts, effects, retries, cancellation, and runtime receipts. [Plan 36](36-git-aware-change-context-and-index-transactions.md) owns native Git preflight/apply/receipt mechanics; Plan 32 effect/audit/receipt contracts orchestrate workflow-owned effects without becoming native Git authority. |
| PR18 | Official API stabilization and SDKs: freeze revisioned names/schemas for the accepted PR17 graph, task-intelligence, and workflow semantics; publish OpenAPI/schema and first-party Rust/TypeScript/Python SDKs with oldest-supported/current compatibility matrices plus structural, semantic, and lifecycle conformance, without moving scoring, policy, query, or runtime logic into clients. |
| PR19 | Compatibility migration, defragmentation, cutover, and deletion: destination-committed resumable backfill, read-only isolated shadow parity, bounded cutover, forward restoration into verified V2 during the recovery window, V2 default, explicit compatibility dispositions, and removal of every superseded V1 path. V1 archives are recovery input, never renewed authority; no reverse cutover, long-lived dual write, lazy read migration, or production shadow read remains. |
| PR20 | End-to-end performance optimization: measured database, synchronization, projection, indexing, cache/generation, query, task-intelligence evidence/calibration/proposal paths, and repository-controlled developer-build improvements gated by frozen workload identity, A/A noise floors, paired effect sizes and intervals, practical margins, worst-stratum/resource/tail results, open-loop overload accounting, recomputation equivalence, Linux/Windows, and crash/restart correctness rather than a universal score or paper threshold. |

PR #421 stays open through PR20. It merges only after PR20 and the aggregate
Linux, Windows, migration, recovery, privacy, performance, and deletion gates
are stable.

## Component-plan ownership

- Plans 01–04 and 18: PR5–PR7 capture, storage, privacy, identity, projection,
  recovery, and migration boundaries.
- Plans 05, 15, 23, 25, and 31: PR8–PR10 temporal, lexical, code, semantic,
  ranking, and evaluation behavior. Plan 05 owns shared query execution, Plan
  23 alone owns current/as-of/evolution/forensic session narrative retrieval,
  Plan 25 owns exact code generations and typed graph evidence, Plan 15 owns
  locked retrieval/quantifier evaluation, and Plan 31 owns the optional
  semantic representation/search profile. Exact lexical inclusion remains
  authoritative before PR10 augmentation.
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
  stabilization remains PR18. Plan 20 alone owns configuration definitions,
  precedence, snapshots, behavior/provenance digests, activation, and audit;
  consumers pin its revisioned result rather than rereading or resolving
  configuration locally.
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
  task-graph/runtime regressions. Plan 11 owns renderer-neutral presentation
  and no backend truth; Plan 14 alone owns Doctor/health/remediation
  composition and cross-cutting failure fixtures; Plan 26 alone owns
  measurement descriptors, cohorts, labels, calibration/drift observations,
  and denominator-safe read models.
- Plan 16: PR15 canonical scope. Plan 24: PR17 canonical product task/work
  graph, model-routing review, and Kanban/DAG/timeline/causal/workload
  projections; it is not a parser or executor for this developer roadmap.
- Plan 28: PR16 remote topology and sink-enforced fencing authority. Immutable
  capture structures may support integrity, dedupe, and gap evidence but never
  multi-primary canonical mutation.
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

## Cross-cutting source, retrieval, and surface contracts

These contracts are already owned by numbered plans. They do not change the
active PR8 Session/LCM temporal-retrieval priority in [NEXT.md](NEXT.md).

Dependency order (consume only after the predecessor publishes typed results):

1. [Plan 01](01-domain-crate.md) generic external-source identities, definitions,
   and owner bindings; [Plan 27](27-cross-host-agent-plugin-bundles.md) connector
   contracts; [Plan 03](03-capture-crate.md) capture/sanitization.
2. [Plan 20](20-configuration-control-plane.md) `SourcePolicyMetadataSnapshotV1`,
   bindings, and other policy metadata — never definition-associated privacy.
3. [Plan 06](06-policy-crate.md) pure source-authorization intersection over Plan
   20 policy metadata plus grants/scope/sinks; [Plan 09](09-application-crate.md)
   loads snapshots, rechecks sinks, and orchestrates effects.
4. [Plan 13](13-research-provenance-and-context-anchors.md) retrieval anchors and
   immutable `EvidenceSpanRecordV1` / retriever-contribution identity.
5. [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md) session/LCM
   temporal truth and PR8 immutable derived evidence spans/bursts (active now).
6. [Plan 16](16-cross-project-repository-worktree-scope.md) `QueryCollection` /
   `WorkspaceCollection` and authorized scope-set resolution; [Plan 05](05-query-crate.md)
   shared federated query execution primitives (PR15 multi-root federation).
7. [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) LSP projection and
   one-way investigation handoff tokens; Plan 09 owns transport-neutral
   investigation handoff results.
8. [Plan 11](11-dashboard-frontend.md) renderer-neutral Brain/Explorer/Loom
   investigation journey and PR17 Work projections; no backend truth.
9. [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) TaskId-rooted
   investigation/evidence packets; [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)
   optional synthesis and runtime execution (PR17). Canonical retrieval rejects
   demonstrated expertise; expertise may exist only in an authorized ephemeral
   interactive view.
10. [Plan 26](26-observability-accounting-and-usage.md) measurement descriptors and
    retrieval/synthesis observations (PR14 Observatory/Costs); [Plan 33](33-end-to-end-performance-optimization.md)
    PR20 cross-path performance optimization after owning slices instrument baselines.

Ownership summary:

- **Source contracts:** Plan 01 definition/binding identity; Plan 20 policy
  metadata including mandatory local privacy; Plan 06 evaluation; Plan 03/27
  capture and connectors. Definitions never own privacy or sinks.
- **Federated retrieval / query collections:** Plan 16 owns collection identity,
  membership, and scope-set digests; Plan 05 executes federation without granting
  ownership or authorization by membership.
- **Evidence spans:** Plan 23 owns PR8 session-derived spans/bursts; Plan 13 owns
  cross-domain `EvidenceSpanRecordV1`; Plan 24 references span IDs/anchors only.
- **Task investigation / evidence / synthesis:** Plan 24 owns task-root evidence
  packets and graph semantics; Plan 32 alone owns synthesis attempts, leases, and
  runtime receipts; Plan 06 owns pure routing evaluators.
- **Execution and delivery topology:** Plan 24 owns the four independent
  semantic dimensions; Plan 16 owns repository/worktree/local-stack identity
  and scope; Plan 27 owns capability probing/packaging; Plan 37 owns GitHub
  stack snapshots and central advisory fanout; Plan 36 owns typed native Git
  preflight/apply/receipt mechanics; Plans 20/21/35 gate policy, transport, and
  handoff exposure; Plan 32 owns runtime orchestration, never native Git
  mechanics. Standard Git/other-forge and no-Git paths remain mandatory.
- **LSP handoff:** Plan 35 encodes short-lived cues/tokens; Plan 09 authorizes
  investigation availability and links; Plan 21 binds surfaces.
- **Dashboard:** Plan 11 presents Plan 09/14/24/26/37 envelopes; Plan 14 alone
  owns Doctor/health/remediation composition.
- **Observability / performance:** Plan 26 owns labels, cohorts, and observation
  schemas; Plan 33 owns PR20 measured optimization. Neither duplicates query,
  policy, graph, or Doctor authority.

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
