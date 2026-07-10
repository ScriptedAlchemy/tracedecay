# TraceDecay V2 Application Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `tracedecay-application`, the transport-neutral use-case layer that authorizes and orchestrates every TraceDecay V2 read, command, replay lab, export, migration, and internal parity operation through one auditable contract.

**Architecture:** Queries compose catalog, query, policy, tool-catalog, projector, and immutable archive ports under one captured request context and return explicit snapshot, coverage, freshness, redaction, and provenance. Non-curation commands use typed execution contracts and, when destructive, preview/confirmation; all commands use idempotency, optimistic aggregate versions, one owning-shard unit of work, one authoritative canonical command-event journal, referenced audit/outbox entries, and resumable workflows for cross-shard effects. Autonomous curation effects have no per-item command. HTTP, CLI, MCP, hooks, and dashboard adapters only map transport data to these use cases.

**Tech Stack:** Rust 2024 workspace; `tracedecay-domain`; `tracedecay-query`; `tracedecay-policy`; `tracedecay-tool-catalog`; store/projector traits; `serde`; `schemars`; `thiserror`; `futures`; `tokio` at the composition boundary; `uuid`; property/contract/differential tests.

---

## 1. Contract Lock

This plan refines master-plan PR 24A, supplies the application contracts consumed by PRs 24B–24E and 25–32, and owns transport parity until V1 retirement.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) adds task/plan registered values and builders for canonical `TraceQueryV1`, command use cases, one authoritative scheduler, owner-shard graph transactions, executor registration/route resolution, fenced lease-acquisition/heartbeat/terminal workflows, context-packet assembly, workspace/cancellation/effect reconciliation, status, and doctor under this application boundary. `WorkClaimV1` remains advisory coordination evidence; only `work_items.acquire_lease` issues execution authority. No root/adapter/dashboard module may become a second query engine, scheduler, event journal, or lease authority. Those task/plan reads and commands are enumerated in the Section 9–10 inventories below; every task mutation is a POST command-envelope use case (plan 10 §8.7) with no PATCH transport shape.

- The application crate owns use-case identity, authorization, orchestration, request deadlines, non-curation command execution/confirmation, autonomous curation effect application, idempotency, optimistic versions, audit requirements, export/job lifecycle, and bounded migration dispatch.
- `tracedecay-domain` owns canonical IDs, scope, evidence, sensitivity, watermarks, the sole `TraceQueryV1` AST, and command envelopes. Application types wrap these contracts; they do not create task selectors, board DSLs, or string substitutes. Task convenience inputs compile losslessly to registered values in `TraceQueryV1` and expose the canonical digest.
- `tracedecay-query` owns planning, federated reads, ranking, cursors, exports bytes, and live snapshot/delta semantics. Application authorizes and selects query profiles; it does not inspect SQL or re-rank rows.
- `tracedecay-policy` owns deterministic evaluation and proposed effects. Application assembles immutable inputs, invokes the runtime, and transactionally revalidates effects. The curation worker then autonomously records/applies eligible owned memory/fact/skill/profile-curation effects; it never waits on a per-item preview/approval/apply action.
- `tracedecay-tool-catalog` owns declarative capability metadata and generated transport mappings. Application implements the stable `UseCaseId`s referenced by that catalog and fails CI on missing or duplicate ownership.
- [`20-configuration-control-plane.md`](20-configuration-control-plane.md) owns the configuration registry/resolution semantics. `tracedecay-application::configuration` is their sole resolver and mutation owner; every other application use case consumes its pinned effective digest.
- [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) requires sealed typed semantic views and one typed outcome/page/notice/freshness/provenance contract. Application constructs those views once; transports and renderers cannot repair or reinterpret them.
- [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) assigns the daemon's asynchronous context-scout workflow, bounded read/model ports, envelope transactions, status, and exact pending-delivery claim to application; hooks never own its orchestration.
- [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md) assigns authorized temporal search/context/replay/corpus/evaluation use cases to application while query owns ranking and temporal resolution. No adapter performs search-to-load routing or synthesis fallback locally.
- `18-secret-detection-redaction-and-private-data-safety.md` owns the mandatory sanitizer and taint-state types. Application accepts new content as `Unclassified<T>`, invokes the one sanitizer port, and passes only sink-eligible wrappers or typed redacted/denied/unknown states to stores, projectors, policy, audit, transports, exports, and workflows. It cannot bless a raw `String`, JSON value, summary, compatibility row, or error detail locally.
- Store/projector/archive implementations enter through narrow ports or sibling-crate public traits. No connection, transaction, SQL string, filesystem path, Axum, MCP, CLI, React, renderer, or provider hook type crosses this boundary.
- One command transaction has exactly one canonical owning shard. Cross-shard work is an explicit durable workflow with steps, expected versions, idempotency keys, compensations where safe, and partial/failure state; V2 does not emulate distributed atomicity.
- A query may span shards but captures one vector watermark and preserves every stale, unavailable, incompatible, locked, skipped, redacted, sampled, and truncated disposition.
- Labs are read-only use cases. Fixture promotion, policy/config activation, export publication, and non-curation mutations are separate audited commands. Curation labs are inspectors only: the autonomous curation worker applies policy-eligible fact/memory/skill evolution independently, with no per-item preview/apply/rollback UI.
- Canonical transcript enumeration preserves every sanitized native row and is lossless for retained non-secret structure/semantics. Message enumeration/search consumes domain `MessageOrigin`/`MessageView` unchanged, exposing native, representative, human-best-effort, direct-user, delegated-agent, tool-result, and provider-protocol views; query-time dedupe never deletes or rewrites sanitized source observations.
- `All` means the active `ProfileId`. Additional profiles require an explicit collection/scope and separate authorization; there is no implicit Hermes profile.
- Every fact, skill, policy, automation, saved investigation, and annotation carries domain `DeclaredScope`. Profile/zero-project/cross-project instances are activity-owned; explicitly project-scoped instances are project-owned. A selected project, route, working directory, or active filter is never an ownership default, and unresolved scope blocks mutation rather than guessing.
- V1 readers exist only as internal shadow/backfill/parity adapters during bounded migration. Once a use case becomes V2-default, old live MCP/CLI/HTTP/plugin names and schemas are not executable fallbacks; stale clients receive a typed version mismatch with restart/update/current binding guidance. Non-disposable V1 data remains preserved until migration and rollback receipts close.

## 2. Goals

- Give every existing and V2 capability one stable, typed application use case shared by HTTP, CLI, MCP, hooks, dashboard commands, automation, and tests.
- Make the default Brain/All reading path, graph-of-graphs lenses, Explorer, Causal Loom, domain workspaces, Observatory, Costs, replay labs, and Evolution Studio compositions first-class use cases rather than UI-side query choreography.
- Enforce authorization and sensitivity before query planning, payload hydration, replay, export, remote refresh, or mutation.
- Make partial coverage, freshness, vector watermarks, retention boundaries, inference/evidence, and redaction impossible for adapters to omit.
- Separate read-only queries from state-changing commands in types, traits, catalog metadata, audit behavior, retry behavior, and transport generation.
- Guarantee idempotent command retry and compare-and-swap aggregate updates without holding database transactions over network, UI, model, GitHub, process, or filesystem work.
- Support many simultaneous agents and readers without one application request retaining a shard read transaction across pages or user think time.
- Preserve V1 behavior evidence through internal parity profiles and differential receipts rather than duplicating V1 service logic or exposing stale live aliases.
- Provide one parity harness proving identical application semantics across in-process, HTTP, CLI JSON, MCP JSON, dashboard client, export, and subscription transports.
- Make every user-visible mutation produce a durable command receipt and audit event linked to actor, scope, request, preview, applied version, resulting events, and any workflow.

## 3. Non-Goals

- No transport parsing, HTTP status selection, SSE framing, markdown rendering, terminal formatting, browser state, or dashboard visualization code.
- No SQL, database migration, WAL/lock management, blob path manipulation, source parsing, projection, ranking, policy bytecode, Git command, GitHub call, provider hook acknowledgement, or daemon lifecycle implementation.
- No general distributed transaction coordinator. Cross-shard workflows expose progress and compensation rather than promising all-or-nothing completion.
- No hidden chain-of-thought reconstruction. Reasoning use cases can return only retained provider-exposed artifacts and unavailable/redacted coverage.
- No arbitrary remote write actions in the first V2 default. Live GitHub/delivery refresh is read-only and allowlisted; PR mutation remains outside scope.
- No ambient authorization, clock, active profile, current directory, process environment, or random idempotency behavior. Adapters/composition supply every request fact explicitly.
- No direct application mutation from replay labs or preview endpoints.

## 4. Incoming-Master and V1 Inputs

### 4.1 Master and incoming changes verified on 2026-07-10

Publication refresh: `origin/master` `3567e31e3a60730400c9b900e32ca02c0bf3bf33` at 0.0.48. PRs #418 and #425 are merged; #425 final head `d3bb28b5` merged as `de3d05dc`. Only draft plan PR #421 was open at final refresh. Proxy routing, catalog refresh, fact ranking, exact analytics, release metadata, and the offline fail-closed resumable two-nonempty-profile-shard consolidation workflow are accepted application inputs; consolidation remains operator administration, not autonomous curation.

| Change | Assumed future behavior | Application consequence |
|---|---|---|
| Merged PR #405, legacy identity-store adoption | The lifecycle resolver adopts uniquely matching legacy stores and records migration/adoption evidence. | Scope resolution and migration commands consume canonical post-adoption `ShardRef`s. Preview must surface ambiguous/split identities and block cutover rather than exposing duplicate projects. |
| Merged PR #412, safe daemon drain during upgrades | Daemon/MCP/watch/index work is leased, drainable, recoverable, and reports update safety state. | Operation/status reads and update/daemon commands preserve lease epoch, accepting/draining/stopped state, in-flight counts, progress, takeover/recovery, and last durable receipt; “process exited” is not equivalent to safely drained. |
| PR #407, Hermes user-profile consolidation | Hermes sources/facts/sessions migrate into the ordinary user TraceDecay profile and Hermes-specific bridges are removed. | Hermes, curator, reflector, and skill-writer are actors/workflows inside the active profile. No use case accepts an implicit Hermes-profile switch or calls removed bridge/config/inventory paths. |
| Merged PR #410, session-query dedupe and author classification | Sanitized native transcript rows remain preserved while query-time parent representative dedupe and direct-user/subagent/tool-result filters are available across message search, LCM, MCP, and CLI. | `ListMessages`, `SearchMessages`, session replay, export, and parity contracts consume domain `MessageOrigin`/`MessageView` unchanged and carry representative provenance, suppression count, and native-row expansion. V2 never treats representative rows as canonical storage. |
| Merged PR #411, foreign-installation doctor severity | Foreign-owned skill packages are informational, not an update/remediation failure owned by TraceDecay. | Doctor findings carry severity, observed owner, authority, evidence, and legal remediation. Application cannot offer apply/update when ownership is foreign or unknown. |
| Merged PR #414, `tracedecay_move_symbol` | Current MCP adds a dry-run-by-default symbol relocation with destination-first rollback, import insertion, impact classes, collisions/cycles/module/visibility evidence, and no automatic caller rewrite. | Add cataloged `code.move_symbol.inspect` and confirmed `code.move_symbol.commit` use cases with exact source/destination snapshot/version, filesystem port, idempotency, sanitization, impact evidence, recovery receipt, and CLI/MCP/API/SDK/dashboard parity; generic query/edit helpers cannot hide them. |
| Merged PR #415, release-PR integrity | Trusted-base release guard rejects unexpected files, tracked ignored files, and dirty release-plz generation. | Generated catalog/OpenAPI/SDK/dashboard/release artifacts require an allowlisted deterministic manifest; application fixtures cannot be silently deleted by release packaging. |
| Merged PRs #413/#416/#418, releases v0.0.46/v0.0.47/v0.0.48 | Source 0.0.48 merged at `3567e31e`; the frozen planning runtime remained installed 0.0.47. | Regenerate version/catalog/compatibility fixtures from `3567e31e`; create no semantic dependency on release-PR layout and require release artifact inventory parity before claiming a host is upgraded. |
| Merged PR #417, doctor identity-split visibility | Error-aware store resolution distinguishes split-store conflict from no index and preserves both stores unchanged. | Add a typed `identity_split` health/error state with exact safe candidate inventory and backup/consolidation preview; never offer `init` or claim absent/healthy when identity is ambiguous. |
| Merged PR #425, explicit split-store consolidation (`de3d05dc`, final head `d3bb28b5`) | Plan/apply freezes both SQLite families, identifies holders by path plus file/inode, blocks unsupported/open holders, reserves writes, backs up both inputs, stages deterministic merge/rebuild/reject dispositions, verifies exhaustively, cuts markers atomically, and resumes/recovers by durable ledger. | Preserve it as accepted V1 anti-corruption behavior behind a capability-gated operator workflow with two explicit source identities, deterministic confirmation, holder/lease/write-reservation state, backup/staging/verification/cutover receipts, and exact recovery. V2 names operation-specific plan/start/recover use cases rather than creating a universal preview/apply framework. It is never a Settings patch, task command, or autonomous curation effect. |
| Merged PR #419, race-safe `move_symbol` writes | Revalidates source/destination snapshots and same-file identity, rejects symlink escapes, uses atomic sibling renames, and preserves concurrent rollback edits. | Every edit command has exact identity/version preconditions, last-moment revalidation, race-safe filesystem ports, and typed commit/recovery conflicts; a prior inspection is not permission to overwrite drift. |
| Merged PR #420, early daemon proxy/hot swap | Chooses managed-daemon authority before local store resolution/open; reconnects per request without replaying writes and requires a new host session for incompatible schemas. | Root/application context declares authority/reconnect state before use-case execution; uncertain writes are never retried, and typed guidance distinguishes reconnect from restart/new-session/tools-list refresh. Merged #422 adds generation-scoped `tools.listChanged` refresh for compatible catalogs. |

Before each PR 24 slice, refresh open PRs, accepted merge bases, catalog digests, and compatibility inventory. If source code or generated inventory differs from this snapshot, update the slice receipt before implementation; never silently bind application semantics to stale branches.

### 4.2 V1 seams and ownership

| V1 seam | Existing responsibility | V2 application treatment |
|---|---|---|
| `src/mcp/tools/handlers/**` and `src/mcp/server.rs` | Scope resolution, SQL/service calls, truncation, mutation, markdown/JSON rendering, response handles | Move scope/auth/orchestration into use cases one domain at a time. MCP retains argument conversion and rendering only. Structured pagination uses V2 cursors before renderer truncation. |
| CLI handlers under `src/cli/**` | Parse flags, select stores, execute operations, print results | Compare old parsing/output only in the migration harness; live CLI exposes current generated bindings over the same `UseCaseId` as HTTP/MCP. |
| `src/dashboard/**` API/plugin state | Direct reads, plugin-specific queries, settings and operational mutations | Route every read/action through application. No dashboard-only command or query survives compatibility retirement. |
| `src/global_db.rs`, `src/storage.rs`, graph/session/memory repositories | Persistence plus application decisions in broad types | Application consumes narrow V2 ports; V1 access is isolated behind internal shadow/backfill adapters and never leaks row IDs/types into public results or becomes a post-cutover live fallback. |
| `src/sessions/lcm/query.rs` and message search | Session/message search, representative selection, replay, status, compression, payload operations | Split into typed read use cases and explicit commands. Preserve #410 raw/native and representative views, author filters, source provenance, and expansion. |
| `src/sessions/git_correlation.rs` and Git MCP tools | Local semantic Git, live delivery state, correlation and tool-specific rendering | Use graph/delivery read compositions plus policy reconciliation. Local and live revisions retain separate freshness/watermarks; drift blocks joined conclusions. |
| `src/hooks/**` | Normalize hooks, classify hints, inject, persist outcomes | Hook adapters call narrow evaluation/record ports. Application records evaluation/state transition and proposed effects; hook transport only renders/acknowledges. |
| `src/memory/**` | Fact reads, retrieval, trust, proposals, mutations, curation | Read via query/policy compositions. Autonomous curation uses expected versions, evidence/privacy/ownership gates, audit, staged monitoring, and automatic recovery; no proposal approval/apply queue survives. User-facing controls configure policy, pause/resume/run-now, pin/protect/exclude, and submit feedback rather than adjudicating each candidate. |
| `src/automation/**` | Config, scheduling, leases, runs, skills, proposals, artifacts, outcomes | Expose status/read models and typed commands. Scheduler policy proposes; application revalidates and acquires fenced lease before launch. |
| Doctor/index/watch/daemon/migration/backup code | Operational reads and side effects selected ad hoc by caller | Separate inspect/preview queries from execute commands/jobs. Every long operation has durable progress, cancellation rules, receipt, and recovery state. |

## 5. Exact Crate and Companion File Tree

```text
crates/tracedecay-application/
├── Cargo.toml
├── src/
│   ├── lib.rs                         # curated public use-case API
│   ├── error.rs                       # stable application error codes and retry classes
│   ├── context.rs                     # RequestContext, Principal, deadline, locale-safe clock
│   ├── access.rs                      # AuthorizationPort and authorized scope/payload decisions
│   ├── use_case.rs                    # UseCaseId, QueryUseCase, CommandUseCase, descriptors
│   ├── registry.rs                    # implementation registry checked against tool catalog
│   ├── response.rs                    # ApplicationResponse, coverage/freshness/audit metadata
│   ├── unit_of_work.rs                # single-owner transaction and durable workflow ports
│   ├── idempotency.rs                 # reservation, replay, conflict, completed-result contract
│   ├── audit.rs                       # immutable audit envelope and redacted summaries
│   ├── optimistic.rs                  # version/revalidation tokens and conflict views
│   ├── privacy.rs                     # sanitizer port, output-eligibility seal, privacy status/workflow mapping
│   ├── jobs.rs                        # resumable operation/job lifecycle
│   ├── migration.rs                   # bounded shadow dispatch, parity receipt, removal state
│   ├── ports/
│   │   ├── mod.rs
│   │   ├── catalog.rs                 # scope/profile/shard/capability inventory reads
│   │   ├── evidence.rs                # entity/event/relation hydration and owner lookup
│   │   ├── command_store.rs           # aggregate load, append, idempotency, audit transaction
│   │   ├── workflow_store.rs          # durable cross-shard workflow/checkpoint operations
│   │   ├── archive.rs                 # immutable bundle/input/recorded-result reads
│   │   ├── remote_delivery.rs         # allowlisted read-only live Git/delivery refresh
│   │   ├── capture.rs                 # source ingest/status/cutover command port
│   │   ├── projection.rs              # projector status/rebuild/cutover command port
│   │   ├── operations.rs              # doctor/index/watch/backup/repair/GC adapters
│   │   ├── hooks.rs                   # HookApplicationPort evaluation/delivery boundary
│   │   └── event_sink.rs              # canonical command/evaluation/outcome append port
│   └── use_cases/
│       ├── mod.rs                     # only executable capability registry entrypoints
│       ├── query.rs                   # generic TraceQueryV1 execution
│       ├── search.rs                  # universal search profile
│       ├── graph.rs                   # neighborhood/path/impact/lens composition
│       ├── timeline.rs                # density/lanes/as-of/follow/compare compositions
│       ├── export.rs                  # export creation/status composition
│       ├── subscribe.rs               # authorized snapshot/delta/gap subscription
│       ├── capabilities.rs            # catalog and implementation availability/drift
│       ├── scopes.rs                  # lazy profile/project/worktree/ref/snapshot tree
│       ├── brain.rs                   # All reading path and graph-of-graphs summaries
│       ├── activity.rs                # consequential cross-domain activity/facets
│       ├── sessions.rs                # sessions/messages/turns/context lineage
│       ├── agents.rs                  # actors, goals, workflows, handoffs, outcomes
│       ├── coordination.rs            # presence, proximity, overlap, safe summaries
│       ├── code.rs                    # code search/context/diagnostics/tests/impact
│       ├── delivery.rs                # Git branches/commits/PRs/checks reconciliation
│       ├── knowledge.rs               # facts/entities/trust/conflicts/retrieval history
│       ├── automation.rs              # jobs/runs/skills/proposals/artifacts/outcomes
│       ├── observatory.rs             # health/coverage/ingest/projection/privacy/migrations
│       ├── privacy.rs                 # policy/coverage/findings/scan/remediation/quarantine reads
│       ├── accounting.rs              # usage/cost/savings and denominators
│       ├── settings.rs                # effective values, sources, scope, and impact
│       ├── operations.rs              # durable command/job/workflow status/recovery
│       ├── research.rs                # stable evidence anchors and retrieval recipes
│       ├── saved.rs                   # saved views, collections, annotations reads
│       ├── hooks/
│       │   ├── mod.rs                 # narrow hook use-case façade
│       │   ├── capture.rs             # captured-observation/request-facts validation
│       │   ├── evaluate.rs            # pinned query/policy/catalog/state composition
│       │   └── deliver.rs             # delivery receipt/terminal-outcome recording
│       ├── commands/
│       │   ├── mod.rs
│       │   ├── runner.rs              # execution-mode dispatch and command receipts
│       │   ├── projects.rs            # register/alias/unenroll
│       │   ├── operations.rs          # index/watch/doctor/repair/backup
│       │   ├── automation.rs          # job CRUD/run/pause/resume/cancel
│       │   ├── curation.rs            # autonomous fact/memory/skill evolution worker
│       │   ├── curation_control.rs    # config, pause/resume/run-now, pin/protect/exclude
│       │   ├── memory.rs              # explicit feedback and non-curation admin deletion
│       │   ├── policy.rs              # publish/activate/rollback
│       │   ├── settings.rs            # scoped config patches
│       │   ├── diagnostics.rs         # refresh operation
│       │   ├── payloads.rs            # retention/delete/hold/GC workflows
│       │   ├── capture.rs             # ingest/preflight/compress/boundary controls
│       │   ├── projections.rs         # rebuild/pause/resume/publish/rollback
│       │   ├── migrations.rs          # backfill/reconcile/cutover/rollback
│       │   ├── delivery.rs            # read-only remote evidence refresh
│       │   ├── coordination.rs        # message/handoff/ack/suppress overlap actions
│       │   ├── exports.rs             # create/cancel/publish/delete export jobs
│       │   ├── tokens.rs              # auth.tokens.create/list/revoke over plan 17 §18.2's registry
│       │   ├── saved.rs               # save/share/update/delete investigation state
│       │   └── labs.rs                # sanitized fixture-promotion command only
│       └── labs/
│           ├── mod.rs
│           ├── hint.rs
│           ├── retrieval.rs
│           ├── ingest.rs
│           ├── query.rs
│           ├── search_quality.rs
│           ├── scope_federation.rs
│           ├── privacy.rs
│           ├── correlation.rs
│           ├── coordination.rs
│           ├── scheduler.rs
│           ├── orchestration.rs
│           ├── memory.rs
│           ├── policy_diff.rs
│           └── evolution.rs
├── tests/
│   ├── support/mod.rs
│   ├── registry_completeness.rs
│   ├── authorization_privacy.rs
│   ├── query_coverage.rs
│   ├── message_representation.rs
│   ├── graph_of_graphs.rs
│   ├── command_pipeline.rs
│   ├── idempotency_optimistic.rs
│   ├── workflow_recovery.rs
│   ├── labs_read_only.rs
│   ├── future_master_migration.rs
│   └── v1_parity.rs
└── benches/
    ├── brain.rs
    ├── commands.rs
    └── subscriptions.rs
```

Companion implementations owned by later adapter PRs:

```text
crates/tracedecay-api/src/**
src/cli/v2_adapter/**
src/mcp/v2_adapter/**
src/hooks/v2_adapter/**
src/dashboard/v2_compat_api/**
tests/v2_transport_parity/**
tests/fixtures/v2/use-case-catalog.json
tests/fixtures/v2/v1-compatibility.json
```

Canonical composition rule: concrete glue for capture, projectors, query, and policy archives lives only at `src/v2_adapters/{capture_store,projector_store,query_store,policy_archive}/**`. Application retains only the ports above. The `src/use_cases/{query,search,graph,timeline,export,subscribe}.rs`, `src/use_cases/hooks/**`, and `src/use_cases/labs/**` paths remain exactly as plans 05–07 require.

The later agent-coordination/search-quality requirement adds bounded companion files under existing lower-crate owners: `tracedecay-projectors/src/read_models/coordination.rs`, `tracedecay-query/src/profiles/{hybrid_search,search_benchmark,agent_proximity}.rs`, `tracedecay-policy/src/evaluators/coordination.rs`, and generated tool-catalog definitions/bindings. Application consumes those ports; it does not reimplement projection, ranking, or hint policy. These additions extend PRs 16/17/23C/22A before PR 24A4/24A7 and require their own registry/parity receipts.

Production modules target at most 800 lines. Domain-specific orchestration stays in its query/command file; transport-specific mapping stays in adapters.

## 6. Dependency and Forbidden-Import Rules

```text
tracedecay-domain
  ↑
  ├── tracedecay-query
  ├── tracedecay-policy
  ├── tracedecay-tool-catalog
  ├── tracedecay-store/projector/capture public ports
  └──────────────┬───────────────────────────────
                 ↑
        tracedecay-application
                 ↑
        hooks / CLI / MCP / HTTP / dashboard
```

- Application may depend on public contracts from domain, query, policy, tool catalog, capture, projectors, and store. It may not depend on the root crate or any V1 concrete type.
- Query/policy/tool-catalog/store/projectors/capture may not depend on application.
- `queries/**` may use read ports only. A compile-time architecture test rejects `CommandStorePort`, workflow mutation, effect apply, or usage-counter ports from those modules.
- `labs/**` may use immutable archive/query/evaluator ports only. The only fixture-write operation is `commands/labs.rs`, which requires a sanitized artifact and explicit confirmation receipt.
- `commands/**` cannot call an HTTP/GitHub/process/filesystem adapter while a unit of work is open. External operations run before revalidation or after durable workflow-step commit.
- Reject imports containing `axum`, `tower`, `rmcp`, `clap`, dashboard packages, `rusqlite`, `libsql`, `git2`, `octocrab`, `reqwest`, `std::process`, or provider-specific hook modules.
- A `cargo metadata` architecture test asserts adapters point inward and no cycle exists among application/query/policy/store/projectors.

## 7. Application Kernel Contracts

### 7.1 Request, principal, and response

```rust
#[derive(Clone)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub principal: Principal,
    pub active_profile: ProfileId,
    pub issued_at: UtcMicros,
    pub deadline: Deadline,
    pub cancellation: Arc<dyn ApplicationCancellation>,
    pub locale: LocaleId,
    pub client: ClientDescriptor,
}

#[derive(Clone, Debug)]
pub struct Principal {
    pub subject: ActorRef,
    pub authentication: AuthenticationClass,
    pub grants: GrantSet,
    pub session_digest: ContentDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationResponse<T> {
    pub request_id: RequestId,
    pub use_case: UseCaseRef,
    pub catalog_snapshot: CatalogSnapshotRefV1,
    pub data: T,
    pub resolved_scope: ScopeResolutionV2,
    pub snapshot: Option<FrozenSnapshot>,
    pub coverage: CoverageReportV1,
    pub freshness: FreshnessReport,
    pub redactions: RedactionReport,
    pub retention: EvidenceRetentionWatermark,
    pub limits: AppliedLimits,
    pub warnings: Vec<ApplicationWarning>,
}
```

The context captures time once. Relative query times, command expiry, cursor validity, policy effective time, audit time, and authorization decisions derive from that value. No use case reads the ambient clock. `CoverageReportV1` is plan 01's canonical shared coverage type, consumed unchanged.

`AuthenticationClass` and `GrantSet` are concrete contracts, not open strings:

```rust
pub enum AuthenticationClass {
    BrowserSession,                       // plan 10 §10.2 cookie + CSRF
    BearerToken { token_id: ApiTokenId }, // scoped/TTL/revocable registry token, plan 17 §18.2
    BootstrapLaunch,                      // per-launch secret; only legal command is auth.tokens.create
    LocalProcess { os_user: OsUserRef },  // in-process CLI/MCP composition
    InternalWorker,                       // curation/workflow/scheduler actors with recorded provenance
}

pub struct GrantSet {
    pub capabilities: BTreeSet<CapabilityGrant>, // Read | Preview | Mutate | Admin | named destructive grants
    pub scope_constraints: Vec<ScopeSelectorV2>,
    pub sensitivity: SensitivityGrantSet,
    pub expires_at: Option<UtcMicros>,
}
```

Composition mints the in-process `Principal` for CLI and MCP adapters: it verifies the invoking OS user against the profile owner, resolves the operator's local token grants, and constructs `LocalProcess` principals explicitly. CLI inherits the operator token's grants; local MCP agent hosts default to Read+Preview and receive Mutate/Admin only from an explicit scoped token (plan 17 §17's read-only default for direct agent credentials). No adapter constructs an ambient admin principal, and the per-launch bootstrap bearer (plan 10 §10.2) authenticates only `auth.tokens.create` for the initial admin-class token.

`ApplicationError` stable codes include `invalid_input`, `client_update_required`, `daemon_restart_required`, `capability_replaced`, `not_authenticated`, `scope_not_found`, `scope_ambiguous`, `scope_denied`, `identity_split`, `ownership_unresolved`, `payload_denied`, `payload_redacted`, `capability_unavailable`, `freshness_required`, `version_conflict`, `idempotency_conflict`, `preview_expired`, `revalidation_failed`, `workflow_in_progress`, `workflow_failed`, `read_only_lab`, `partial_result_disallowed`, `deadline_exceeded`, `cancelled`, `retention_crossed`, and `internal_invariant`. It carries only the canonical safe problem inputs: code, `CatalogSafeText`, retry/restart/current-binding directive, correlation ID, safe scope candidates, invalid fields, optional current aggregate version, and optional operation ref. `identity_split` includes safe candidate/adoption evidence and legal backup/consolidation preview but never maps to “initialize.” Transport status/formatting is plan 10's mapping; no transport creates another semantic error enum. Bounded failure reasons and compatibility errors cross the output-safety seal and never include raw request, command, query, summary, provider error, or secret content.

Stale-client codes are exactly the plan 17 §12 contract-IR registry — `client_update_required`, `daemon_restart_required`, and `capability_replaced { current_binding }` — with no locally minted variants. `error.rs` also owns the retry classes: `RetryDirective` is the tagged union below (with `RestartDirective` as its restart payload); plan 17 §12 reproduces it verbatim for SDKs and adds no variants.

```rust
pub enum RetryDirective {
    Never,
    SameRequestAfter { delay: DurationMicros, condition: RetryCondition },
    RetryWith { canonical_request: CanonicalRequestRef },
    RestartPagination { request_without_cursor: CanonicalRequestRef, reason: CursorRestartReason },
    PollOperation { operation_id: OperationRef, after: DurationMicros },
    RefreshAuth { method: AuthMethodRef },
    UpdateClient { minimum_protocol: ProtocolRef, current_binding: BindingRef, command: CatalogSafeText },
    ResolveScope { candidates: Vec<SafeCandidate>, canonical_request_template: CanonicalRequestRef },
    Resubscribe { snapshot_request: CanonicalRequestRef, reason: ResubscribeReason },
}
```

`ApplicationResponse<T>` requires a sealed `TransportEligibleView` implemented only by generated structs whose content fields use plan 18's `CatalogSafeText`, `SearchEligibleText`, `PromptEligibleText`, `ExportEligibleText`, or explicit redacted/denied/unknown variants. Raw `String`, `serde_json::Value`, and bytes cannot satisfy it. This is a compile-time boundary plus a runtime sanitization receipt check, not a convention left to each use case.

### 7.2 Query and command separation

```rust
pub trait QueryUseCase<I, O>: Send + Sync {
    fn id(&self) -> UseCaseId;
    fn execute<'a>(
        &'a self,
        input: I,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<ApplicationResponse<O>, ApplicationError>>;
}

pub trait CommandUseCase<C, O>: Send + Sync {
    fn id(&self) -> UseCaseId;
    fn execute<'a>(
        &'a self,
        command: CommandEnvelopeV1<C>,
        context: &'a RequestContext,
    ) -> BoxFuture<'a, Result<CommandReceipt<O>, ApplicationError>>;
}
```

- Queries do not reserve idempotency keys, append audit mutations, update access counters, or apply policy effects. Optional view-access analytics are a separately submitted event after the read and never change the returned snapshot.
- Commands always have a canonical owner, idempotency key, expected version, authorization decision, audit schema, and catalog-owned `ExecutionModeV2`. Direct commits execute once; autonomous policy effects are not public item commands; resumable workflows return an operation; host lifecycle events remain internal. A destructive operation exposes a separately named typed preflight use case and a separately named confirmed domain command (for example `storage.consolidation.plan` then `storage.consolidation.start`) whose payload carries the preflight receipt/token. There is no universal `preview`, `apply`, `dry_run`, or `ApplyConfirmation` method.
- An operation-specific preflight captures aggregate/evidence versions, impact, redactions, disk/network/process effects, and a typed confirmation token. The confirmed domain command revalidates every version/capability/hold/freshness dependency; operations that do not need this safety boundary do not manufacture a preflight.
- Retrying an identical completed command returns the stored receipt. Reusing an idempotency key with a different canonical command digest returns `idempotency_conflict` without mutation.
- Adapters cannot invoke repository operations directly; the use-case registry is the only executable capability surface.

Query inputs embed one typed read envelope; adapters map it losslessly instead of inventing per-transport consistency semantics:

```rust
pub struct ReadRequirementsV1 {
    pub consistency: ReadConsistency,   // Eventual | Frozen | AtLeastWatermark(VectorWatermark)
    pub budget: ResourceBudget,         // row/byte/shard/time caps within catalog hard limits
    pub payload: RequestedPayloadPolicy,
}
```

This is plan 17 §11.1's per-request consistency/budget/payload contract. HTTP read POST bodies carry it as a top-level `read` object and GET enumerations accept only its bounded enum/watermark forms (plan 10 §8); the request deadline itself stays in `RequestContext`.

### 7.3 Authorization and privacy

```rust
pub trait AuthorizationPort: Send + Sync {
    fn authorize_query<'a>(
        &'a self,
        principal: &'a Principal,
        use_case: UseCaseId,
        requested: &'a RequestedAccess,
    ) -> BoxFuture<'a, Result<AuthorizedQueryAccess, AuthorizationError>>;

    fn authorize_command<'a>(
        &'a self,
        principal: &'a Principal,
        use_case: UseCaseId,
        requested: &'a RequestedEffect,
    ) -> BoxFuture<'a, Result<AuthorizedCommandAccess, AuthorizationError>>;
}
```

- Authorization happens before scope expansion, query planning, payload hydration, policy snapshot load, remote refresh, export staging, or command preview.
- Scope authorization yields profile/privacy-domain/sensitivity grants and an `access_digest` bound into cursors, subscriptions, previews, exports, and recorded evaluations.
- Locked stores return catalog-safe coverage only. Reasoning payload requires an explicit retained-artifact grant and remains excluded from search/export by default.
- Secret-like/quarantined content is never eligible for query text, policy fixtures, fact apply, search/vector projection, or export. Application cannot override that invariant.
- Cross-profile collections authorize each profile independently and return segregated coverage; content is never copied into the catalog to simplify joins.
- New content-bearing commands, model summaries, remote payloads, V1 compatibility rows, operator notes, and generated failure details enter as `Unclassified<T>` and must receive a complete `SanitizationReceiptV1` before preview/audit/persistence. Scanner timeout, incomplete parsing, unsupported encoding, or missing policy returns blocked/unknown coverage and persists only a non-content receipt.
- `privacy.status` derives configured policy, effective safety floor, adapter/source/sink/detector coverage, scanner versions, last verified scan, sanitized/quarantined/legacy-unscanned counts, and unknowns independently. The existence of a historical lossy row is never evidence that protection is enabled.

## 8. Unit of Work, Idempotency, Audit, and Workflow Contracts

### 8.1 Single-owner command transaction

```rust
pub trait UnitOfWorkFactory: Send + Sync {
    fn begin<'a>(
        &'a self,
        owner: ShardRef,
        command: &'a CommandIdentity,
    ) -> BoxFuture<'a, Result<Box<dyn UnitOfWork>, CommandStoreError>>;
}

pub trait UnitOfWork: Send {
    fn load_aggregate(&mut self, target: AggregateRef)
        -> Result<AggregateSnapshot, CommandStoreError>;
    fn reserve_idempotency(&mut self, reservation: IdempotencyReservation)
        -> Result<IdempotencyDisposition, CommandStoreError>;
    fn append(&mut self, event: CanonicalEventV1)
        -> Result<(), CommandStoreError>;
    fn append_relation(&mut self, relation: RelationAssertionV1)
        -> Result<(), CommandStoreError>;
    fn append_audit(&mut self, audit: AuditEnvelopeV1)
        -> Result<(), CommandStoreError>;
    fn append_outbox(&mut self, entry: OutboxEntryV1)
        -> Result<(), CommandStoreError>;
    fn complete_idempotency(&mut self, result: StoredCommandResult)
        -> Result<(), CommandStoreError>;
    fn commit(self: Box<Self>) -> Result<CommandCommitReceipt, CommandStoreError>;
}
```

Transaction order is fixed:

1. Authorize the requested effect and resolve canonical owner without opening a writer transaction.
2. Build preview from a frozen read snapshot; return impact, required approvals, and digest.
3. On apply, validate confirmation and deadline, then perform any safe external preflight outside the transaction.
4. Open one owning-shard unit of work and fence the writer lease.
5. Reserve idempotency; exact prior completion returns the prior receipt.
6. Load aggregate and compare expected/preview versions, holds, permissions, and policy/capability digests.
7. Append immutable canonical domain events/relations, audit event, outbox entries that reference their causing canonical event IDs, and stored command result atomically.
8. Commit and return `CommandReceipt` with resulting aggregate version and shard watermark.
9. Trigger asynchronous projections or a durable workflow after commit; never claim their completion in the command receipt until their own receipt exists.

No network, process launch, source scan, blob upload, large export encoding, model evaluation, or user wait occurs between steps 4 and 8.

The canonical event journal is authoritative. Current rows and specialized histories are transactionally maintained indexes; projectors, scheduler, replay, and subscriptions advance only from committed canonical event sequence/checkpoints. Outbox entries carry post-commit wakeup or external-effect delivery intent plus causing event IDs; notifier, adapter receipt, audit, SSE, or outbox delivery state cannot create task/domain truth or acknowledge a command that the journal did not commit.

Idempotency records are concrete contracts, not conventions; plan 02 stores them in the owning shard's `command_idempotency` table:

```rust
pub struct IdempotencyReservation {
    pub key: IdempotencyKey,              // caller-supplied, <=128 bytes
    pub principal: ActorRef,
    pub use_case: UseCaseId,
    pub command_digest: ContentDigest,    // canonical CommandEnvelopeV1 digest
    pub reserved_at: UtcMicros,
    pub retain_until: UtcMicros,
}

pub enum IdempotencyDisposition {
    Reserved,
    Completed(StoredCommandResult),
    ConflictingDigest { stored_digest: ContentDigest },
}

pub struct StoredCommandResult {
    pub key: IdempotencyKey,
    pub command_id: CommandId,
    pub receipt_digest: ContentDigest,
    pub receipt: BoundedReceiptBytes,     // canonical CommandReceipt encoding, <=256 KiB
    pub completed_at: UtcMicros,
    pub retain_until: UtcMicros,
}
```

- Key scope and uniqueness: the primary key is `(principal, use_case, key)` in the command's owning shard; the same key under a different principal or use case is a distinct reservation, never a conflict.
- Retention: completed results are retained at least 7 days (plan 20 configuration, per command class) and never shorter than the longest declared retry/operation-confirmation window; an index on `retain_until` drives GC. After expiry the key is forgotten and a retry executes as a new command; clients needing longer recovery follow the receipt's `OperationRef`.
- Size: a stored result larger than 256 KiB persists the receipt plus an `OperationRef` instead of inline output; identical retry returns that receipt with the operation pointer.

### 8.2 Command receipts and conflicts

```rust
pub struct OperationPreflightV1<P> {
    pub preflight_id: OperationPreflightId,
    pub confirmation_token: ProtectedConfirmationToken,
    pub operation_kind: UseCaseId,
    pub owner: ShardRef,
    pub based_on: VectorWatermark,
    pub aggregate_versions: BTreeMap<EntityRef, AggregateVersion>,
    pub impact: P,
    pub required_approvals: Vec<ApprovalRequirement>,
    pub confirmation_required: bool,
    pub expires_at: UtcMicros,
}

pub struct CommandReceipt<O> {
    pub command_id: CommandId,
    pub execution_mode: ExecutionModeV2,
    pub disposition: CommandDisposition,
    pub result: O,
    pub owner: ShardRef,
    pub aggregate_version: AggregateVersion,
    pub watermark: ShardWatermark,
    pub audit_event: EventId,
    pub operation: Option<OperationRef>,
    pub workflow: Option<WorkflowRef>,
}
```

`CommandId` is allocated deterministically by the application on first `execute` — a digest over principal, use case, idempotency key, and canonical command digest — so retry is stable and at most one ID exists per reservation; adapters never mint it. `OperationPreflightV1` exists only for catalog-declared confirmed destructive workflows, uses its own idempotency/expiry/authorization contract, and cannot be passed to an unrelated use case. An expired preflight returns `operation_preflight_expired` and requires the same named preflight use case again.

Version conflict returns the current version, changed dependency IDs, safe summary, and, only for a confirmed operation, a new-preflight requirement. It never auto-rebases a destructive command. Idempotent status/run/refresh requests may explicitly declare a merge policy; that policy is versioned in the catalog and fixture-tested.

### 8.3 Cross-shard workflows

```rust
pub struct WorkflowDefinition {
    pub kind: WorkflowKind,
    pub version: SemVer,
    pub steps: Vec<WorkflowStepSpec>,
}

pub struct WorkflowStepReceipt {
    pub workflow: WorkflowId,
    pub step: WorkflowStepId,
    pub attempt: u32,
    pub expected_versions: VersionVector,
    pub input_digest: ContentDigest,
    pub disposition: WorkflowStepDisposition,
    pub effect_receipts: Vec<EffectReceiptRef>,
    pub compensation: Option<CompensationRef>,
}
```

Cross-shard workflows cover retention/delete descendants, profile/project settings propagation, export publication, projection rebuild/publish, migration/backfill/cutover, autonomous managed-skill materialization/supersession/recovery, backup/restore, and remote refresh plus local reindex. They obey:

- Durable state is written before executing the next effect; retries use the same workflow/step idempotency key.
- Each step owns at most one shard transaction or one bounded external effect, never both simultaneously.
- Leases are fenced by epoch. A takeover cannot publish a stale step receipt.
- Compensation is declared only where it is safe and semantic; irreversible content deletion has recovery grace and explicit terminal state, not fictional rollback.
- Partial completion is visible in Observatory and returned by status queries. Other shards remain queryable.
- Cancellation stops before the next step. It cannot undo a committed canonical observation, audit event, or externally completed effect.
- Workflow terminal states are `Succeeded`, `Failed`, `Cancelled`, `CompensationRequired`, or `Blocked`; `Blocked` names the missing authority/version/capability.

## 9. Complete Read Use-Case Inventory

The tables below use compact operation slugs to stay readable. They are not a second ID grammar: `tracedecay-tool-catalog` supplies canonical `UseCaseId` (`usecase.<domain>.<verb-noun>`) mappings for current bindings; V1 alias mappings live only in the internal migration manifest. For example, `git.branches.list` maps to `usecase.git.list-branches`. Application code accepts only the generated typed ID and cannot construct it from a slug. Each read returns `ApplicationResponse<T>` and therefore cannot omit coverage/freshness/redaction/retention.

### 9.1 System, scope, capability, and operations

| Use-case ID | Input/output contract |
|---|---|
| `system.capabilities.get` | Current active implementations/bindings, catalog digest, prerequisites, disabled state, and transport mappings. Migration parity/old-name state is operator-only and never enters current help/hints/catalog. |
| `system.scopes.list` / `system.scopes.resolve` | Lazy All/repository/project/worktree/ref/snapshot tree plus exact-name/path/alias resolution, parent/depth/search/changed-since, same-name labels, ambiguity candidates, one-step retry token, provenance, health, and watermark. |
| `system.projects.list` / `system.projects.search` / `system.project.get` | Registered projects and exact identity/adoption/alias/health evidence; no unbounded store opening. |
| `system.health.get` / `system.doctor.get` | Store, daemon, watcher, provider, index, migration, privacy, payload and capability health with exact runtime/store identity. |
| `system.coverage.get` | Domain/shard/source/projection coverage and gaps at a vector watermark. |
| `system.migrations.list` / `system.migration.get` | Import/backfill/cutover/rollback receipts, counts, hashes, quarantine, status. |
| `system.projections.list` / `system.projection.get` | Projector versions, input/output watermarks, lag, dead letters, generations. |
| `privacy.status.get` / `privacy.scans.list/get` / `privacy.findings.list/get` | Effective safety floor/policy, source/sink/detector coverage and versions, last verified scan, safe finding classes/states, sanitized/quarantined/legacy-unscanned/unknown counts, and restore eligibility; never candidate content. |
| `privacy.detectors.list` / `privacy.detectors.diff` / `privacy.remediations.get` / `privacy.quarantine.status` | Detector metadata, synthetic-only comparison, descendant/rebuild/rotation state, and elevated quarantine metadata without plaintext. |
| `system.daemon.status` / `system.watchers.list` / `system.index.status` | Operational status and freshness only; lifecycle changes are commands. |
| `settings.effective.get` / `settings.sources.list` | Effective profile/project/integration/automation/storage settings, declared owner, source layer, default, validation, restart/reindex/privacy impact; environment is an immutable source, not a writable target. |
| `operations.list` / `operations.get` | Durable command/job/workflow/export/migration/automation progress, effect receipts, audit ref, retry/cancel capability, blocked reason, and explicit terminal disposition. |
| `research.anchor.resolve` / `research.recipe.execute` | Resolve a stable session/thread/Turn/message/agent/subagent/workflow/goal/Git evidence anchor or re-execute its versioned retrieval recipe with drift/coverage; never depend on an ephemeral response handle. |

Doctor and provider state are typed evidence, not branding strings:

```rust
pub struct DoctorFindingView {
    pub severity: FindingSeverity,
    pub observed_owner: ObservedOwner,
    pub remediation_authority: RemediationAuthority,
    pub evidence: Vec<EvidenceRef>,
    pub legal_actions: Vec<UseCaseId>,
    pub diagnostic: DiagnosticEnvelopeV1,
}

pub enum ProviderIntegrationState {
    Detected,
    Installed,
    Configured,
    Healthy,
    Degraded,
    Partial,
    Unsupported,
    ForeignOwned,
}
```

`Info + ForeignOwned + None` cannot become an update nag or actionable repair. Provider names/logos do not imply `Healthy`: each binding reports observed hooks/tools/session coverage, missing pieces, last verified time, and exact repair authority.

Doctor, privacy, code, task/executor, migration, storage, provider, and remediation findings use the one domain `DiagnosticEnvelopeV1` defined by plan 01 and governed by plan 24 §4.11. Application revalidates the envelope's subject/version/scope/catalog/config/evidence and recomputes `legal_actions` at read/command time. It never converts diagnostic prose into a command. Unknown action kinds remain disabled evidence; a stale/expired envelope cannot authorize an action. The specialized view fields above are projections for filtering, not a competing diagnostics schema.

Scope resolution uses domain `ScopeSelectorV2`, `ScopeRootV2`/`ScopeTargetV2`, `ScopeResolutionV2`, and its candidate/retry types unchanged. The exact selector fields are `version`, nonempty `roots`, `exclude`, `time`, `activity_attribution`, `coverage`, `freshness`, `traversal`, `ambiguity`, and `limits`; locators are `ScopeTargetV2::Locator(ScopeLocatorV2)` and canonical IDs are `ScopeTargetV2::Canonical(EntityRef)`. Application adds authorization, request preservation, and use-case validation; it does not define `ScopeExpr`, a transport selector, or another resolution enum.

- Every application request contains a valid `ScopeSelectorV2`. A generated binding may declare a convenience default by inserting an explicit root before invocation: Brain/Observatory use `AllAuthorized { profile_id }`; code-local bindings may use `CurrentInvocation`. The shared application resolver converts locators/current invocation to a canonical selector and returns `ScopeResolutionV2`, including `defaulted_current`; no cwd, last project, route, selected row, or host heuristic overrides any explicit root.
- Repository, project, checkout/worktree, ref, commit/snapshot, and explicit multi-selection are distinct scope kinds. A project filter never becomes durable ownership.
- Every candidate includes opaque IDs, kind, profile, disambiguated `owner/repository/project/worktree/ref` label, path/remotes only when authorized, alias/adoption evidence, index generation, freshness, and partial/unavailable state.
- Same-name repositories/projects/branches are never merged by label. Ambiguity returns bounded candidates and a signed token; selecting one candidate retries the original canonical request in one step without retyping query/filter/time state.
- CLI, MCP, HTTP, dashboard, exports, and saved recipes use the same generated scope request/result and candidate token. Transport display may differ; resolved IDs, candidates/order, provenance, errors, coverage, and retry semantics may not.

### 9.2 Universal query, Brain, graphs, and timeline

| Use-case ID | Input/output contract |
|---|---|
| `query.execute` | Authorized `TraceQueryV1` to typed rows/edges/facets/aggregates/cursor/explain/coverage. |
| `search.universal` | Evaluated lexical/exact-phrase/fuzzy/entity/semantic/graph/recency hybrid with explicit origin/kind filters, grouping/dedupe, caps, candidate/rank explanation, and profile/corpus version; embeddings are one optional feature, never presumed beneficial. |
| `representations.artifacts.list/get/status` / `representations.generations.list` | Signed catalog versus local bytes/verification/activation/revocation, license/runtime/resource envelope, leases/pins/cache pressure, affected index generations, cold/warm status, and typed unavailable/fallback coverage from plan 05 §11.2A; never model input/vector values or raw cache paths. |
| `search.benchmark.evaluate` | Read-only execution of one or two ranking profiles over the versioned redacted benchmark corpus with per-slice quality/latency/candidate/coverage deltas and promotion blockers. |
| `entities.batch_get` | Bounded inspector hydration for canonical IDs, evidence, provenance, authorized payload slices. |
| `brain.overview.get` | First-scan claim, focal clusters, aligned activity summary, health strip, feedback loop, unfinished work, source watermarks. |
| `brain.lens.get` | One bounded Git/code/thread/agent/turn/timeline/memory/automation-skill graph lens with legal node/edge schema and LOD. |
| `brain.cluster.expand` | Stable aggregate cluster membership/counts, child cursor, denominator, sampling, algorithm/layout version. |
| `graph.neighborhood.get` / `graph.path.get` / `graph.subgraph.get` / `graph.diff.get` | Bounded evidence-filtered neighborhood/path/query-driven subgraph/frozen comparison with confidence, redacted frontier, stable ordering, exact snapshot identities, and legal relation schemas. |
| `graph.impact.get` / `graph.affected_tests.get` | Direct versus inferred impact with algorithm/evidence and source snapshot. |
| `timeline.density.get` / `timeline.events.get` | Bounded buckets or event lanes with hidden/late counts, half-open interval, LOD and cursor. |
| `timeline.as_of.get` | Known state at valid and observed time: scope, context, facts, policies, catalog, goals, delivery, coverage. |
| `timeline.follow_agent` / `timeline.compare` | Stable agent/subagent lanes or aligned sessions/agents/branches/models/policies/time ranges with anchors. |
| `activity.events.get` / `activity.facets.get` | Consequential cross-domain activity and project/domain/actor/kind/health facets over the same event/timeline model, with routine-noise hidden counts and live/frozen coverage; no UI-side merge. |
| `coordination.presence.get` / `coordination.nearby.get` / `coordination.overlaps.get` | Expiring evidence-bearing presence/work claims and nearby-agent overlap across the same or parallel worktrees, refs, files, symbols, tests, goals, and review/delivery surfaces; includes safe compact summary plus research anchors/recipes. |

Brain, graph, timeline, search, impact, and Explorer accept domain `ScopeSelectorV2` unchanged and return `ScopeResolutionV2`. Every node/edge/row retains repository and snapshot identity; same-name symbols/files/refs never collapse without canonical entity lineage. Cross-repository edges require registered dependency/session/workflow/Git/evidence relations, and each shard contributes explicit provenance/freshness/coverage rather than a synthetic global timestamp.

Graph-of-graphs selection is one application composition:

```rust
pub struct InvestigationSelection {
    pub scope: ScopeSelectorV2,
    pub time: InvestigationTime,
    pub selected: Vec<EntityRef>,
    pub pinned: Vec<EntityRef>,
    pub lens: GraphLensKind,
    pub snapshot: SnapshotMode,
    pub lod: LevelOfDetail,
}

pub enum GraphLensKind { Git, Code, Thread, Agent, Turn, Timeline, Memory, AutomationSkill }

pub struct GraphLensResponse {
    pub schema: LensSchema,
    pub nodes: Vec<LensNode>,
    pub edges: Vec<LensEdge>,
    pub aggregates: Vec<LensAggregate>,
    pub inspector_refs: Vec<EntityRef>,
    pub timeline_refs: Vec<EventId>,
    pub expansion: Option<OpaqueCursor>,
}
```

Stable investigation handoff exposes domain `RetrievalAnchorId`; the owning store resolves it to domain `RetrievalAnchorRecordV1` under current authorization. Plan 13's research bundle/context manifest cites those IDs. Application consumes plan 01's portable multi-anchor `RetrievalRecipeV1` unchanged — recipe ID, owning use case, anchor list, optional protected input ref, privacy-domain-bound canonical input digest, scope selector, investigation time, optional message view, schema/catalog/ranking version set, and freshness requirement — and defines neither a second recipe type nor a second anchor record.

Every session/thread/Turn/message/agent/subagent/workflow/goal/Git result exposes at least one `RetrievalAnchorId` and a safe recipe or protected recipe ref. Research bundles use `ResearchContextAnchorV1` only for implementation provenance; it is not a parallel result-citation model. Recipes contain no literal prompt/query/path secret, cursor, response-handle token, or remote credential. Resolution loads `RetrievalAnchorRecordV1` and returns current identity, source evidence, drift from recorded versions/watermarks, and coverage. Cursors remain page mechanics; V1 response handles may bridge a migration renderer but are never the sole research locator or saved/exported reference.

Agent proximity is a claim/evidence model, not global truth:

```rust
pub struct AgentPresenceClaimV1 {
    pub agent: EntityRef,
    pub host_provider: HostProviderRef,
    pub workflow_goal_turn: Vec<EntityRef>,
    pub repository: Option<RepositoryId>,
    pub worktree: Option<WorktreeId>,
    pub revision: Option<CommitId>,
    pub work_claims: Vec<WorkClaimRef>,
    pub observed_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub source: EvidenceRef,
    pub confidence: Confidence,
}

pub struct CoordinationOverlapView {
    pub agents: Vec<EntityRef>,
    pub overlap: Vec<OverlapEvidence>,
    pub proximity: ProximityClass,
    pub safe_summary: SafeCoordinationSummary,
    pub anchors: Vec<RetrievalAnchorId>,
    pub retrieval: RetrievalRecipeV1,
    pub actions: Vec<UseCaseId>,
}
```

- `ProximityClass` distinguishes same worktree, parallel worktree/same repository, overlapping branch/ref, direct file/symbol/test/goal/review overlap, and weak temporal proximity. Temporal proximity alone is never a conflict claim.
- Presence expires; missing/expired claims mean unknown, not absent. Safe summaries are bounded, secret-scanned, provenance-bearing, and contain no raw prompts, tool arguments, payloads, sensitive paths, or inferred chain of thought.
- Overlap actions are exactly `inspect`, `message`, `handoff`, `ack`, and `suppress`. Inspect is a read. The others are direct or resumable typed commands with target capability, authority, idempotency, delivery receipt, expiry, and audit; failure to deliver never becomes an acknowledgement.
- Policy may select at most one dynamic coordination hint per eligible overlap horizon. It requires material overlap, ranks an actionable target, includes one stable anchor/recipe, and applies per-agent/pair/work-claim dedupe, cooldown, acknowledgement, suppression, and terminal-outcome attribution. Repeated hook prompts cannot spam the same unresolved overlap.
- Coordination analytics distinguish eligible, material, selected, delivered, inspected, messaged, handed off, acknowledged, suppressed, expired, resolved, duplicate-prevented, and unresolved horizons with coverage/denominators.

Universal search quality is corpus-evaluated:

- Retrieval stages are separately observable: exact token/field, exact phrase, typo-tolerant fuzzy, entity/alias, semantic/vector, graph-neighborhood, and recency/activity. Each stage declares tokenizer/model/index/version, candidates, caps, exclusions, and latency.
- A versioned ranking profile combines only available features and explains missing ones. Semantic/vector contribution is enabled only when labeled benchmark evidence improves the declared task slice without unacceptable precision, privacy, latency, or memory regression.
- Origin/kind/provider/session/agent/project/ref/time/sensitivity filters execute before or during candidate generation where legal. Representative grouping/dedupe preserves native membership/expansion and cannot erase a better exact match.
- The benchmark corpus covers exact literals, phrases, misspellings, symbols/entities/aliases, direct-user versus delegated/protocol ambiguity, cross-project concepts, graph-related evidence, recency, no-result, capped, adversarial-noise, and embedding-regression cases. A named cross-repo slice spans Rspack, Rsbuild, and React Router work/benchmarks with same-name files/symbols/branches and known dependency/session/PR evidence. Report MRR/nDCG/Recall@k/Precision@k, zero-result rate, latency, candidate counts, coverage, and per-slice regressions; no aggregate score hides a failing exact-match or repository-disambiguation slice.

Selecting an entity can request another lens using the same `InvestigationSelection` and frozen watermark. Application does not render or position nodes. It guarantees legal lens schema, evidence-bearing cross-links, stable selection identity, bounded expansion, and table/outline projection fields.

### 9.3 Sessions, messages, turns, agents, and workflows

| Use-case ID | Input/output contract |
|---|---|
| `sessions.list` / `sessions.get` | Cursor enumeration without text predicate; provider/host/actor/project/time/goal/workflow filters, participants, coverage, snapshots. |
| `messages.list` / `messages.search` | Cursor enumeration/search with role/kind/provider/time plus domain `MessageOrigin`/`MessageView` filters defined below. |
| `messages.get` / `messages.expand_native` | One canonical row or representative with exact source observations and bounded sanitized-native expansion. |
| `turns.get` / `turns.list` | First-class Turn intervals linking visible context, messages, reasoning artifacts, tools, goals, code/Git/effects and end state. |
| `sessions.replay` | Read-only historical assembly with exact/recorded/best-effort availability and missing-input declarations. |
| `sessions.context_lineage` | LCM sanitized-native/source/summary DAG, compression decisions, payload coverage and source ranges. |
| `agents.list` / `agents.get` | Actor/instance identity, provider-native aliases, lifecycle, parent/child, goals, handoffs, usage and outcomes. |
| `goals.list` / `goals.get` | First-class Codex goals and provider-native objectives with owner agent/session/workflow, versioned status/plan updates, Turns, evidence, terminal state, and coverage. |
| `workflows.list` / `workflows.get` | Claude workflows, Codex goals, TraceDecay automations, Hermes-style curation agents with native semantics and shared relations. |

Application does not define a second message-filter vocabulary. It consumes domain `MessageOrigin::{DirectUser,DelegatedAgentPrompt,ToolResultProtocol,ProviderProtocol,Unknown}` and `MessageView::{NativeRows,RepresentativeRows,HumanBestEffort,DirectUser,DelegatedAgents,ToolResults,ProviderProtocol}` unchanged. Its output row is named distinctly from the domain query enum:

```rust
pub enum MessageRowKind { NativeRow, RepresentativeRow }

pub struct MessageReadModel {
    pub entity: EntityRef,
    pub origin: MessageOrigin,
    pub row_kind: MessageRowKind,
    pub representative_for: Vec<EntityRef>,
    pub representative_rule: Option<AlgorithmRef>,
    pub suppressed_duplicate_count: u64,
    pub source_observations: Vec<ObservationId>,
    pub raw_expansion: Option<OpaqueCursor>,
    pub content: SanitizedContentView,
}
```

`SanitizedContentView` is a generated tagged availability view—`Available(SanitizedPayload)`, `Redacted`, `Denied`, `Unavailable`, or `Unknown` with safe reason/receipt refs. It is not an independent sanitizer or raw-text wrapper.

`NativeRows` is the complete canonical enumeration of sanitized rows and is lossless for retained non-secret structure/semantics. `RepresentativeRows` is a query projection that preserves every represented row ID, observation, rule/version, suppression count, and expansion cursor. A client that needs both follows the representative expansion cursor or issues a second `NativeRows` request at the same frozen snapshot; no ambiguous combined count exists. Direct-user, delegated-agent, tool-result, and provider-protocol views are independent of provider `role=user`; unknown origin remains visible in `NativeRows`, `RepresentativeRows`, and `HumanBestEffort` coverage.

### 9.4 Code, Git, delivery, knowledge, automation, and accounting

| Family | Stable read use cases |
|---|---|
| Code discovery | `code.search_symbols`, `code.find_exact_symbol`, `code.grep`, `code.context`, `code.files`, `code.callers`, `code.callees`, `code.call_path`, `code.impact`, `code.affected_tests`, `code.test_map`, `code.health`, `code.diagnostics`, `code.diagnose_result`. |
| Git semantic tools | `git.branches.list`, `git.branches.search`, `git.branches.diff`, `git.pr.context`, `git.changelog`, `git.commit.context`, `git.sessions_for`, `git.workflows_for`. Each result states local indexed ref/merge-base/generation/watermark and fallback state. |
| Delivery truth | `delivery.repositories.get`, `delivery.pull.get`, `delivery.checks.list`, `delivery.reviews.list`, `delivery.releases.list`, `delivery.reconcile`. Live state carries provider/fetched-at/ETag/base/head/cap/coverage separately from local semantic state. |
| Knowledge | `knowledge.facts.list/get/search`, `knowledge.entities.list/get`, `knowledge.trust.history`, `knowledge.conflicts.list`, `knowledge.retrieval.history`, `knowledge.feedback.history`, `knowledge.deletion_impact`. |
| Automation | `automation.jobs.list/get`, `automation.scheduler.status`, `automation.runs.list/get`, `automation.artifacts.get`, `automation.proposals.list/get`, `automation.skills.list/get`, `automation.outcomes.get`, `automation.workflow_graph.get`. |
| Tasks/orchestration | `initiatives.list/get`, `initiatives.graph`, `plans.list/get/diff`, `work_items.list/get/query`, `work_items.context`, `work_items.dependencies`, `attempts.list/get/timeline`, `executors.list/get/match`, `scheduler.status/explain`, `task_views.list/get`. Plan 24 §9.1 owns semantics and module files: reads use read ports only, `executors.match` is read-only, and task events are subscription read-model kinds under `subscriptions.create`, never a second stream vocabulary. |
| Hints and policy | `hints.evaluations.list/get`, `hints.outcomes.get`, `hints.opportunities.get`, `policy.bundles.list/get`, `policy.coverage.get`. |
| Accounting | `accounting.usage.get`, `accounting.costs.get`, `accounting.savings.get`, `accounting.adoption.get`, `accounting.denominators.get`. Unknown/capped denominators are typed, never zero. |

Current Git tools are not hidden behind the generic query alone: their stable use cases remain catalog aliases so hint routing can recommend them. `delivery.reconcile` refuses to combine semantic impact with live PR/check claims when head/base/merge-base/changed-file digest drift; it returns `RefreshLive`, `ReindexLocal`, or `RecomputeBoth` as an explicit next action.

### 9.5 Saved investigations, exports, subscriptions, and labs

| Use-case ID | Contract |
|---|---|
| `saved_views.list/get`, `collections.list/get`, `annotations.list/get` | Authorized profile content storage; sensitive literals never enter catalog or URL-safe summaries. |
| `exports.get`, `exports.list` | Job state, frozen watermark, parts, hashes, counts, redaction, completeness, expiry; bytes served by API after authorization. |
| `subscriptions.create` | Authorize a query/read-model request, capture snapshot, return opaque subscription ID and finite replay contract. |
| `labs.hints.evaluate`, `labs.retrieval.evaluate`, `labs.ingest.evaluate`, `labs.query.evaluate`, `labs.correlation.evaluate`, `labs.scheduler.evaluate`, `labs.memory.evaluate`, `labs.policy_diff.evaluate` | Immutable exact/recorded/best-effort input and typed explanation/diff with no writes. |
| `labs.search_quality.evaluate/compare` / `labs.scope_federation.evaluate` / `labs.privacy.evaluate` | Read-only retrieval-profile/corpus comparison, selector-resolution/shard-plan replay, and reserved/invalid synthetic sanitizer evaluation with exact anchors, versions, coverage, resource costs, and zero live registry/qrel/finding/policy mutation. |
| `labs.coordination.evaluate` | Replay presence/overlap classification, proximity ranking, one-hint selection/suppression/dedupe, safe summary, legal action set, and outcome attribution with exact versions/coverage; message/handoff/ack/suppress are simulated only. |
| `labs.orchestration.replay` | Read-only replay of plan 24 scheduler/executor decisions — readiness evaluation, route resolution, lease-acquisition fencing, and context-packet assembly — against recorded task-graph state with exact versions/coverage; work claims, leases, attempts, and views are never mutated. |
| `labs.evolution.inspect` / `labs.evolution.simulate` | Evidence collection through curator/reflector/skill-writer graph, proposal/version diff, validation, historical corpus simulation, rollout/rollback prediction. |

Evolution Studio preserves Hermes-style self-improvement as ordinary evidence-bearing actors, goals, turns, tools, artifacts, skills, memories, autonomy decisions, automatic applies, uses, outcomes, revisions, automatic recoveries, archives, and deletions. Simulation is an inspector and never mutates live state; it returns changed decisions/tool routes/outcomes, regressions/wins only where labels exist, unknown horizons, privacy exclusions, and cost/latency deltas. The live autonomous worker does not wait for the inspector.

## 10. Complete Command Use-Case Inventory

Every non-curation mutation has an explicit typed execution contract; destructive/irreversible operations use a separately named preflight and confirmed domain command when required. Curation is deliberately different: it is fully autonomous under versioned configuration and emits no per-item preview/approve/apply/rollback commands. The catalog marks autonomy, authorization, expected versions, side effects, monitoring/recovery behavior, job behavior, and audit.

| Domain | Stable command use cases |
|---|---|
| Projects/indexing | `projects.register`, `projects.update_alias`, `projects.unenroll`, `index.refresh`, `index.pause`, `index.resume`, `watchers.start`, `watchers.stop`. Unenroll previews retained evidence and never deletes content implicitly. |
| Runtime/daemon/update | `daemon.start`, `daemon.stop`, `daemon.drain`, `daemon.restart`, `runtime.update.plan`, `runtime.update.start`, `runtime.update.recover`. Drain/update are durable workflows carrying lifecycle lease epoch, accepting/draining/stopped state, in-flight work, checkpoint/receipt, restart requirement, takeover, recovery artifact, and current client-binding version. |
| Diagnostics/repair | `diagnostics.refresh`, `doctor.run`, `repair.plan`, `repair.apply`, `backup.create`, `backup.restore`. Restore is a durable workflow with preflight and rollback point. |
| Store administration | `storage.consolidation.inspect`, `storage.consolidation.plan`, `storage.consolidation.start`, `storage.consolidation.status`, `storage.consolidation.resume`, `storage.consolidation.recover`. This is the operator-only merged-#425 workflow for two nonempty profile shards: fail closed on unsupported/path-or-file-identity holders, freeze/reserve both sources, back up, stage, verify every table/artifact/disposition, cut markers atomically, and return exact recovery. `start` requires the recomputed deterministic confirmation token and administrative grant. It never runs from scheduler, task execution, Settings auto-save, or autonomous curation. V1 `preview/apply` names remain only in the compatibility adapter/inventory. |
| Capture/LCM | `capture.ingest`, `capture.pause`, `capture.resume`, `lcm.compress.plan`, `lcm.compress.start`, `lcm.boundary.create`, `lcm.lifecycle.preflight`, `lcm.lifecycle.repair`. Source offsets advance only through capture/store receipts. |
| Automations | `automation.jobs.create/update/delete`, `automation.run`, `automation.cancel`, `automation.pause`, `automation.resume`, `automation.scheduler.enable/disable`. Run revalidates policy/config/activity/lease before fenced acquisition. |
| Autonomous curation | `curation.run_now`, `curation.pause`, `curation.resume`, `curation.status`, `curation.history`, `curation.pin`, `curation.protect`, `curation.exclude`, `facts.feedback`. Candidate create/update/supersede/archive/quarantine and owned skill validate/materialize/revise/recover are internal autonomous effects, not public per-item commands. Each records artifact/evidence/validation/config/policy/expected-version/staged-monitoring/outcome receipts; foreign-owned targets are skipped. Explicit administrative deletion remains the separate descendant/hold/index/blob workflow. |
| Policy | `policy.publish`, `policy.activate`, `policy.rollback`. Exact artifact validation and immutable registry CAS are required; activation never changes an in-flight evaluation. |
| Representation artifacts | `representations.artifacts.install/import/activate/deactivate/evict/verify`, `representations.generations.rebuild`. Plan 05 §11.2A/PR 14E owns lifecycle semantics. Commands pin signed manifest/digest/license/runtime/config, enforce allowlisted egress and disk/RAM/device budgets, stage/verify before publish, preserve active/replay/index pins, and emit operation/audit receipts; query execution never invokes them. |
| Settings | `settings.profile.patch`, `settings.project.patch`, `settings.integration.patch`, `settings.automation.patch`, `settings.storage.patch`. Preview shows declared owner, source/default, restart/reindex/privacy/migration impact; environment-derived values are read-only and storage relocation is a durable workflow, never an arbitrary path write. |
| Payload/privacy | `payloads.gc.plan`, `payloads.gc.start`, `retention.run.plan`, `retention.run.start`, `holds.create`, `holds.release`, `entities.retire.plan`, `entities.retire.start`, `privacy.scan.start/cancel`, `privacy.remediation.plan/start/verify`, `privacy.quarantine.hold/release`. Privacy commands use safe finding/scan IDs, elevated grants where required, durable jobs, and candidate-free audit receipts. |
| Projections/migration | `projections.rebuild`, `projections.pause`, `projections.resume`, `projections.publish`, `projections.rollback`, `migrations.backfill`, `migrations.reconcile`, `migrations.cutover`, `migrations.rollback`. |
| Delivery refresh | `delivery.refresh`. Read-only remote fetch into captured evidence; repository allowlist, credential capability, rate/cap state, and fetched revision are audited. No PR write command. |
| Saved investigations | `saved_views.create/update/delete`, `saved_views.share.plan/start/revoke`, `collections.create/update/delete`, `annotations.create/update/delete`. Protected content stays with its declared activity/project owner; sharing creates a separately authorized, redacted, expiring local published view readable through `saved_views.get`, never publishes remotely, and never copies source content into catalog metadata. |
| Agent coordination | `coordination.message`, `coordination.handoff`, `coordination.ack`, `coordination.suppress`. Every command targets one presence/overlap claim and stable anchor, checks host/agent capability and expiry, previews disclosed summary/effects, records delivery/acceptance separately, and cannot mutate another agent's state without an authorized provider action. |
| Tasks/orchestration | `initiatives.create/update/pause/resume/retire`, `plans.create_version/activate`, `plans.decompose`, `work_items.create/update/replace/retire`, `work_items.link/unlink`, `work_items.assign/reassign/assign_set`, `work_items.pause/resume/cancel/reopen/archive`, `work_items.acquire_lease/heartbeat/progress/complete/block`, `work_items.retry`, `executors.register/heartbeat/drain/unregister`, `scheduler.pause/resume/run_once`, `task_views.create/update/delete/share/revoke`. Plan 24 §9.2 owns semantics and module files; every one is a POST command-envelope use case (plan 10 §8.7) — the former `PATCH /initiatives/{id}` / `PATCH /work-items/{id}` transport shapes are the `*.update` commands. `work_items.assign_set` is one bounded all-or-none owner-shard transaction with plan/item expected versions and per-item receipts; `work_items.acquire_lease` CAS-checks `expected_readiness_digest` and creates the sealed packet/attempt/lease set atomically. It never creates advisory `WorkClaimV1`. Task-view commands preserve the complete protected `TraceQueryV1` (including its scope), projection/group/layout, snapshot/version/watermark, sharing, and revocation contract without copied rows or a second selector. |
| API tokens | `auth.tokens.create`, `auth.tokens.list`, `auth.tokens.revoke`. Audited commands minting, listing, and revoking the scoped/TTL/revocable tokens of plan 17 §18.2; creation returns the secret exactly once through the secure flow, storage keeps only the hash and token ID, revocation declares stream/operation implications, and the per-launch bootstrap bearer (plan 10 §10.2) may execute only `auth.tokens.create` for the initial admin-class token. |
| Exports | `exports.create`, `exports.cancel`, `exports.delete`. Create freezes query/access/redaction, stages parts under profile export root, and publishes only after final manifest hash. |
| Lab promotion | `labs.fixtures.promote`. Requires sanitized redacted payload, secret scan receipt, exact source manifest, explicit confirmation, and repository-write capability outside lab runtime. |
| Code edits | `code.move_symbol.inspect/commit`. Inspect returns exact source-removal/destination-insertion diff, destination imports, caller/dependency/visibility/collision/module/cycle/orphaned-import/cfg impact, snapshot/version, and affected-test evidence without writing. Commit requires repository/worktree grant, typed confirmation, clean revalidation, destination-first write plus source recovery receipt, reindex operation, and never rewrites callers implicitly. |

V1 writable dashboard actions not represented by a row above block V1 retirement. PR 3's generated inventory and the application registry jointly enforce this: each mutation has exactly one V2 use case or an explicit retired-with-replacement decision.

Scope-sensitive command rules are exhaustive and fixture-locked:

- Create/import/propose operations for facts, skills, policies, automations, saved investigations, and annotations require explicit `DeclaredScope`; there is no “current project” fallback.
- Updates, approvals, applies, rollbacks, archives, restores, and deletes resolve the canonical owner from the target entity and reject a conflicting request scope before preview.
- Cross-project reuse creates evidence relations from the original owner. It never copies a profile fact/skill/policy into a project shard or promotes project state to profile scope implicitly.
- All-scope reads may combine profile-owned and project-owned rows, but each result and command capability retains `owner`, `declared_scope`, privacy domain, and authorization state.
- Moving ownership is a named migration workflow with source/target versions, conflict checks, copy/delete receipts, rollback boundary, and no in-place owner-field edit.

## 11. Orchestration Rules for Key Product Flows

### 11.1 Brain and graph-of-graphs

`brain.overview.get` captures one authorized scope and vector watermark, then requests bounded rollups, health, active-workflow summaries, feedback outcomes, and a focal lens. Components may finish partially; the response retains component coverage rather than failing the whole Brain. It does not open every project shard: catalog statistics and All rollups select candidate shards, and expansion is explicit.

`brain.lens.get` calls query graph/time operators with a lens schema from projectors/tool catalog, then batch-hydrates inspector references. Cross-lens links carry `RelationAssertionV1`, evidence class, confidence, producer/version, supporting events/observations, and validity. Temporal adjacency alone is never exposed as causation.

### 11.2 Session/agent investigation

One Causal Loom request composes density, lane events, Turn hubs, agent tree, tool results, code/Git/delivery evidence, knowledge/policy/automation links, and impact ribbon at the same frozen watermark. Missing project/Git/reasoning data creates lane coverage markers. Follow-agent retains collaborator and delivery context through bounded relation traversal; it does not filter away parent/subagent causation anchors.

### 11.3 Hint evaluation and injection

This flow implements the `HookApplicationPort` fixed by `07-hooks-crate.md`; hooks normalize/render/acknowledge, capture owns spool/fsync/journal durability, and this application composition owns the pinned evaluation.

1. Authorize host/session/project snapshot access and load immutable tool-catalog, policy, memory, skill, prior-state, and Git evidence refs.
2. Execute policy with explicit effective time, budget, and vector watermark.
   Coordination candidate facts contain only unexpired evidence-bearing overlap claims and safe summaries; policy may return at most one coordination hint after pair/work-claim dedupe, cooldown, acknowledgement, and suppression.
3. If live hook mode, transactionally record evaluation and accepted hint-state proposal in the activity owner before returning payload when deadline permits.
4. Hook adapter renders the returned bytes and reports delivery success/failure as a new event. Application never claims emitted/adopted before that evidence.
5. Outcome projector/application records terminal observed/unobserved/unresolvable, missed capability, and human correction with evidence and correct denominator.

### 11.4 Remote Git reconciliation

Live refresh is a command because it performs network I/O and appends new evidence, although it cannot mutate GitHub. Local semantic queries remain reads. `delivery.reconcile` joins them only after confirming repository, base/head, merge base, changed-file digest/cap, fetched-at, and local generation. Drift returns both alternatives and an action; application does not silently prefer GitHub or TraceDecay.

### 11.5 Export

`exports.create` authorizes requested fields/payload/sensitivity, captures a frozen query/access/redaction snapshot, creates a durable job, and returns immediately. A worker streams query frames into a contained staging sink, checks limits/hashes, writes final manifest, fsyncs, and atomically publishes. Failure/cancel leaves no completed manifest or downloadable partial. Export status preserves searched/skipped/stale/unavailable/incompatible/locked/redacted coverage and reasoning exclusions.

### 11.6 Evolution Studio and autonomous curation boundary

Inspection/simulation are lab reads and never gates live progress. The application curation worker consumes policy decisions continuously, revalidates exact candidate/version/evidence/validation/config/privacy/ownership state transactionally, and autonomously creates/updates/supersedes/archives/quarantines/materializes eligible owned facts, memories, and skills. It monitors staged outcomes and automatically revises/recovers when thresholds fire. No `approve`, `reject`, `preview`, `apply`, or user-triggered `rollback` command exists for a curation item; operators configure policy, inspect history, pause/resume/run-now, pin/protect/exclude, or submit feedback.

## 12. Internal Parity and Bounded Migration

### 12.1 Use-case parity receipt

```rust
pub struct UseCaseParityReceipt {
    pub use_case: UseCaseId,
    pub v1_inventory_item: CompatibilityItemId,
    pub corpus: ManifestId,
    pub v1_version: ComponentVersion,
    pub v2_version: ComponentVersion,
    pub source_watermarks: VectorWatermark,
    pub inclusion_digest: ContentDigest,
    pub ordering_digest: Option<ContentDigest>,
    pub mutation_effect_digest: Option<ContentDigest>,
    pub explained_differences: Vec<ParityDifference>,
    pub status: ParityStatus,
}
```

- Reads compare entities/rows/order/facets/coverage/watermarks/errors/caps and payload provenance before renderer formatting.
- Commands compare preview, validation, durable domain effects, audit, idempotent retry, version conflict, side effects, and rollback behavior; never run a destructive parity command against live user data.
- #410 representative query behavior is a named internal V1 parity profile, never a post-cutover live mode. Native rows are the completeness authority; representative output differences require rule/version/source evidence.
- #405 adoption and #407 profile consolidation fixtures assert no duplicate scope/project/session/fact exposure and preserve migration provenance.
- Migration-only shadow dispatch is selected by versioned feature state per use case, not one global flag, and is unreachable after its cutover receipt closes. A V2 cursor/preview/subscription is never interpreted by V1.

### 12.2 Cutover order

1. Register every use case and generate catalog/schema fixtures with no executable V2 default.
2. Land read-only system/query/session vertical slice, including sanitized-native/representative messages and partial coverage.
3. Shadow read use cases and compare typed semantic results.
4. Land command kernel and no-op fixture commands; prove idempotency/version/audit/workflow recovery.
5. Move domains independently: sessions, graph/code, Git/delivery, knowledge, policy/hints, automation/skills, accounting/operations, saved/export/labs.
6. For each domain, record freeze watermark, parity receipt, active implementation, rollback procedure, and monitoring gate.
7. Default transports to V2 only after all exposed use cases are parity-proven; atomically disable old live bindings/names and return typed restart/update/current-binding guidance to stale clients.
8. Archive receipts and retain V1 source stores only for the bounded rollback/data-verification period defined by the cutover receipt, then explicitly archive/remove them without deleting unmigrated user data.

Before the V2-default cutover receipt closes, an operator rollback may restore the migration-mode V1 owner at a declared watermark. After V2 default, rollback means the prior compatible V2 implementation/schema or data restore—not revival of stale V1 live bindings. It leaves evidence/read models intact for diagnosis, terminates incompatible subscriptions with a restart reason, and never reverse-deletes V2 canonical events.

## 13. PR and TDD Execution Plan

Commands run from the repository root with the checkout-local `target/`; do not override target/data directories unless Cargo reports target-lock contention. Each red test must fail for the named missing contract before implementation.

### PR 24A1: Crate boundary, request context, registry, and architecture rules

**Files:** workspace `Cargo.toml`; application `Cargo.toml`; `src/{lib,error,context,use_case,registry,response,migration}.rs`; `tests/registry_completeness.rs`; `tests/fixtures/v2/use-case-catalog.json`.

- [ ] Add tests `every_catalog_use_case_has_exactly_one_implementation`, `query_and_command_ids_do_not_overlap`, `context_time_is_explicit`, `missing_capability_has_stable_error`, and `application_has_no_forbidden_dependency`.
- [ ] Run `cargo test -p tracedecay-application --test registry_completeness -- --nocapture`. Expected: compilation fails because the crate and registry do not exist.
- [ ] Implement the kernel types from Section 7, load generated tool-catalog descriptors, register all Section 9–10 IDs as typed descriptors, and add dependency lint.
- [ ] Re-run the command. Expected: all tests pass; generated inventory has no duplicate/orphan implementation and no transport/storage-concrete import.
- [ ] Commit `feat(application): add use-case registry and request contracts`.

### PR 24A2: Authorization, query composition, and explicit coverage

**Files:** `src/{access,response}.rs`; `src/ports/{catalog,evidence}.rs`; `src/use_cases/{capabilities,scopes,query,search,settings}.rs`; `tests/{authorization_privacy,query_coverage}.rs`.

- [ ] Add tests `catalog_default_is_materialized_before_execution`, `brain_default_is_active_profile_all`, `current_invocation_is_reported_and_never_overrides_explicit_target`, `cwd_and_last_project_never_narrow_scope`, `same_name_scope_returns_ordered_candidates`, `candidate_token_retries_original_request_once`, `scope_result_is_identical_across_cli_mcp_http`, `denies_before_scope_expansion`, `binds_access_digest_to_query`, `locked_shard_returns_metadata_coverage`, `partial_query_preserves_every_disposition`, `query_does_not_write_usage_counter`, `reasoning_requires_explicit_grant`, `settings_report_effective_source_and_owner`, `environment_setting_is_not_writable`, `foreign_doctor_finding_has_no_update_action`, and `partial_provider_is_not_healthy`.
- [ ] Run `cargo test -p tracedecay-application --test authorization_privacy --test query_coverage -- --nocapture`. Expected: tests fail because access/query services are absent.
- [ ] Implement authorization-first execution, `QueryAccess` conversion, response metadata propagation, deadline/cancellation, and capability/scope/query/search/entity use cases.
- [ ] Re-run the command. Expected: all tests pass; denied fixture opens zero shards; read port mutation sentinel remains zero.
- [ ] Commit `feat(application): authorize and compose federated reads`.

### PR 24A3: Native and representative message/session contracts

**Files:** `src/use_cases/{sessions,agents}.rs`; `tests/message_representation.rs`; redacted #410 compatibility fixtures.

- [ ] Add tests `native_rows_preserve_retained_structure`, `representative_preserves_source_ids_and_rule`, `representative_expansion_cannot_double_count`, `direct_user_excludes_delegated_and_protocol_rows`, `unknown_origin_remains_visible`, and `native_expansion_is_cursor_bounded`.
- [ ] Run `cargo test -p tracedecay-application --test message_representation -- --nocapture`. Expected: tests fail because audience/representation contracts do not exist.
- [ ] Implement Section 9.3 use cases over query/projector classifications; representative projection must carry represented IDs, observations, algorithm version, suppression count, and expansion cursor.
- [ ] Re-run the command. Expected: exact sanitized-native fixture count/manifest digest matches the retained source manifest; representative expansion reconstructs the same retained set once with no deletion.
- [ ] Commit `feat(application): expose complete sanitized message audience views`.

### PR 24A4: Brain, graph-of-graphs, timeline, and domain reads

**Files:** `src/use_cases/{brain,activity,graph,timeline,sessions,agents,coordination,code,delivery,knowledge,automation,observatory,accounting,research}.rs`; `tests/graph_of_graphs.rs`; `benches/brain.rs`.

- [ ] Add tests `brain_uses_rollups_before_project_shards`, `federated_graph_preserves_repo_snapshot_identity`, `same_name_symbol_never_collapses_cross_repo`, `rspack_rsbuild_react_router_fixture_keeps_provenance`, `each_lens_rejects_illegal_edge_kind`, `selection_pivots_at_same_watermark`, `temporal_correlation_is_not_causation`, `git_drift_blocks_joined_impact`, `turn_hub_preserves_native_semantics`, `codex_goal_updates_remain_first_class`, `research_anchor_survives_cursor_and_handle_expiry`, `recipe_reports_version_and_watermark_drift`, `nearby_parallel_worktree_has_direct_overlap_evidence`, `expired_presence_is_unknown_not_absent`, `safe_summary_contains_no_secret_payload`, and `partial_component_does_not_fail_brain`.
- [ ] Run `cargo test -p tracedecay-application --test graph_of_graphs -- --nocapture`. Expected: tests fail because graph/Brain compositions are absent.
- [ ] Implement Section 9.2/9.4 compositions with bounded query profiles, evidence-bearing cross-links, stable inspector/timeline refs, local/live Git reconciliation, and domain response schemas.
- [ ] Re-run the command. Expected: all tests pass; irrelevant shard open counter remains zero; no inferred edge uses observed/causal copy.
- [ ] Run `cargo bench -p tracedecay-application --bench brain -- --save-baseline pr24a4`. Expected: first useful response meets the master two-second current-scale gate and reports shard opens, watermarks, component coverage, bytes, p50/p95.
- [ ] Commit `feat(application): compose Brain and investigation reads`.

### PR 24A5: Command unit of work, idempotency, optimistic versions, and audit

**Files:** `src/{unit_of_work,idempotency,audit,optimistic}.rs`; `src/ports/{command_store,event_sink}.rs`; `src/use_cases/commands/{mod,runner}.rs`; `tests/{command_pipeline,idempotency_optimistic}.rs`; `benches/commands.rs`.

- [ ] Add tests `identical_retry_returns_stored_receipt`, `changed_payload_same_key_conflicts`, `version_conflict_writes_nothing`, `confirmed_operation_preflight_token_must_match`, `scope_sensitive_create_requires_declared_scope`, `route_scope_never_selects_owner`, `target_owner_conflict_writes_nothing`, `canonical_event_audit_outbox_and_result_commit_atomically`, `outbox_cannot_create_domain_truth`, `external_effect_never_runs_inside_uow`, and `writer_takeover_fences_stale_commit`.
- [ ] Run `cargo test -p tracedecay-application --test command_pipeline --test idempotency_optimistic -- --nocapture`. Expected: tests fail because command runner/unit-of-work contracts are absent.
- [ ] Implement Section 8 single-owner pipeline, command receipts, safe error details, preview expiry/revalidation, and audit redaction.
- [ ] Re-run the command. Expected: all tests pass; crash-before/after-commit fixture yields either no effect or one effect and repeatable receipt.
- [ ] Run `cargo bench -p tracedecay-application --bench commands -- --save-baseline pr24a5`. Expected: reports preflight/confirmed-commit/direct-commit/idempotent-retry p50/p95 and transaction duration without external I/O.
- [ ] Commit `feat(application): add audited idempotent commands`.

### PR 24A6: Resumable workflows and operational commands

**Files:** `src/{jobs}.rs`; `src/ports/{workflow_store,operations,capture,projection,remote_delivery}.rs`; `src/use_cases/operations.rs`; all `src/use_cases/commands/*.rs` except runner; `tests/workflow_recovery.rs`.

- [ ] Add workflow fault cases for process death before/after step effect and receipt, duplicate worker, stale lifecycle lease, drain with active MCP/watch/index work, upgrade process exit before durable drain receipt, update restart/takeover/recovery, version drift, disk pressure, cancelled export, projection publish failure, retention hold, migration ambiguity, #425 split-store open-holder refusal/freeze/write-reservation/backup/staging/verification/cutover/restart recovery, remote refresh followed by ref rewrite, coordination target expiry/delivery-without-ack/duplicate handoff/suppression, scope-owner move conflict, share-bundle expiry/revocation, and irreversible delete grace.
- [ ] Run `cargo test -p tracedecay-application --test workflow_recovery -- --nocapture`. Expected: tests fail because workflow runner/definitions are absent.
- [ ] Implement each Section 10 command descriptor, pollable operation status, and the workflows named in Section 8.3; every external effect is a separately receipted step and every owner transaction is idempotent.
- [ ] Re-run the command. Expected: every fault fixture reaches one named recoverable/terminal state, no duplicate effect receipt, and unaffected shard reads remain available.
- [ ] Commit `feat(application): orchestrate recoverable operational workflows`.

### PR 24A7: Replay labs and Evolution Studio

**Files:** `src/use_cases/labs/*.rs`; `src/use_cases/commands/labs.rs`; `src/ports/archive.rs`; `tests/labs_read_only.rs`.

- [ ] Add exact/recorded/best-effort fixtures for every lab plus `search_quality_preserves_cutoff_qrels_and_anchor`, `scope_federation_replays_resolution_and_shard_plan`, `privacy_lab_accepts_synthetic_canary_only`, `coordination_selects_at_most_one_hint`, `coordination_replay_preserves_suppression_and_anchor`, `coordination_lab_cannot_message`, `evolution_tracks_skill_and_memory_lifecycle`, `simulation_reports_unknown_outcome_horizon`, `lab_ports_have_no_write_method`, `simulation_does_not_increment_counters`, and `promotion_requires_scan_and_confirmation`.
- [ ] Run `cargo test -p tracedecay-application --test labs_read_only -- --nocapture`. Expected: tests fail because application lab compositions are absent.
- [ ] Compose policy/query/capture/projector evaluators with immutable refs, preserve fidelity/substitutions/coverage, implement Evolution inspection/simulation, and keep fixture promotion in the separate command path.
- [ ] Re-run the command. Expected: all tests pass; write sentinels remain zero; exact digests verify; unavailable artifacts downgrade/refuse explicitly.
- [ ] Commit `feat(application): add read-only replay and evolution labs`.

Plan 18's PR 24H extends these same application registries/ports with privacy status, scan, safe finding, remediation, verify, detector, and quarantine use cases after PRs 7A/10A/12C/22B. It is not a second privacy service or transport-specific workflow. The official API/SDK slices in plan 17 generate from the same registry after PR 24A/24B contracts are stable.

### PR 24A8: Future-master migration and V1 parity harness

**Files:** `src/migration.rs`; `tests/{future_master_migration,v1_parity}.rs`; generated post-merge parity fixture.

- [ ] Add copied/redacted fixtures for merged #405 unique/ambiguous legacy adoption, merged #412 daemon drain/update recovery, #407 sessions/facts-only/profile identity, #410 native/origin/representative messages, #411 foreign-owner doctor severity, merged #425 split-store consolidation manifests/recovery guidance, release-only #413 inventory drift, local/live Git drift, and every V1 writable dashboard action.
- [ ] Run `cargo test -p tracedecay-application --test future_master_migration --test v1_parity -- --nocapture`. Expected: parity assertions fail before bounded shadow dispatch/receipts are complete.
- [ ] Implement per-use-case V1/V2 dispatch and `UseCaseParityReceipt`; regenerate inventory from actual accepted master rather than the planning branch snapshots.
- [ ] Re-run the command. Expected: zero duplicate canonical entities, exact native-message hashes/counts, representative provenance parity, all mutations accounted, and every divergence explained by a checked-in receipt.
- [ ] Commit `test(application): prove future-master and V1 use-case parity`.

### PR 24E series: Thin current CLI/MCP/dashboard adapters and internal shadow harness

**Files:** companion adapter/test files in Section 5 and one existing V1 domain handler family per PR.

- [ ] Add a semantic fixture that invokes one `UseCaseId` through in-process application, HTTP JSON, CLI JSON, MCP JSON, dashboard client, and subscription/export where applicable; compare data, order, scope defaults/candidates/retry, provenance, coverage, watermarks, errors, command receipts, and audit refs before formatting.
- [ ] Run the domain's transport parity test. Expected: fail while at least one adapter selects V1 stores/services directly or omits required metadata.
- [ ] Replace one CLI/MCP/dashboard adapter domain with current generated argument/result mapping to application. Exercise old flags/tool schemas only inside the internal parity harness; do not publish them as post-cutover aliases or fallbacks. Provider hook adapters migrate under PR 24F after this crate's `HookApplicationPort` is stable.
- [ ] Re-run focused V1 and V2 tests. Expected: semantic fixtures match; only approved presentation whitespace differs; handlers import no store/query/policy concrete modules.
- [ ] Commit one domain at a time as `refactor(<transport>): route <domain> through application use cases`.

## 14. Performance, Reliability, Privacy, and Migration Gates

- Application adds at most 5 ms p95 overhead over query engine time for ordinary reads and at most 10 ms p95 outside the owning store transaction for ordinary commands on the reference machine.
- Brain composition opens no irrelevant shards, returns the first useful evidence within two seconds at current scale, and names partial components instead of failing globally.
- One request opens at most 32 shards through query; no application cursor/page retains a read transaction.
- 64 concurrent reads plus 32 command producers preserve exact authorization and idempotency; command writer queues remain bounded in store.
- 10,000 identical concurrent command retries yield one domain effect/audit event and the same receipt. 10,000 conflicting expected versions yield no partial mutation.
- Workflow kill matrix covers every external-effect/receipt boundary; duplicate or takeover execution never publishes a second semantic effect.
- Secret corpus and named plan 18 bypass regressions produce zero query literal/audit/export/fixture/log/catalog/summary/error/response-handle/backup leaks. Every application output satisfies `TransportEligibleView`; locked/retained/redacted/reasoning behavior matches domain policy.
- Every response includes resolved scope, exact coverage/freshness/redaction/retention/applied limits and catalog digest. Every command includes owner/version/watermark/audit and optional operation/workflow; pending work has a pollable status read and explicit terminal disposition.
- Message native mode exports exact source rows; representative mode can expand to that set with complete provenance and no hidden deletion.
- Local/live Git drift never yields a joined semantic/live conclusion; refresh/reindex action is explicit.
- Nearby-agent results distinguish same/parallel worktree and direct/weak overlap evidence, expose safe anchor-backed summaries, expire presence honestly, and never send/ack/handoff without a separate authorized receipt. One eligible overlap horizon emits at most one deduped dynamic hint.
- Search gates pass per-slice lexical/phrase/fuzzy/entity/semantic/graph/recency benchmarks; exact-match and origin/kind-filter regressions block release even when aggregate hybrid scores improve, and embeddings may be disabled by profile.
- Every current read/mutation in generated compatibility inventory has one use-case owner and status; no dashboard-only behavior remains before retirement.
- Every scope-sensitive row and command exposes declared scope/canonical owner; route/project selection never changes ownership, and cross-project reuse never duplicates durable memory/skill/policy/automation state.
- All/repository/project/worktree/ref scopes have identical generated semantics across CLI/MCP/API/dashboard; same-name ambiguity is candidate-based with one-step retry, and federated results retain per-repository provenance/stale/partial state.
- New production files target at most 800 lines. All architecture, clippy, test, property, crash, differential, and benchmark suites pass.

## 15. Cutover and Removal

1. Ship registry/read contracts behind `v2_application_shadow` with V1 effect ownership unchanged.
2. Cut over read use cases only after semantic parity, partial-state, privacy, performance, and transport fixtures pass.
3. Enable V2 operation-specific inspection/preflight while V1 still owns mutation; compare validation/impact without mutation.
4. Cut over each command only after idempotency, audit, workflow recovery, rollback, and side-effect parity receipts pass.
5. Keep migration dispatch reversible per domain/use case until that domain's V2-default receipt closes; do not retain it as live compatibility afterward.
6. During bounded migration, rollback may restore V1 ownership from the receipt. After V2 default, terminate incompatible subscriptions/cursors/previews with typed restart and recover through the prior compatible V2/data snapshot without re-enabling stale names.
7. At V2 default, disable old live CLI/MCP/HTTP/dashboard names and handlers. Stale versions fail clearly with required restart/update and the current generated binding; they never route silently to V1.
8. Remove a V1 handler/service after internal parity/backfill/rollback receipts are archived and all non-disposable data is migrated or explicitly quarantined; compatibility duration is receipt-bounded, not a generic release count.

## 16. Final Verification

- [ ] Run `cargo fmt --check`. Expected: exit 0.
- [ ] Run `cargo clippy -p tracedecay-domain -p tracedecay-query -p tracedecay-policy -p tracedecay-tool-catalog -p tracedecay-application --all-targets -- -D warnings`. Expected: exit 0, no warnings.
- [ ] Run `cargo test -p tracedecay-application --all-features`. Expected: all unit/integration/property/fault tests pass, none ignored.
- [ ] Run the V1 storage/session/LCM/Git/memory/hook/automation/CLI/MCP/dashboard suites referenced by generated compatibility inventory. Expected: all remain green until their declared retirement.
- [ ] Run transport semantic parity for every registered use case. Expected: identical typed semantics or checked-in, approved compatibility difference; zero missing mutation owners.
- [ ] Run application benchmarks at current and 10x corpora. Expected: Section 14 gates pass and output records corpus, reference machine, vector watermark, shard opens, p50/p95, allocations, and peak RSS.
- [ ] Run `rg -n 'axum|tower|rmcp|clap|rusqlite|libsql|git2|octocrab|reqwest|std::process|dashboard/' crates/tracedecay-application/src`. Expected: no matches.
- [ ] Inspect `cargo metadata` dependency graph. Expected: application depends inward on contracts; no lower crate imports application; adapters are the only outward dependents.
- [ ] Compare the generated capability/use-case inventory with V1 MCP, CLI, dashboard, hook, config, schema, and sidecar inventories. Expected: no orphan read, mutation, or compatibility alias.
- [ ] Complete #405/#407/#410 ownership/message migration, #411 doctor authority, #412 drain/update recovery, #413 inventory refresh, stable research anchor/recipe, cross-shard recovery, local/live Git drift, lab read-only, privacy, cutover, stale-client failure, and rollback drills before V2 application becomes default.
