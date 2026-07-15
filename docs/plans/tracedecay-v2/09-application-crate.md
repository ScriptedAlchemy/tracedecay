# TraceDecay V2 Application Crate Implementation Plan

> **Accepted-base refresh delta (audit 29 / packet 30):** the automation
> memory-curator already has bounded retry/timeout behavior; retired FM-168 adds
> no application or policy obligation. At the `run(cli)` composition surface,
> support plan 12/root lifecycle's end-to-end daemon shutdown deadline for a
> never-resolving outer `shutdown_background_tasks().await` (FM-163). See
> [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
> §5, §7.2, §7.4.

**Goal:** Build `tracedecay-application`, the transport-neutral use-case layer that authorizes and orchestrates every TraceDecay V2 read, command, replay lab, export, migration, and internal parity operation through one auditable contract.

**Architecture:** Queries compose catalog, query, policy, tool-catalog, projector, and immutable archive ports under one captured request context and return explicit snapshot, coverage, freshness, redaction, and provenance. Non-curation commands use typed execution contracts and, when destructive, an operation-specific inspect or immutable plan followed by confirmed start/commit; all commands use idempotency, optimistic aggregate versions, one owning-shard unit of work, one authoritative canonical command-event journal, referenced audit/outbox entries, and resumable workflows for cross-shard effects. Autonomous curation effects have no per-item command. HTTP, CLI, MCP, hooks, and dashboard adapters only map transport data to these use cases.

**Tech Stack:** Rust 2024 workspace; `tracedecay-domain`; `tracedecay-query`; `tracedecay-policy`; `tracedecay-tool-catalog`; store/projector traits; `serde`; `schemars`; `thiserror`; `futures`; `tokio` at the composition boundary; `uuid`; property/contract/differential tests.

---

## 1. Contract Lock

This plan refines master-plan PR 24A, supplies the application contracts consumed by PRs 24B–24E and 25–32, and owns transport parity until V1 retirement.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) adds task/plan registered values and builders for canonical `TraceQueryV1`, command use cases, one authoritative scheduler, owner-shard graph transactions, executor registration/route resolution, fenced offer-acceptance/lease/heartbeat/terminal workflows, context-packet assembly, workspace/cancellation/effect reconciliation, status, and doctor under this application boundary. `WorkClaimV1` remains advisory coordination evidence; only `task_offers.accept` invokes the internal atomic lease-acquisition transaction and issues execution authority. No root/adapter/dashboard module may become a second query engine, scheduler, event journal, or lease authority. Those task/plan reads and commands are enumerated in the Section 9–10 inventories below; every task mutation is a POST command-envelope use case (plan 10 §8.7) with no PATCH transport shape.

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
- Lab evaluators are read-only against production state. The generic experiment/run operation may persist only immutable replay artifacts and explicitly granted model/egress cost; fixture promotion, policy/config activation, export publication, and non-curation mutations are separate audited commands. Curation labs are inspectors only: the autonomous curation worker applies policy-eligible fact/memory/skill evolution independently, with no per-item preview/apply/rollback UI.
- Canonical transcript enumeration preserves every sanitized native row and is lossless for retained non-secret structure/semantics. Message enumeration/search consumes domain `MessageOrigin`/`MessageView` unchanged, exposing native, representative, human-best-effort, direct-user, delegated-agent, tool-result, and provider-protocol views; query-time dedupe never deletes or rewrites sanitized source observations.
- `All` means the active `ProfileId`. Additional profiles require an explicit collection/scope and separate authorization; there is no implicit Hermes profile.
- Every fact, skill, policy, automation, saved investigation, and annotation carries domain `DeclaredScope`. Profile/zero-project/cross-project instances are activity-owned; explicitly project-scoped instances are project-owned. A selected project, route, working directory, or active filter is never an ownership default, and unresolved scope blocks mutation rather than guessing.
- Projectless provider activity is accepted into the profile activity shard without fabricating a project. Durable user preferences use explicit `DeclaredScope::Profile`; conversation-local unresolved/general-chat evidence remains `DeclaredScope::ZeroProject`. The application classifies a single `ScopeRootV2::Profile` request before project discovery; the query AST may filter the rows by either declared scope, while legacy scalar user/profile aliases combined with compatibility project fields are invalid. Fact, LCM, memory-status, and message-search calls in that route reach the authenticated profile owner with no project handshake/open/init; CWD and host home are not even fallback candidates. A canonical authorized multi-root read may explicitly contain Profile plus Project and uses `ExplicitReadScope`. Active-project memory reads use plan 05's generated profile-plus-exact-project selector and preserve both owner scopes; no application use case opens a separate user-memory database, copies a profile fact into projects, or treats a Hermes/Codex/Claude/Cursor host profile as a TraceDecay data profile.
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
- Provide one parity harness proving identical application semantics across a hermetic in-process test oracle and production daemon-protocol HTTP/local-IPC, CLI JSON, MCP JSON, dashboard client, export, and subscription transports. No production client uses the oracle to bypass daemon authority.
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

### 4.1 Master and incoming changes verified through 2026-07-11

The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). Proxy routing, catalog refresh, fact ranking, exact analytics, release metadata, restart-safe applied-manifest retirement, and the offline fail-closed resumable two-nonempty-profile-shard consolidation workflow are accepted application inputs; consolidation remains operator administration, not autonomous curation.

| Change | Assumed future behavior | Application consequence |
|---|---|---|
| Merged PR #405, legacy identity-store adoption | The lifecycle resolver adopts uniquely matching legacy stores and records migration/adoption evidence. | Scope resolution and migration commands consume canonical post-adoption `ShardRef`s. Preview must surface ambiguous/split identities and block cutover rather than exposing duplicate projects. |
| Merged PR #412, safe daemon drain during upgrades | Daemon/MCP/watch/index work is leased, drainable, recoverable, and reports update safety state. | Operation/status reads and update/daemon commands preserve lease epoch, accepting/draining/stopped state, in-flight counts, progress, takeover/recovery, and last durable receipt; “process exited” is not equivalent to safely drained. |
| PR #407, Hermes user-profile consolidation | Hermes sources/facts/sessions migrate into the ordinary user TraceDecay profile and Hermes-specific bridges are removed. | Hermes, curator, reflector, and skill-writer are actors/workflows inside the active profile. No use case accepts an implicit Hermes-profile switch or calls removed bridge/config/inventory paths. |
| Merged PRs #441/#443/#445/#448, Hermes memory/context, rollout recovery, projectless routing, and user-message refresh | User/profile compatibility stores shipped; legacy session import distinguishes neutral/project/unresolved evidence; host-profile ownership comes from installed/configured plugin identity; provider workspace/home resets per session; selected-profile user-scoped memory/LCM/message search bypasses project routing; registry/read-only/mutating routes are distinct. | Treat these as V1 parity and migration fixtures. Application owns canonical scope classification, every mixed-selector rejection including `project_key`, route class, authority, explicit refresh operation, and truthful coverage; host adapters/handlers cannot derive ownership from `HERMES_HOME`/CWD, retain another client's profile, ingest during reads, open stores, promote unresolved memory, or maintain tool-name route allowlists. |
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

## 5. Responsibility inventory and exact physical layout rule

The expanded tree below is a responsibility inventory for review coverage; its flat names are not permitted implementation paths. The normative physical rule after the inventory is `kernel/**` plus `features/<domain>/{queries,commands,views,ports}/**`, which collapses the inventory into bounded feature modules and prevents a second flat `use_cases`/`commands` architecture.

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
│   │   ├── host_deployment.rs          # root-owned host probe/config/install/reload effects
│   │   ├── user_effects.rs              # signed scoped user-identity effect grants/receipts/reconciliation
│   │   ├── hooks.rs                   # HookApplicationPort evaluation/delivery boundary
│   │   └── event_sink.rs              # canonical command/evaluation/outcome append port
│   └── use_cases/
│       ├── mod.rs                     # only executable capability registry entrypoints
│       ├── query.rs                   # generic TraceQueryV1 execution
│       ├── search.rs                  # universal search profile
│       ├── search_evaluation.rs       # corpus/qrel/pool/judgment/report/profile reads; runs use experiments
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
│       ├── automation.rs              # jobs/runs/skills/candidates/decisions/effects/recoveries/artifacts/outcomes
│       ├── observatory.rs             # health/coverage/ingest/projection/privacy/migrations
│       ├── privacy.rs                 # policy/coverage/findings/scan/remediation/quarantine reads
│       ├── accounting.rs              # usage/cost/savings and denominators
│       ├── settings.rs                # effective values, sources, scope, and impact
│       ├── operations.rs              # durable command/job/workflow status/recovery
│       ├── integrations.rs             # generated host inventory/difference/status views
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
│       │   ├── integrations.rs         # install/update/repair/uninstall/verify workflows
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
│       │   ├── research.rs            # append immutable research-manifest versions
│       │   ├── search_evaluation.rs   # immutable eval artifacts/reports/profile activation; no run lifecycle
│       │   ├── exports.rs             # create/cancel/publish/delete export jobs
│       │   ├── tokens.rs              # auth.tokens.create/list/revoke over plan 17 §18.2's registry
│       │   ├── saved.rs               # save/share/update/delete investigation state
│       │   ├── experiments.rs          # create/run/cancel/resume/retry/minimize through OperationKernelV1
│       │   └── fixture_promotion.rs    # sanitized fixture-promotion command only
│       └── experiments/
│           ├── mod.rs
│           ├── manifest.rs             # exact/recorded/best-effort input/version/environment closure
│           ├── hermetic.rs             # frozen clock/RNG, immutable mounts, overlay, capability deny/receipt
│           ├── runner.rs               # one operation-backed lifecycle over evaluator registry
│           ├── trace.rs                # stable stages/anchors
│           ├── comparison.rs           # one alignment vocabulary
│           ├── minimize.rs             # bounded typed delta debugging
│           └── evaluators/             # fourteen typed adapters; no lifecycle/persistence/effect ports
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
│   ├── experiments_replay.rs
│   ├── future_master_migration.rs
│   └── v1_parity.rs
└── benches/
    ├── brain.rs
    ├── commands.rs
    └── subscriptions.rs
```

Companion implementations owned by later adapter PRs:

```text
src/v2/api/**
src/cli/v2_adapter/**
src/mcp/v2_adapter/**
src/v2/hooks/**
src/dashboard/v2_compat_api/**
tests/v2_transport_parity/**
tests/fixtures/v2/use-case-catalog.json
tests/fixtures/v2/v1-compatibility.json
```

Canonical composition rule: concrete glue for capture, projectors, query, and policy archives lives only at `src/v2_adapters/{capture_store,projector_store,query_store,policy_archive}/**`. Application retains only the ports above. Query/search/graph/timeline/export/subscription compose under their `src/features/<domain>/` owners; hook integration is `src/features/hooks/`; host-integration orchestration is `src/features/integrations/{queries,commands,views,ports}.rs`; the generic experiment harness and evaluator adapters are `src/features/experiments/`. No flat `src/use_cases/**`, parallel `src/commands/**`, or `src/labs/**` tree is created.

The later agent-coordination/search-quality requirement extends bounded files under existing lower-crate owners: `tracedecay-projectors/src/read_models/coordination.rs`, plan 05's existing `operators/coordination.rs`, `rank/**`, and `eval/**` modules, `tracedecay-policy/src/evaluators/coordination.rs`, and generated tool-catalog definitions/bindings. No `profiles/search_benchmark` production module or lab-specific runner is added. Application consumes those ports; it does not reimplement projection, ranking, evaluation metrics, or hint policy. These additions extend PRs 16/17/23C/22A before PR 24A4/24A7 and require their own registry/parity receipts.

Production modules target at most 800 lines. The physical layout is `kernel/{context,access,uow,idempotency,operations,subscriptions,audit}` plus `features/<domain>/{queries,commands,views,ports}`; the inventory above names responsibilities, not permission for parallel flat registries or a giant `commands/runner` match. Kernel cannot import features. Features cross only through explicit registered workflows, import `UseCaseId`/descriptors from plans 01/08, and register handlers through generated bindings. Domain-specific orchestration stays in its feature; transport-specific mapping stays in adapters.

Reuse is measured, not aspirational. The V1 seeds include separate automation run ledgers/dashboard task endpoints, index `sync_with_progress`, consolidation ledgers, cancel-on-drop guards, daemon `operation_in_flight`, query/result envelopes, and transport-specific error helpers. PRs 24A1/24A5/24A6 extract one application kernel and delete those mechanics as each feature cuts over; wrapping them behind ports does not satisfy the plan-19 negative-code gate.

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
- `experiments/evaluators/**` may use immutable archive/query/policy ports only. `experiments/runner.rs` may write only operation and experiment artifacts through its narrow repository; architecture tests deny every production write/counter/cache/lease/effect port. The only fixture-write operation is `commands/fixture_promotion.rs`, which requires a sanitized artifact and explicit confirmation receipt.
- `commands/**` cannot call an HTTP/GitHub/process/filesystem adapter while a unit of work is open. External operations run before revalidation or after durable workflow-step commit.
- Reject imports containing `axum`, `tower`, `rmcp`, `clap`, dashboard packages, `rusqlite`, `libsql`, `git2`, `octocrab`, `reqwest`, `std::process`, or provider-specific hook modules.
- A `cargo metadata` architecture test asserts adapters point inward and no cycle exists among application/query/policy/store/projectors.

`UserEffectPortV1` is the one generic application boundary for filesystem/Git/worktree/owned-host-config/contained-workspace effects that must execute under the ordinary user identity while strong database isolation runs the daemon under a service identity. Application owns authorization, exact resource/precondition manifests, capability intersection, idempotency, revocation generation, expiry, audit, and uncertain-effect reconciliation; root plan 12 PR 24E0 implements the local broker transport and race-safe primitives. Plan 24 task effects reuse this kernel and add lease/attempt proof rather than defining another broker. Source capture remains a separate read-only port. Neither application nor the daemon receives ambient filesystem/process capability or a TraceDecay store path through this port.

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
    LocalProcess { os_user: OsUserRef },  // authenticated local daemon client principal
    InternalWorker,                       // curation/workflow/scheduler actors with recorded provenance
}

pub struct GrantSet {
    pub capabilities: BTreeSet<CapabilityGrant>, // Read | Preview | Mutate | Admin | named destructive grants
    pub scope_constraints: Vec<ScopeSelectorV2>,
    pub sensitivity: SensitivityGrantSet,
    pub expires_at: Option<UtcMicros>,
}
```

Daemon composition mints the `LocalProcess` principal for CLI and MCP clients after verifying local endpoint peer credentials against the profile owner and resolving the operator's local token grants. Client adapters submit credentials/context but never construct an authoritative principal or application/store service. CLI inherits the operator token's grants; local MCP agent hosts default to Read+Preview and receive Mutate/Admin only from an explicit scoped token (plan 17 §17's read-only default for direct agent credentials). No adapter constructs an ambient admin principal, and the per-launch bootstrap bearer (plan 10 §10.2) authenticates only `auth.tokens.create` for the initial admin-class token.

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
- Queries never discover, parse, ingest, backfill, repair, refresh, or checkpoint a provider source. They answer from one captured watermark with explicit stale/partial coverage. A caller that requires newer evidence starts or joins the separately authorized refresh command and receives an `OperationRef`; no `catch_up` boolean turns a read into a hidden command.
- Commands always have a canonical owner, idempotency key, expected version, authorization decision, audit schema, and catalog-owned `ExecutionModeV2`. Direct commits execute once; autonomous policy effects are not public item commands; resumable workflows return an operation; host lifecycle events remain internal. A destructive operation exposes a separately named typed preflight use case and a separately named confirmed domain command (for example `storage.consolidation.plan` then `storage.consolidation.start`) whose payload carries the preflight receipt/token. There is no universal `preview`, `apply`, `dry_run`, or `ApplyConfirmation` method.
- An operation-specific preflight captures aggregate/evidence versions, impact, redactions, disk/network/process effects, and a typed confirmation token. The confirmed domain command revalidates every version/capability/hold/freshness dependency; operations that do not need this safety boundary do not manufacture a preflight.
- Retrying an identical completed command returns the stored receipt. Reusing an idempotency key with a different canonical command digest returns `idempotency_conflict` without mutation.
- Adapters cannot invoke repository operations directly; the use-case registry is the only executable capability surface.

Query inputs embed one typed read envelope; adapters map it losslessly instead of inventing per-transport consistency semantics:

```rust
pub struct ReadRequirementsV1 {
    pub consistency: ReadConsistencyV1, // Authoritative | BoundedStale | OfflineCache | AsOfWatermark
    pub freeze_pagination: bool,         // orthogonal snapshot/cursor behavior, not a consistency mode
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
- New content-bearing commands, model summaries, remote payloads, V1 compatibility rows, operator notes, and generated failure details enter as `Unclassified<T>` and must receive a complete `SanitizationReceiptV1` before inspection result, audit, or persistence. Scanner timeout, incomplete parsing, unsupported encoding, or missing policy returns blocked/unknown coverage and persists only a non-content receipt.
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
2. For a catalog-declared destructive operation only, execute its named inspect/plan use case against a frozen read snapshot and return the operation-specific impact, requirements, and immutable confirmation token. Ordinary commands have no preview phase.
3. Execute the exact named command; when its catalog metadata requires confirmation, validate the matching unexpired operation-specific token and deadline, then perform any safe external preflight outside the transaction. There is no generic apply route.
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
    pub key: IdempotencyKeyV1,            // caller-supplied, <=128 bytes
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
    pub key: IdempotencyKeyV1,
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
}
```

`CommandId` is allocated deterministically by the application on first `execute` — a digest over principal, use case, idempotency key, and canonical command digest — so retry is stable and at most one ID exists per reservation; adapters never mint it. `OperationPreflightV1` exists only for catalog-declared confirmed destructive workflows, uses its own idempotency/expiry/authorization contract, and cannot be passed to an unrelated use case. An expired preflight returns `operation_preflight_expired` and requires the same named preflight use case again.

Version conflict returns the current version, changed dependency IDs, safe summary, and, only for a confirmed operation, a new-preflight requirement. It never auto-rebases a destructive command. Idempotent status/run/refresh requests may explicitly declare a merge policy; that policy is versioned in the catalog and fixture-tested.

### 8.3 Cross-shard operation workflows

```rust
pub struct OperationWorkflowDefinitionV1 {
    pub kind: OperationWorkflowKindV1,
    pub version: SemVer,
    pub steps: Vec<OperationWorkflowStepSpecV1>,
}

pub struct OperationWorkflowStepReceiptV1 {
    pub operation: OperationId,
    pub step: OperationStepId,
    pub attempt: u32,
    pub expected_versions: VersionVector,
    pub input_digest: ContentDigest,
    pub disposition: OperationStepDispositionV1,
    pub effect_receipts: Vec<EffectReceiptRef>,
    pub compensation: Option<CompensationRef>,
}
```

These types are closed, application-owned recipes over `OperationKernelV1`; they are not Plan 32 user-authored `WorkflowDefinitionV1`, workflow source, executable IR, or replay history. A dynamic workflow may invoke registered application use cases through its execution-unit envelope, but neither system translates into or executes the other's definition type. `CommandReceipt.operation` is the sole pending-work pointer for both a direct durable operation and a cross-shard operation workflow; there is no second generic `WorkflowRef` status family.

Cross-shard operation workflows cover retention/delete descendants, profile/project settings propagation, export publication, projection rebuild/publish, migration/backfill/cutover, autonomous managed-skill materialization/supersession/recovery, backup/restore, and remote refresh plus local reindex. They obey:

- Durable state is written before executing the next effect; retries use the same workflow/step idempotency key.
- Each step owns at most one shard transaction or one bounded external effect, never both simultaneously.
- Leases are fenced by epoch. A takeover cannot publish a stale step receipt.
- Compensation is declared only where it is safe and semantic; irreversible content deletion has recovery grace and explicit terminal state, not fictional rollback.
- Partial completion is visible in Observatory and returned by status queries. Other shards remain queryable.
- Cancellation stops before the next step. It cannot undo a committed canonical observation, audit event, or externally completed effect.
- Workflow terminal states are `Succeeded`, `Failed`, `Cancelled`, `CompensationRequired`, or `Blocked`; `Blocked` names the missing authority/version/capability.

### 8.4 Shared fenced operation substrate

`OperationKernelV1<K>` is mechanical infrastructure, not a generic domain workflow. It supplies `OperationId`, typed kind key `K`, owner, epoch/heartbeat, expected versions, idempotency, phase/checkpoint ordinal, progress, cancellation intent, retry/takeover, effect/compensation refs, terminal disposition, and audit/status receipts. Its one attempt contract records immutable input, attempt ordinal/classification, bounded jittered backoff, `next_retry_at`, deadline, circuit state, idempotency/effect receipt, and uncertain-effect reconciliation. Migration, export, privacy repair, projection/index rebuild, daemon maintenance, automation, and task execution wrap it with closed domain admission/state/effect policies; none builds a private retry engine. Task leases remain execution authority and projection outbox leases remain consumer authority; they reuse the fenced-epoch/CAS primitives but are not interchangeable aliases.

Task dispatch and integration bind this kernel to plan 24 Appendix A's complete `TaskReconciliationGateV1`; application is the sole owner of its ordered inspection, five-dimensional verdict, snapshot/boundary revalidation, payload-bound idempotency, board-only CAS proposal, and durable `Prepared → Applying → ObservedApplied → Recorded` recovery protocol. Stores, schedulers, adapters, Git ports, and transports may not flatten those dimensions, infer equivalence, replay an in-doubt effect, or treat a consumed fence/process death as a receipt. Equal-key/equal-payload requests return the existing operation/receipt without mutation; changed payload under the same key returns `idempotency_conflict` before any journal, projection, offer, fence, or external effect.

Provider/session freshness is one registered aggregate operation over durable per-source lanes. `CaptureSourceLaneKeyV1` is the sole serialization/singleflight key and binds profile, provider, one `SourceInstanceId` plus source generation, sanitizer/parser/config digests, and authority epoch—never request scope, starting frontier, or target watermark. The lane stores one committed frontier and monotonic requested ceiling. Each `CaptureSourceDemandV1 { lane_key, observed_frontier, target_watermark }` joins that lane; under its fenced lease/CAS, a higher target extends the ceiling, a lower target waits for or is satisfied by existing progress, and only one scanner may advance any overlapping range. `CaptureRefreshOperationKeyV1` contains an ordered demand set plus declared attribution/read scope and aggregate coverage, so exact/overlapping/differently scoped/subset/superset requests share scans while retaining separate routing receipts. `OperationAdmissionRoleV1::Leader | Joiner` is recorded per lane and aggregate attachment, never as terminal outcome. Leader disappearance or joiner role cannot imply success. A changed source generation, parser/sanitizer/config digest, or authority epoch opens a new fenced lane; ordinary frontier/target changes do not.

Session hydration accepts plan-01 `SessionLocatorV1`. `Canonical(SessionId)` resolves directly under the pinned scope/snapshot; `Native { profile_id, provider_id, native_session_id }` performs bounded alias resolution and can return zero, one, or multiple generation/variant candidates. The application never hydrates from a native alias: one candidate is converted to canonical `SessionId`, ambiguity is rechecked at the pinned snapshot and returned unchanged across transports, and hydration then uses only the canonical ID.

One `OperationStorePort` and one generated operation-status view replace feature-local ledger/status/cancel/recovery engines. No worker holds a shard transaction across process, Git/network/model, filesystem, or user wait. Autonomous curation still performs eligible owned effects directly through its typed worker; this substrate does not create preview/apply/rollback proposals.

One narrow `SchedulerKernelV1` similarly supplies wakeup ingestion, backoff, fairness queues, checkpointing, and fenced admission. Task readiness/lease/offer semantics and automation job/run semantics remain separate registered policies above it; agent-bearing automation materializes canonical task work, and no second polling loop or executor-dispatch authority survives.

`StructuredEditWorkspaceKernelV1` is the equally narrow mechanical substrate for contained, expiring, user-editable representations. It owns opaque workspace allocation, owner/principal binding, frozen-base pins, byte/file/count limits, streamed upload/download staging, source-span diagnostics, candidate digests, TTL extension rules, purge state, and crash reaping under the profile runtime root. It never defines a document grammar, converts omission into deletion, allocates domain entity IDs, resolves semantic conflicts, or exposes server filesystem paths. The task-graph feature is the first consumer: it owns the strict Markdown/frontmatter compiler, graph validation, semantic diff/rebase, active-attempt impact, and atomic owner-shard submit described by plan 24. Any later consumer must register a separate closed format and semantic compiler while reusing this kernel; there is no public arbitrary-document workspace API.

Workspace bytes are transient private staging, not canonical events or drafts. Export and submit cross plan 18's sanitizer and secret-scan boundaries; a successful submit or explicit delete purges bytes immediately, failed validation retains them only for a bounded retry TTL, and the durable receipt keeps digests/counts/anchors/cleanup outcome but no content or local path. Archive ingestion rejects traversal, absolute paths, links, devices, duplicate members, decompression/count overrun, and non-UTF-8 before a domain parser runs.

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
| `brain.status.get` / `brain.topology.get` / `brain.nodes.list/get` / `brain.placements.list` / `brain.sync.status` / `brain.replicas.list` / `brain.backup.status` / `brain.repositories.candidates` | Plan 28's authorized multi-machine status/topology, fenced authority epochs, placement generation, grants/versions, replica/cache lag, pending spool/gaps/conflicts, recovery point, and Git identity candidates. Never returns addresses, raw paths, credentials, keys, database locations, or sync chunks. |
| `system.migrations.list` / `system.migration.get` | Import/backfill/cutover/rollback receipts, counts, hashes, quarantine, status. |
| `system.projections.list` / `system.projection.get` | Projector versions, input/output watermarks, lag, dead letters, generations. |
| `privacy.status.get` / `privacy.scans.list/get` / `privacy.scan.inspect` / `privacy.findings.list/get` | Effective safety floor/policy, source/sink/detector coverage and versions, last verified scan, safe finding classes/states, sanitized/quarantined/legacy-unscanned/unknown counts, and restore eligibility; inspect resolves protected scope/source selectors and estimates coverage without persisting a scan or exposing candidate content. |
| `privacy.detectors.list` / `privacy.detectors.diff` / `privacy.remediations.get` / `privacy.quarantine.status` | Detector metadata, synthetic-only comparison, descendant/rebuild/rotation state, and elevated quarantine metadata without plaintext. |
| `system.daemon.status` / `system.watchers.list` / `system.index.status` | Operational status and freshness only; lifecycle changes are commands. |
| `settings.effective.get` / `settings.sources.list` | Effective profile/project/integration/automation/storage settings, declared owner, source layer, default, validation, restart/reindex/privacy impact; environment is an immutable source, not a writable target. |
| `integrations.list` / `integrations.get` / `integrations.diff` / `integrations.status` | Administrative host-integration inventory, one installation, desired-versus-supported-versus-observed capability difference, or effective/drift/restart/health status. Views carry opaque host/installation/package/component/registration/profile refs, versions and digests, ownership/trust, cache freshness, omissions/fallbacks, and legal operations; they never expose host filesystem paths, raw host configuration bodies, environment values, or credentials. `diff` and `status` are read-shaped and cannot probe or rewrite a host; use `integrations.verify` for a fresh external probe. |
| `operations.list` / `operations.get` | Durable command/job/workflow/export/migration/automation progress, effect receipts, audit ref, retry/cancel capability, blocked reason, and explicit terminal disposition. |
| `auth.tokens.list` | Elevated owner-only enumeration of token IDs, scopes, grants, issued/expiry/last-used/revoked state and affected streams/operations; never token secrets or hashes. |
| `retrieval_anchors.metadata_batch_get` / `retrieval_anchors.resolve` / `retrieval_recipes.execute` | Batch-load bounded safe anchor metadata without content, resolve authorized records/payloads at a frozen watermark, or re-execute a versioned protected retrieval recipe with drift/coverage. These are three distinct reads; none accepts an ephemeral response handle as the sole locator. |

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
- A single `ScopeRootV2::Profile` request resolves before project discovery and binds the authenticated TraceDecay `ProfileId` directly; an optional canonical query predicate distinguishes `DeclaredScope::Profile` from `DeclaredScope::ZeroProject`, and the daemon does not initialize/open a project even when the client process is inside one. A canonical `ScopeSelectorV2` may explicitly contain authorized Profile+Project roots for a read and resolves both independently. Only migration scalar user/profile aliases mixed with compatibility project locator fields return typed `invalid_input` before catalog/store access.
- Repository, project, checkout/worktree, ref, commit/snapshot, and explicit multi-selection are distinct scope kinds. A project filter never becomes durable ownership.
- Every candidate includes opaque IDs, kind, profile, disambiguated `owner/repository/project/worktree/ref` label, path/remotes only when authorized, alias/adoption evidence, index generation, freshness, and partial/unavailable state.
- Same-name repositories/projects/branches are never merged by label. Ambiguity returns bounded candidates and a signed token; selecting one candidate retries the original canonical request in one step without retyping query/filter/time state.
- CLI, MCP, HTTP, dashboard, exports, and saved recipes use the same generated scope request/result and candidate token. Transport display may differ; resolved IDs, candidates/order, provenance, errors, coverage, and retry semantics may not.

### 9.2 Universal query, Brain, graphs, and timeline

All graph-shaped use cases return one sealed `GraphSliceViewV1` (nodes, registered edges, per-node/edge lens membership and bridge role, frontier, atlas tiles/anchor lineage, clusters, LOD, ordering, cursor, explain, coverage, snapshot/watermarks, allowed pivots); all time-shaped use cases return one `TimelineSliceViewV1` (half-open interval, density buckets or typed lanes/events, late/hidden counts, cursor, explain, coverage, snapshot/watermarks). Both travel in one `VisualizationEnvelopeV1<T>` carrying the generated visual-semantic registry ref, selection/query-delta capabilities, camera/layout hint, accessibility summary, deterministic export metadata, and the same coverage/snapshot truth. Domain lenses provide registered node/edge/lane schemas and view presets only. CLI/MCP/API/dashboard render these views through plan 21/11; no domain endpoint owns another pagination, LOD, layout, selection, table, visual-semantic, accessibility, or export transform.

| Use-case ID | Input/output contract |
|---|---|
| `query.execute` | Authorized `TraceQueryV1` to typed rows/edges/facets/aggregates/cursor/explain/coverage. |
| `query.compose_from_selection` | Accept domain `ComposeFromSelectionRequestV1` and return `ComposeFromSelectionResultV1` losslessly: canonical query/inverse breadcrumb, cost, snapshot, coverage, and supported/unsupported linked slots. No renderer-local selection or filter shape is accepted. |
| `search.universal` | Evaluated lexical/exact-phrase/fuzzy/entity/semantic/graph/recency hybrid with explicit origin/kind filters, grouping/dedupe, caps, candidate/rank explanation, and profile/corpus version; embeddings are one optional feature, never presumed beneficial. |
| `representations.artifacts.list/get/status` / `representations.generations.list` | Signed catalog versus local bytes/verification/activation/revocation, license/runtime/resource envelope, leases/pins/cache pressure, affected index generations, cold/warm status, and typed unavailable/fallback coverage from plan 05 §11.2A; never model input/vector values or raw cache paths. |
| Search benchmark launch | There is no `search.benchmark.evaluate` use case. Explorer drafts and runs a bounded generic `LabKindV1::SearchQuality` experiment over the frozen benchmark corpus, then renders its ordinary run/cell/comparison/report reads. |
| `entities.batch_get` | Bounded inspector hydration for canonical IDs, evidence, provenance, authorized payload slices. |
| `brain.overview.get` | First-scan claim; focal project/workflow/agent plus initiative/plan/task/attempt/blocker/lease/acceptance clusters; aligned work and domain activity; health strip; feedback loop; unfinished work/resume queue; source watermarks. |
| `brain.lens.get` | One bounded domain `GraphCompositionSpecV1` (primary lens, at most two overlays, explicit bridge kinds) lowered to the shared graph slice; illegal or over-budget compositions return typed alternatives, never a flattened mixed graph. |
| `brain.atlas.tiles.get` / `brain.cluster.expand` | Versioned profile-atlas tile pyramid with zoom bands/hysteresis/prefetch neighbors, stable geometry/label priority/parent-entry anchors, generation lineage, aggregate→canonical identity, membership/counts, child cursor, denominator, sampling, algorithm/layout version. |
| `graph.neighborhood.get` / `graph.path.get` / `graph.subgraph.get` / `graph.diff.get` | Bounded evidence-filtered neighborhood/path/query-driven subgraph/frozen comparison with confidence, redacted frontier, stable ordering, exact snapshot identities, and legal relation schemas. |
| `graph.impact.get` / `graph.affected_tests.get` | Direct versus inferred impact with algorithm/evidence and source snapshot. |
| `timeline.density.get` / `timeline.events.get` | Bounded buckets or event lanes with hidden/late counts, half-open interval, LOD and cursor. |
| `timeline.as_of.get` | Known state at valid and observed time: scope, context, facts, policies, catalog, goals, delivery, coverage. |
| `timeline.follow_agent` / `timeline.compare` | Stable agent/subagent lanes or aligned sessions/agents/branches/models/policies/time ranges with anchors. |
| `timeline.replay_frames.get` | Consequential frame stream with current Turn/event, previous/next anchors, before/after state, graph delta, code/diff refs, collaborator changes, impact wake, fidelity, and substitutions for one synchronized playhead. |
| `timeline.derived_lane.get` | Compile one compatible canonical query result into a bounded event/interval/counter lane with grouping, snapshot, coverage, anchor, and recipe; it never creates client-owned event order or counts. |
| `activity.events.get` / `activity.facets.get` | Consequential cross-domain activity and project/domain/actor/kind/health facets over the same event/timeline model, with routine-noise hidden counts and live/frozen coverage; no UI-side merge. |
| `coordination.presence.get` / `coordination.nearby.get` / `coordination.overlaps.get` | Expiring evidence-bearing presence/work claims and nearby-agent overlap across the same or parallel worktrees, refs, files, symbols, tests, goals, and review/delivery surfaces; includes safe compact summary plus research anchors/recipes. |

Brain, graph, timeline, search, impact, and Explorer accept domain `ScopeSelectorV2` unchanged and return `ScopeResolutionV2`. Every node/edge/row retains repository and snapshot identity; same-name symbols/files/refs never collapse without canonical entity lineage. Cross-repository edges require registered dependency/session/workflow/Git/evidence relations, and each shard contributes explicit provenance/freshness/coverage rather than a synthetic global timestamp.

Graph-of-graphs selection is one application composition over the domain-owned lens vocabulary:

```rust
pub struct InvestigationSelection {
    pub scope: ScopeSelectorV2,
    pub time: InvestigationTime,
    pub selected: Option<VisualSelectionV1>,
    pub pinned: Vec<EntityRef>,
    pub graph: GraphCompositionSpecV1,
    pub snapshot: SnapshotMode,
    pub lod: LevelOfDetail,
}

pub struct VisualizationEnvelopeV1<T> {
    pub data: T,
    pub visual_semantic_registry: RegistryManifestDigest,
    pub snapshot: SnapshotManifestRefV1,
    pub coverage: CoverageReportV1,
    pub interaction_schema: SchemaRef,
    pub camera_layout_hint: Option<PayloadRef>,
    pub accessibility_scene: AccessibilitySceneV1,
    pub export_manifest: ManifestId,
}

pub struct AccessibilityNodeId(pub u64); // scene-local deterministic identity, not registry/domain entity identity

pub struct AccessibilityNodeV1 {
    pub id: AccessibilityNodeId,
    pub parent_id: Option<AccessibilityNodeId>,
    pub role: RegistryEntryId,
    pub label: SinkEligible<LogSafeText>,
    pub logical_position: u32,
    pub logical_set_size: u32,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub relation_ids: BoundedVec<RelationId, 128>,
    pub action_ids: BoundedVec<UseCaseId, 32>,
}

pub struct AccessibilitySceneV1 {
    pub root_id: AccessibilityNodeId,
    pub nodes: BoundedVec<AccessibilityNodeV1, 5_000>,
    pub visible_measure: AggregateMeasureV1,
    pub hidden_measure: AggregateMeasureV1,
    pub coverage: CoverageReportV1,
    pub truncated: bool,
    pub continuation_anchor: Option<RetrievalAnchorId>,
}

pub enum AggregateMeasureV1 {
    Exact { value: u64, denominator: Option<u64> },
    Sampled { estimate: u64, sampled: u64, denominator: Option<u64>, uncertainty_micros: u64 },
    Capped { lower_bound: u64, cap: u64, denominator: Option<u64> },
    Unknown { reason: ReasonCode },
}

pub struct BrainEdgeMeasureV1 {
    pub relation_kind: PredicateId,
    pub evidence_class: EvidenceClass,
    pub measure: AggregateMeasureV1,
    pub coverage: CoverageReportV1,
}

pub struct AtlasLayoutRefV1 {
    pub algorithm: RegistryEntryId,
    pub version: ComponentVersion,
    pub seed_digest: ManifestDigest,
    pub anchor_microunits: Option<(i64, i64)>,
}

pub struct BrainTileViewV1 {
    pub id: ProfileAtlasTileId,
    pub kind: RegistryEntryId,
    pub label: SinkEligible<LogSafeText>,
    pub membership: AggregateMeasureV1,
    pub activity: BTreeMap<RegistryEntryId, AggregateMeasureV1>,
    pub edge_measures: BoundedVec<BrainEdgeMeasureV1, 128>,
    pub coverage: CoverageReportV1,
    pub hidden_children: AggregateMeasureV1,
    pub expandable: bool,
    pub expansion_anchor: Option<RetrievalAnchorId>,
    pub layout: AtlasLayoutRefV1,
}
```

`AccessibilityNodeId` is deterministically derived from the sealed scene snapshot, composition slot, logical path, and subject identity and is unique only within that scene. Registry IDs remain role/semantic/action vocabulary; they never stand in for dynamic node identity. Rebuilding the same scene is byte-stable, while a changed snapshot may publish a new scene-local ID set without minting canonical entities.

`GraphSliceViewV1` and `TimelineSliceViewV1` remain the sole data payloads inside this envelope. The client may store camera/composition preferences but cannot rewrite server semantics, coverage, query deltas, atlas anchors, or accessibility counts. `ReplayFrameViewV1` is a registered timeline payload/preset over the same envelope and pagination, not another event API.

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

### 9.3 Sessions, messages, turns, agents, and orchestration observations

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
| `orchestration_observations.list` / `orchestration_observations.get` | Read-only `OrchestrationObservationV1` views of provider-native Claude workflow runs, Codex goals, TraceDecay automations, and Hermes-style curation agents with native semantics and shared relations. These observations never acquire native dynamic-workflow identity or execution authority. Plan 32 exclusively owns `workflows.*` definition/version/run/node reads and commands. |

Plan 32 adds native dynamic-workflow handlers to the same generated application registry after its reconciliation gate. In particular, `workflows.runs.history_page.get` is one read-only handler over `WorkflowHistoryPort`: it authorizes the run, creates or verifies `WorkflowHistorySealV1`, and returns one bounded `WorkflowHistoryPageV1` whose opaque cursor is bound to frozen history/tape maxima and the access digest. HTTP, SDK, MCP resources, CLI, and Studio call this handler; none reads workflow event rows or reconstructs a command tape independently. It is separate from the live generic subscription and has no mutation, resume, replay, or effect authority.

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
| Code discovery | `code.search_symbols`, `code.find_exact_symbol`, `code.grep`, `code.context`, `code.files`, `code.callers`, `code.callees`, `code.call_path`, `code.impact`, `code.affected_tests`, `code.test_map`, `code.health`, `code.diagnostics`, `code.diagnose_result`, `code.move_symbol.inspect`. The inspect use case is read-shaped and cannot write or mint edit authority. |
| Git semantic tools | `git.branches.list`, `git.branches.search`, `git.branches.diff`, `git.pr.context`, `git.changelog`, `git.commit.context`, `git.sessions_for`, `git.workflows_for`. Each result states local indexed ref/merge-base/generation/watermark and fallback state. A corrupt/missing optional diagnostics, test-annotation, session, or workflow enrichment returns healthy direct Git/diff context with typed component-level partial coverage and a rebuild anchor; it never aborts the whole answer (FM-117). |
| Delivery truth | `delivery.repositories.get`, `delivery.pull.get`, `delivery.checks.list`, `delivery.reviews.list`, `delivery.releases.list`, `delivery.reconcile`. Live state carries provider/fetched-at/ETag/base/head/cap/coverage separately from local semantic state. |
| Knowledge | `knowledge.facts.list/get/search`, `knowledge.entities.list/get`, `knowledge.trust.history`, `knowledge.conflicts.list`, `knowledge.retrieval.history`, `knowledge.feedback.history`, `knowledge.deletion_impact`. |
| Automation | `automation.jobs.list/get`, `automation.scheduler.status`, `automation.dirty_scopes.list`, `automation.admissions.list/get`, `automation.runs.list/get`, `automation.artifacts.get`, `automation.candidates.list/get`, `automation.decisions.list/get`, `automation.effects.list/get`, `automation.outcomes.list/get`, `automation.recoveries.list/get`, `automation.history.list`, `automation.skills.list/get`, `automation.workflow_graph.get`. Dirty/admission views expose exact job/scope/dependency watermarks, coalesced delta, quiet/backoff time, input-digest dedupe, skip reason, and model/tool work avoided. Imported V1/provider proposals, approvals, and applies appear only as labeled historical records in `automation.history.list`. |
| Context Scout | `scout.status.get`, `scout.runs.list`, `scout.runs.get`, `scout.envelopes.list`, `scout.envelopes.get`, `scout.decision.explain`, `scout.evaluation.get`. These are authorized, cursor-bounded views over worker/model/tool/host/config/coverage status, phase receipts, addressed envelope lifecycle, deterministic suppression/dedupe explanation, and frozen evaluation evidence. They never expose model transcripts, claim or deliver an envelope, mutate counters, or replay through a Scout-local path. |
| Research provenance | `research.manifests.list/get`. Manifest reads expose immutable `ResearchAnchorId` entry identity plus each entry's nonempty canonical `RetrievalAnchorId` references; the sole anchor metadata/resolution/recipe operations are inventoried once in §9.1. |
| Search evaluation | `retrieval.corpus_versions.list/get`, `retrieval.qrel_versions.list/get`, `retrieval.candidate_pools.list/get`, `retrieval.judgments.list/get`, `retrieval.adjudications.list/get`, `retrieval.evaluation_reports.list/get`, `retrieval.profiles.list/get`; Search Quality runs are generic experiment/run/stage/comparison reads filtered by `LabKindV1::SearchQuality`. Every read names owner, immutable version/cutoff, sanitization state, source/index/model/config/catalog watermarks, coverage, and authorization; protected rationales or examples require an eligible payload policy and never enter list metadata. |
| Tasks/orchestration | `initiatives.list/get`, `initiatives.graph`, `plans.list/get/diff`, `work_items.list/get/query`, `work_items.context`, `work_items.dependencies`, `attempts.list/get/timeline`, `task_offers.list/get`, `context_packets.list/get`, `task_notifications.list/get`, `executors.list/get/match`, `scheduler.status/explain`, `task_graph.status/doctor/events`, `task_graph.edit_bundles.get/validate/diff`. Plan 24 §9.1 and §4.12 own semantics and module files: reads use read ports only, offer reads are registration-scoped, packet reads are attempt-scoped, `executors.match` is read-only, and `task_graph.events` is a subscription read-model kind under `subscriptions.create`, never a second stream vocabulary. Edit-bundle reads return opaque workspace/operation state, source-span diagnostics, candidate digest, semantic graph/active-attempt impact, expiry, and cleanup state; large validation/diff may use generic operation staging but never publishes a plan version or mutation token. Task saved views are filtered `saved_views.list/get` results from the shared row below. |
| Hints and policy | `hints.evaluations.list/get`, `hints.outcomes.get`, `hints.opportunities.get`, `policy.bundles.list/get`, `policy.coverage.get`. |
| Accounting | `accounting.usage.get`, `accounting.costs.get`, `accounting.savings.get`, `accounting.adoption.get`, `accounting.denominators.get`. Unknown/capped denominators are typed, never zero. |

Current Git tools are not hidden behind the generic query alone: their stable use cases remain catalog aliases so hint routing can recommend them. `delivery.reconcile` refuses to combine semantic impact with live PR/check claims when head/base/merge-base/changed-file digest drift; it returns `RefreshLive`, `ReindexLocal`, or `RecomputeBoth` as an explicit next action.

### 9.5 Saved investigations, exports, subscriptions, and labs

| Use-case ID | Contract |
|---|---|
| `saved_views.list/get`, `collections.list/get`, `annotations.list/get` | Authorized profile content storage; sensitive literals never enter catalog or URL-safe summaries. |
| `exports.get`, `exports.list` | Job state, frozen watermark, parts, hashes, counts, redaction, completeness, expiry; bytes served by API after authorization. |
| `subscriptions.create` / `subscriptions.revoke` | Create authorizes a query/read-model request, captures a snapshot, and returns `SubscriptionId` plus a finite replay contract. Revoke is an idempotent principal-bound command that terminates delivery, releases replay/snapshot pins, and appends an audit receipt; transport disconnect alone is not revocation. |
| `experiments.draft_from_selection`, `experiments.list/get`, `experiment_runs.list/get`, `experiment_cells.list/get`, `replay_stages.list/get`, `replay_comparisons.list/get`, `replay_comparison_cells.list/get`, `replay_reductions.list/get` | Draft is a read-shaped, nonpersisting domain `VisualSelectionV1`→typed spec/source-backlink operation. `replay_stages.list` requires a cell and returns exact `ReplayTraceV1`; stage get returns one stage. Other reads expose the immutable branch DAG, operation-backed run cohort, typed coordinates/cells/alignment/reductions, every canonical anchor, actual fidelity/substitutions/coverage, terminal and side-effect receipts. Reads never execute an evaluator or mutate live metrics. |
| `experiments.evaluator_catalog.get` | Generated `LabKindV1` input/parameter/stage/output schemas, source-selection compatibility, cost/capability policy, and dashboard/CLI/MCP/API bindings used by universal Fork to Playground; there is no feature-local lab router. |

One internal evaluator registry covers Hint, Retrieval, Search Quality, Coordination, Orchestration, Ingest, Query, Correlation, Scheduler, Memory, Policy Diff, Evolution, Scope/Federation, and Privacy. The generic experiment runner validates the tagged input/parameter schema, then calls the selected pure evaluator with read-only archive ports. The evaluator may emit typed `ReplayStageV1` records and explanations but cannot own run state, cancellation, scheduling, comparison alignment, anchors, manifests, resource grants, or persistence. Evolution preserves Hermes-style self-improvement as ordinary evidence-bearing actors, goals, turns, tools, artifacts, skills, memories, autonomy decisions, automatic applies, uses, outcomes, revisions, automatic recoveries, archives, and deletions. Simulation is an experiment and never mutates live state; it returns changed decisions/tool routes/outcomes, regressions/wins only where labels exist, unknown horizons, privacy exclusions, and cost/latency deltas. The live autonomous worker does not wait for it.

## 10. Complete Command Use-Case Inventory

Every non-curation mutation has an explicit typed execution contract; destructive/irreversible operations use a separately named preflight and confirmed domain command when required. Curation is deliberately different: it is fully autonomous under versioned configuration and emits no per-item preview/approve/apply/rollback commands. The catalog marks autonomy, authorization, expected versions, side effects, monitoring/recovery behavior, job behavior, and audit.

| Domain | Stable command use cases |
|---|---|
| Projects/indexing | `projects.register`, `projects.update_alias`, `projects.unenroll`, `index.refresh`, `index.pause`, `index.resume`, `watchers.start`, `watchers.stop`. Unenroll returns an operation-specific retained-evidence impact/confirmation and never deletes content implicitly. |
| Runtime/daemon/update | `daemon.start`, `daemon.stop`, `daemon.drain`, `daemon.restart`, `runtime.update.plan`, `runtime.update.start`, `runtime.update.recover`. Drain/update are durable workflows carrying lifecycle lease epoch, accepting/draining/stopped state, in-flight work, checkpoint/receipt, restart requirement, takeover, recovery artifact, and current client-binding version. |
| Diagnostics/repair | `diagnostics.refresh`, `doctor.run`, `repair.inspect`, `repair.start`, `backup.create`, `backup.restore`. Inspect is read-only; start is a confirmed resumable lifecycle-fenced workflow with exact repair kind/input version. Restore is a durable workflow with preflight and recovery point. |
| Database authority | `storage.authority.get`, `storage.isolation.get`, `storage.isolation.verify`, `storage.integrity.get`, `storage.snapshots.list/get/create/verify`, `storage.checkpoint.request`. Reads return daemon/authority epoch, `StoreIsolationStatusV1`, safe health/watermark and receipt IDs without paths. Mutations are operator-only daemon workflows; checkpoint/snapshot never accept a path or method override and always use plan 02's consistent-snapshot coordinator. No use case returns SQL, file handles, page/WAL bytes, keys, or raw backups. |
| Host integrations | `integrations.install`, `integrations.update`, `integrations.repair`, `integrations.uninstall`, `integrations.verify`. All require the administrative host-integration grant, an opaque target or installation reference, expected desired/observed/manifest versions, and an idempotency key. They return `OperationRef` immediately and run through one durable lifecycle; no command accepts a host path, raw configuration body, arbitrary component manifest, or credential value. |
| Store administration | `storage.consolidation.inspect`, `storage.consolidation.plan`, `storage.consolidation.start`, `storage.consolidation.status`, `storage.consolidation.resume`, `storage.consolidation.recover`. This is the operator-only merged-#425 workflow for two nonempty profile shards: fail closed on unsupported/path-or-file-identity holders, freeze/reserve both sources, back up, stage, verify every table/artifact/disposition, cut markers atomically, and return exact recovery. `start` requires the recomputed deterministic confirmation token and administrative grant. It never runs from scheduler, task execution, Settings auto-save, or autonomous curation. V1 `preview/apply` names remain only in the compatibility adapter/inventory. |
| Shared Brain administration | `brain.join/leave`, `brain.nodes.rotate/revoke`, `brain.placements.plan/apply/verify`, `brain.sync.run/pause/resume/repair`, `brain.replicas.seed/verify/retire`, `brain.backup.verify`, `brain.failover.plan/promote/verify`, `brain.repositories.adopt/split`. Plan 28/08 own the closed family. `join` owns enrollment plus initial-placement compensation; there is no public `nodes.enroll` twin. Every mutation is operator-scoped, expected-version/idempotency bound, placement/authority-epoch fenced, audit-bearing, and resumable where external effects exist. No command accepts a database URL/path or makes Tailscale mandatory. |
| Context Scout | `scout.feedback.record`, `scout.runtime.pause`, `scout.runtime.resume`, `scout.runtime.cancel`. Feedback appends bounded explicit evidence against an exact envelope; pause/resume stop or restart optional work only at safe boundaries; cancel targets one active run and never deletes an envelope. Historical/current-best-effort replay remains the generic Hint experiment family and no `scout.replay.*` command exists. |
| Capture/LCM | `capture.refresh`, `capture.ingest`, `capture.pause`, `capture.resume`, `lcm.compress.plan`, `lcm.compress.start`, `lcm.boundary.create`, `lcm.lifecycle.preflight`, `lcm.lifecycle.repair`. `capture.refresh` starts or joins the daemon-owned provider/source freshness operation and returns its shared `OperationRef`; it scans each source once and never copies canonical transcript bodies per project. `capture.ingest` is the narrow authenticated broker submission path, not an interactive catch-up alias. Source offsets advance only through capture/store receipts. |
| Automations | `automation.jobs.create/update/delete`, `automation.run`, `automation.cancel`, `automation.pause`, `automation.resume`, `automation.scheduler.enable/disable`. Run revalidates dirty generation, dependency/input digest, policy/config/activity/lease before fenced acquisition; a due or run-now request cannot bypass unchanged-input admission. |
| Autonomous curation | `curation.run_now`, `curation.pause`, `curation.resume`, `curation.status`, `curation.history`, `curation.pin`, `curation.protect`, `curation.exclude`, `facts.feedback`. `run_now` evaluates current dirty scopes immediately but still skips an identical terminal input; unchanged historical trials use Evolution/Memory experiments. Candidate create/update/supersede/archive/quarantine and owned skill validate/materialize/revise/recover are internal autonomous effects, not public per-item commands. Each records artifact/evidence/validation/config/policy/expected-version/staged-monitoring/outcome receipts; foreign-owned targets are skipped. Explicit administrative deletion remains the separate descendant/hold/index/blob workflow. |
| Policy | `policy.publish`, `policy.activate`, `policy.rollback`. Exact artifact validation and immutable registry CAS are required; activation never changes an in-flight evaluation. |
| Representation artifacts | `representations.artifacts.install/import/activate/deactivate/evict/verify`, `representations.generations.rebuild`. Plan 05 §11.2A/PR 14E owns lifecycle semantics. Commands pin signed manifest/digest/license/runtime/config, enforce allowlisted egress and disk/RAM/device budgets, stage/verify before publish, preserve active/replay/index pins, and emit operation/audit receipts; query execution never invokes them. |

### Optional semantic-code representation and rerank orchestration

`search.universal` and `code.search_symbols` are the only search use cases. Their qualified profile may request optional native semantic candidates from FastEmbed using one exact activated benchmark-promoted embedding artifact (`JinaEmbeddingsV2BaseCode` is the primary candidate and `GTELargeENV15Q` the required comparator) and may request an independently activated `BGERerankerV2M3` artifact; no `fastembed.*`, `semantic_code.*`, or provider-specific operation is added. Before a labeled Search Quality benchmark promotion receipt exists, the semantic feature is disabled by default. Strict semantic mode returns typed `representation_unavailable`, `generation_incompatible`, `rebuild_required`, deadline, or resource errors. Non-strict mode returns the byte-stable lexical candidate order and records that semantic/rerank stages were absent; it never silently selects another model or perturbs lexical ties.

Application resolves and returns four independent facts for each stage: desired configuration, activated verified artifact/generation, effective request route after policy/capability/budget checks, and observed daemon runtime/model/device execution. Reads through `representations.artifacts.list/get/status` and `representations.generations.list` expose exact model, digest, cache verification/offline readiness, CPU/device/thread/batch/RAM/disk/residency envelope, generation/rebuild status and coverage, stage latency/RSS/cache/vector/index coverage, and provenance without cache paths or vector values. Install/import/download consent and verification continue through `representations.artifacts.install/import/verify/activate`; rebuild continues through `representations.generations.rebuild`. The daemon/root representation runtime exclusively creates and owns native FastEmbed sessions and model residency; application, query callers, HTTP, CLI, MCP, SDKs, and the browser never load a model or inspect storage directly.

Reranking is a separate toggle/profile. Its candidate top-N defaults to 25 and can never exceed 25. In addition to native BGE reranking, a separately registered optional Codex Spark/app-server-style rerank capability may be requested only when discovery evidence, credential reference, privacy/egress policy, explicit model, cost/token/deadline, and top-N budgets all resolve. It is never required or default, never embeds code, and never substitutes for the promoted FastEmbed embedding or native BGE reranker. Every attempt records requested and actual route/model plus cost, tokens, deadline, candidate count, outcome, and fallback. Unavailable, denied, timed-out, or malformed model-assisted output preserves the exact pre-rerank order. Search Quality Lab uses the existing experiment/replay family for lexical versus native semantic versus native rerank versus model-assisted rerank ablations; plan 22 may consume the same registered capability for active hinting/scout, but it cannot bypass these application contracts.
| Settings | `settings.profile.patch`, `settings.project.patch`, `settings.integration.patch`, `settings.automation.patch`, `settings.storage.patch`. Inline validation shows declared owner, source/default, restart/reindex/privacy/migration impact before the direct expected-version save; environment-derived values are read-only and storage relocation is a separate durable workflow, never an arbitrary path write. |
| Payload/privacy | `payloads.gc.plan`, `payloads.gc.start`, `retention.run.plan`, `retention.run.start`, `holds.create`, `holds.release`, `entities.retire.plan`, `entities.retire.start`, `privacy.scan.start/cancel`, `privacy.remediation.plan/start/verify`, `privacy.quarantine.hold/release`. Privacy commands use safe finding/scan IDs, elevated grants where required, durable jobs, and candidate-free audit receipts. |
| Projections/migration | `projections.rebuild`, `projections.pause`, `projections.resume`, `projections.publish`, `projections.rollback`, `migrations.backfill`, `migrations.reconcile`, `migrations.cutover`, `migrations.rollback`. |
| Delivery refresh | `delivery.refresh`. Read-only remote fetch into captured evidence; repository allowlist, credential capability, rate/cap state, and fetched revision are audited. No PR write command. |
| Saved views and investigations | `saved_views.create/update/delete`, `saved_views.share.plan/start/revoke`, `collections.create/update/delete`, `annotations.create/update/delete`. `SavedViewDefinitionV1::{Investigation,Task,Experiment}` shares this exact lifecycle; plan 11 owns investigation/experiment-view validation and plan 24 owns task-spec validation. Protected content stays with its declared activity/project owner; sharing creates a separately authorized, redacted, expiring local published view readable through `saved_views.get`, never publishes remotely, and never copies source content into catalog metadata. |
| Research provenance | `research.manifests.create_version`. Appends one immutable successor after validating owner, expected predecessor/version, classification and secret-scan receipts, and every manifest entry's nonempty canonical `RetrievalAnchorId` set; it never creates a parallel evidence locator or resolves `ResearchAnchorId` directly. |
| Search evaluation | `retrieval.corpus_versions.create/freeze`, `retrieval.qrel_versions.create/freeze`, `retrieval.candidate_pools.create`, `retrieval.judgments.record/supersede`, `retrieval.adjudications.record`, `retrieval.evaluation_reports.publish`, `retrieval.profiles.publish/activate`; Search Quality run/cancel/resume/retry/minimize uses the generic experiment row below and all sanitized evaluator-fixture promotion uses the one `experiments.fixtures.promote` row. These are direct expected-version, idempotent commands over immutable/superseding artifacts: freezes never rewrite members; supersession retains the prior judgment; adjudication retains every source label; report publication exposes only aggregate/redacted output; profile activation revalidates locked promotion gates and never alters an in-flight query. |
| Agent coordination | `coordination.message`, `coordination.handoff`, `coordination.ack`, `coordination.suppress`. Every direct/resumable command targets one presence/overlap claim and stable anchor, checks host/agent capability and expiry, returns the inline disclosed summary/effects with its receipt, records delivery/acceptance separately, and cannot mutate another agent's state without an authorized provider action. |
| Tasks/orchestration | `initiatives.create/update/pause/resume/retire`, `plans.create_version/activate`, `plans.decompose`, `work_items.create/update/replace/retire`, `work_items.link/unlink`, `work_items.assign/reassign/assign_set`, `work_items.pause/resume/cancel/reopen/archive`, `work_items.record_attestation/record_review/record_decision/record_exception/handoff/reverse_transition`, `work_items.retry`, `task_offers.accept/decline/revoke`, `attempts.heartbeat/progress/complete/block`, `context_packets.accept`, `task_notifications.create/update/delete`, `executors.register/heartbeat/drain/unregister`, `scheduler.pause/resume/run_once`, `task_graph.edit_bundles.export/rebase/submit/delete`. Plan 24 §9.2, §4.5A, and §4.12 own semantics and module files; every mutation is a POST command-envelope use case (plan 10 §8.7). Review application semantics use the active PlanVersion as sole authority, atomically terminalize every accepted negative with preferred/fallback recovery, enforce one failed-predecessor successor CAS, linear correction-head CAS, typed immutable anchors, deterministic combined-review decomposition, and return the sealed lineage/validity/remediation view; no application-local review head or renderer evaluation exists. Edit export freezes an explicit selection/base/pins, rebase creates a successor workspace rather than rewriting the stale one, submit performs parse/validation/diff/secret scanning outside the transaction then CAS-validates and publishes the bounded normalized owner-shard mutation set atomically, and delete purges staging only. Omission never deletes, local keys allocate canonical IDs only inside the successful transaction, and no action is named preview/apply/rollback. Attestation/review/decision/exception/handoff commands append typed evidence and never set readiness or acceptance directly; `reverse_transition` is a registered optimistic compensating transition with exact prior/new versions and a receipt, while `reopen` creates a new work-item version and never reopens a terminal attempt. `task_offers.accept` is the sole public execution-admission command: it CAS-checks the offer/readiness and invokes the single internal transaction that creates the sealed packet/attempt/lease set atomically. Attempt lifecycle commands require that fence, packet acceptance is fenced at a safe Turn boundary, and notification subscription changes are direct expected-version commands. `work_items.assign_set` is one bounded all-or-none owner-shard transaction with plan/item expected versions and per-item receipts. No task command creates advisory `WorkClaimV1`. A task view is created, updated, read, and shared only through the shared `saved_views.*` row above using `SavedViewDefinitionV1::Task(TaskViewSpecV1)`; task-specific validation/lenses remain plan-24-owned, and no parallel task-view command namespace exists. |
| API tokens | `auth.tokens.create`, `auth.tokens.revoke`. Audited commands mint and revoke the scoped/TTL/revocable tokens of plan 17 §18.2; creation returns the secret exactly once through the secure flow, storage keeps only the hash and token ID, revocation declares stream/operation implications, and the per-launch bootstrap bearer (plan 10 §10.2) may execute only `auth.tokens.create` for the initial admin-class token. Token listing is the owner-only read use case above, never a mutation command. |
| Exports | `exports.create`, `exports.cancel`, `exports.delete`. Create freezes query/access/redaction, stages parts under profile export root, and publishes only after final manifest hash. |
| Experiments and replay | `experiments.create`, `experiment_runs.create/cancel/resume/retry/minimize`. Create freezes one typed `ExperimentSpecV1`; a changed spec or branch creates an immutable successor/child through the sole `ExperimentBranchRefV1` rather than editing its parent. One run operation owns a bounded cell cohort; cancel/resume/retry/minimize use the generic operation state/receipts. They may persist experiment artifacts and explicitly granted model/egress cost only, never production facts, hints, claims, leases, files, counters, caches, judgments, profiles, policies, or findings. |
| Experiment fixture promotion | `experiments.fixtures.promote`. This is the only evaluator-fixture promotion command, including Search Quality. Its typed target names evaluator/fixture registry destination rather than a path; it requires reviewed sanitized/redacted payload, secret-scan receipt, exact source experiment/run/cell/manifest, explicit confirmation, durable promotion receipt, and repository-write capability outside the hermetic evaluator runtime. |
| Code edits | `code.move_symbol.commit`. It consumes the digest/version from the separate read-shaped `code.move_symbol.inspect`, requires repository/worktree grant and typed confirmation, revalidates both endpoints, performs the destination-first write with source recovery receipt, schedules reindex, and never rewrites callers implicitly. |

V1 writable dashboard actions not represented by a row above block V1 retirement. PR 3's generated inventory and the application registry jointly enforce this: each mutation has exactly one V2 use case or an explicit retired-with-replacement decision.

Scope-sensitive command rules are exhaustive and fixture-locked:

- Create/import operations for facts, skills, policies, automations, saved investigations, experiments, and annotations require explicit `DeclaredScope`; there is no “current project” fallback. Autonomous curation candidates derive scope from sealed evidence and policy rather than a public proposal command.
- Generated host bindings may supply a canonical current-invocation locator for reads, but application resolves it from the immutable provider session/workspace context on every invocation. For a fact mutation, convenience spellings such as `--scope profile` lower to exact `DeclaredScope::Profile`; missing ownership never falls back to process CWD, cached project, first workspace, last project, or host-profile name. A projectless automation may propose profile memory only through the normal evidence/classification/policy/revalidation path, and its source zero-project Turn remains linked.
- Host-integration request context carries immutable installed `HostProfileRef`/configured-owner evidence separately from per-invocation workspace and declared scope. Reinitialization, clone, reload, or session change cannot copy the previous route/home into a new request, and no host-profile identity selects a TraceDecay profile or project.
- Updates, archives, restores, and deletes resolve the canonical owner from the target entity and reject a conflicting request scope before validation or execution; autonomous curation has no item approval/apply/rollback commands.
- Cross-project reuse creates evidence relations from the original owner. It never copies a profile fact/skill/policy into a project shard or promotes project state to profile scope implicitly.
- All-scope reads may combine profile-owned and project-owned rows, but each result and command capability retains `owner`, `declared_scope`, privacy domain, and authorization state.
- Moving ownership is a named migration workflow with source/target versions, conflict checks, copy/delete receipts, rollback boundary, and no in-place owner-field edit.

### 10.1 Host-integration lifecycle boundary

Application owns the complete host-integration state machine. It authorizes every read and command; resolves canonical `HostProfileRef` and `HostInstanceId`; loads the generated desired package/component/registration profile; compares it with probed support and persisted ownership evidence; reserves idempotency; validates expected manifest/config/observation versions; advances durable operation phases; records audit/effect/compensation receipts; and decides whether retry, reconciliation, repair, restart, or terminal failure is legal. The lifecycle phases cover discovery, probe, plan, stage, validate, trust wait, install/enable, reload wait, verify, healthy/degraded, update/repair, uninstall, reconciliation, and terminal disposition without making root composition a second workflow owner.

```rust
pub enum HostDeploymentStateV1 {
    Discovered, Probed, Planned, Staged, Validated, AwaitingHostTrust,
    InstalledDisabled, Enabled, ReloadRequired, Verifying, Healthy, Degraded,
    Updating, Repairing, CompensationPending, Compensating, Uninstalling, Removed,
    CancelRequested, Cancelled, FailedRecoverable, FailedTerminal,
}

pub struct ResolvedHostPackageV1 {
    pub package_id: RegistryEntryId,
    pub bundle_payload_digest: ManifestDigest,
    pub signed_release_manifest_digest: ManifestDigest,
    pub release_attestation: EntityRef,
    pub components: BoundedVec<HostBundleComponentRefV1, 4>,
}

pub struct ResolvedHostBundleV1 {
    pub source_integration_manifest: ManifestDigest,
    pub host_profile: HostProfileRef,
    pub capability_snapshot: HostCapabilitySnapshotV1,
    pub adapter_version: ComponentVersion,
    pub packages: BoundedVec<ResolvedHostPackageV1, 4>,
    pub omissions: BoundedVec<HostComponentOmissionV1, 1024>,
    pub difference_ledger: ManifestDigest,
    pub stock_host_conformance_receipt: EntityRef,
    pub resolved_digest: ManifestDigest,
}
```

These are the sole resolved package/bundle contracts. Application creates them only after verifying plan 12's signed release manifest and release attestation, then binding that immutable payload to a current `HostCapabilitySnapshotV1` whose subject is the matching `Installed` runtime. A pre-install `Target` snapshot can drive diff/install planning but cannot create a resolved installed bundle. Neither plan 08's pure compiler nor root's effect adapter may construct one.

Every effectful state has explicit failure and cancellation edges. A pre-effect cancellation reaches `Cancelled`; after any uncertain/committed host effect it enters `CompensationPending` or reconciliation, never claims cancellation complete early. A retryable fault reaches `FailedRecoverable` with the same operation/idempotency/input and a typed retry/resume directive; retry resumes from the last verified receipt after revalidating desired/observed generations. Exhausted, incompatible, unsafe, ownership-conflicted, or uncompensatable faults reach `FailedTerminal` with repair/manual-reconciliation evidence. Compensation may restore only a verified receipt-owned snapshot and exits to the prior verified state, `Cancelled`, `FailedRecoverable`, or `FailedTerminal`. Generic `operations.cancel|resume|retry` drives these edges; no integration-specific rollback command exists.

Root composition supplies one narrow `HostDeploymentPort` with only typed `probe`, `stage`, `apply_owned_delta`, `request_reload`, `verify`, `restore_owned_snapshot`, and `remove_owned_delta` calls. Each call consumes a generated manifest plus opaque target/component refs and returns sanitized capability observations and effect receipts. The port may use host CLIs, files, caches, or configuration APIs internally, but application views, events, errors, HTTP/SDK/MCP payloads, and audit summaries receive no raw path, file body, command line, environment, or credential. Foreign/unknown ownership is an application-visible blocked state, never permission for an adapter to overwrite.

`integrations.install|update|repair|uninstall|verify` share `OperationKernelV1`, one operation-kind family, and the same polling/recovery semantics as `operations.get`; a killed process resumes from the last receipted step. An identical idempotency retry returns the original operation, a changed request under the same key conflicts, and an uncertain host effect enters typed reconciliation before any retry. Configuration writes express desired state only: they do not call `HostDeploymentPort` or claim effective state until the authorized integration operation probes and acknowledges it.

The application owns one non-flattened matrix cell used by every difference view:

```rust
pub struct HostIntegrationDifferenceRowV1 {
    pub capability: CapabilityId,
    pub desired: HostDesiredCapabilityStateV1,
    pub support: HostCapabilityDispositionV1,
    pub installed: HostInstalledCapabilityStateV1,
    pub observed: HostObservedCapabilityStateV1,
    pub effective: HostEffectiveCapabilityStateV1,
    pub evidence: BoundedVec<RetrievalAnchorId, 16>,
    pub reason: Option<ReasonCode>,
    pub legal_actions: BoundedVec<RegisteredDiagnosticActionKind, 8>,
}
```

`desired`, documented support, physical installation, latest probe observation, and effective usable state are independent axes. `HostCapabilityDispositionV1` is used unchanged—especially `Undocumented`, `PolicyDisabled`, `Stale`, and `TrustPending`; no transport renames it to a generic `Unknown` or mixes it with desired/installed/effective lifecycle state. The other four state enums are closed application view types generated into plan 17 clients. A cell is healthy/effective only when all required axes and current authorization agree.

## 11. Orchestration Rules for Key Product Flows

### 11.1 Brain and graph-of-graphs

`brain.overview.get` captures one authorized scope and vector watermark, then requests bounded rollups, health, active-workflow summaries, feedback outcomes, and a focal lens. Components may finish partially; the response retains component coverage rather than failing the whole Brain. It does not open every project shard: catalog statistics and All rollups select candidate shards, and expansion is explicit.

`brain.lens.get` calls query graph/time operators with one validated `GraphCompositionSpecV1`, resolves the current profile-atlas generation/tile viewport, then batch-hydrates inspector references. Nodes and edges retain primary/overlay membership; cross-lens bridges carry `RelationAssertionV1`, evidence class, confidence, producer/version, supporting events/observations, and validity. Ordinary snapshot refresh updates evidence inside stable atlas geometry; generation changes emit anchor lineage. Temporal adjacency alone is never exposed as causation.

### 11.2 Session/agent investigation

One Causal Loom request composes density, lane events, Turn hubs, agent tree, tool results, code/Git/delivery evidence, knowledge/policy/automation links, and impact ribbon at the same frozen watermark. Missing project/Git/reasoning data creates lane coverage markers. Follow-agent retains collaborator and delivery context through bounded relation traversal; it does not filter away parent/subagent causation anchors.

When the captured session/source watermark is below the caller's requirement, this read returns the available investigation plus typed stale/partial coverage and the legal freshness operation descriptor. Starting refresh is a separate command; the resulting operation is visible in the Loom/Observatory, cancellable at committed source boundaries, and reusable by every transport. Completion invalidates or advances later snapshots but never mutates the already returned read.

### 11.2A Experiment and replay execution

`experiments.draft_from_selection` validates `VisualSelectionV1` against the evaluator catalog and returns a nonpersisted typed draft plus source backlink. `experiments.create` authorizes the source anchor/scene, resolves and freezes exact input/version/environment/privacy manifests, validates one-to-six variants, explicit sweep values, optional branch ancestry, and budgets, expands the complete checked coordinate product, estimates cost/egress, rejects overflow or a total above the spec/hard cell caps, and writes an immutable `ExperimentSpecV1`. `experiment_runs.create` repeats that expansion against the pinned corpus/evaluator manifests before admitting one generic operation whose bounded `ExperimentCellV1` cohort covers every selected variant × evaluator × corpus case × repetition × sweep coordinate. Its steps are manifest verification, hermetic worker launch, cell/evaluator stages, trace alignment/evaluation, side-effect verification, artifact publication, and terminal receipt. Cells reuse the application scheduler/operation kernel with bounded concurrency; they do not own another operation, job system, heartbeat, or cancellation state. `experiment_runs.minimize` is a bounded child operation over a named failing run/cell/predicate and typed removable dimensions.

The replay worker protocol is versioned and digest-pinned. The broker spawns a fresh process with an empty environment, closed inherited file descriptors, no ambient credentials, read-only verified input mounts, a size-limited disposable overlay, frozen clock/RNG, and only explicit brokered model/network grants. OS/container controls enforce wall time, CPU, RSS, overlay/disk-read/network/output bytes, FD count, and process count from `ExperimentBudgetV1`; timeout/cancel kills and reaps the full process tree. Production repositories, stores, query usage counters, hint outcomes, leases/claims, live caches, configuration activation, task execution, Git writes, and external effects have no mount, descriptor, credential, or port. Every allowed open, denied attempt, usage high-water mark, broker call, and forced termination is recorded. Publication atomically fails unless the receipt digest matches, all limits hold, and `ReplaySideEffectReceiptV1.production_effect_count == 0`. Cancellation/resume/retry begins at a receipted operation step and never reruns an uncertain external model call without a recorded-output/idempotency contract. Experiment, run, cell, stage, comparison, comparison-cell, and reduction outputs receive canonical retrieval anchors.

`experiments.evaluator_catalog.get` supplies the universal selection-to-lab mapping. “Fork to Playground” accepts a canonical source anchor plus current scene/snapshot and emits a typed draft spec whose editable fields are explicit patches. Failure minimization is a bounded experiment child operation using evaluator-declared removable dimensions and a named predicate; it records its reduction tree and can produce only a sanitized fixture candidate for the separate promotion command.

### 11.2B Managed declarative task-graph editing

`task_graph.edit_bundles.export` authorizes one explicit full-plan, initiative, saved-query, or saved-view selection; freezes owner, canonical base versions, catalog/config/privacy digests, external immutable stubs, and closure mode; then emits a strict sharded Markdown/frontmatter bundle through a contained stream. The managed local CLI may materialize those bytes in a private `0700` runtime directory with `0600` files, but HTTP/MCP/SDK callers receive streams or resource links and never a server path. Frontmatter owns structured graph state; Markdown bodies own narrative only. Every editable entity carries its canonical version or an `EditLocalKeyV1`, and every destructive intent is explicit `replace`/`retire`; absence means retain.

`get` reports the opaque workspace, frozen base, limits, expiry, uploaded digest, operation, and cleanup state. `validate` performs archive safety, strict UTF-8/CommonMark plus YAML-1.2-subset parsing, schema/source-span validation, reference and graph invariants, permissions/privacy/secret checks, task gates, route/budget constraints, and active-attempt policy without canonical writes. It returns stable machine-readable diagnostic codes, precise file/range spans, related spans, and safe suggested edits. `diff` returns a typed semantic graph delta and affected active attempts rather than a line diff. `rebase` performs a three-way semantic merge against current canonical heads and creates a new workspace with explicit `TaskGraphEditConflictV1` records; it never silently accepts last-writer-wins.

`submit` binds idempotency to principal, workspace, normalized candidate digest, and key; re-runs all safety/semantic validation; checks every frozen version and active-attempt invariant; then passes one normalized bounded mutation set to a single owner-shard unit of work. Canonical ID allocation for local keys, all entity/head/relation writes, audit/outbox, idempotency receipt, and version publication commit together or not at all. Exact retry returns the stored `TaskGraphEditReceiptV1`; stale base returns structured conflicts; partial upload, expired workspace, cross-owner mutation, illegal cycle, or running-attempt route/lease mutation writes nothing. Success purges staging before reporting clean completion, while failed validation retains only through the declared retry TTL. `delete` and crash reaping are idempotent, preserve a content-free cleanup receipt, and cannot touch canonical plans.

CLI `task-graph edit start|get|validate|diff|rebase|submit|clean`, HTTP streams, optional MCP resource links/tools, generated SDK helpers, and Work UI all bind this same family. The intended agent loop is export → edit locally → validate/fix until clean → inspect semantic/active-work impact → submit once → verify receipt and cleanup. Skills teach this loop even when MCP is absent; no transport invents bulk JSON, path inference, per-file CRUD, or an alternate task model.

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

Evolution inspection/simulation runs through the generic experiment artifact lifecycle but has no live curation effect and never gates live progress. Every job version publishes an `AutomationInputContractV1` and one of `EvidenceDriven | TimeDriven | ExternalEvent | Manual`; schedule time is never an implicit input. The live automation scheduler is event-driven over `automation_dirty_scopes`; its clock tick reads only due jobs plus bounded dirty keys and never scans every thread/project/store. Typed field-level dependency selectors map new terminal Turns/messages, fact/relation/trust changes, feedback/outcomes, diagnostics/patterns, skill use/drift, retention horizons, and semantic config/policy/catalog/model changes to exact thread/project/profile jobs. Evidence, registered time-boundary ordinal, external source sequence/event, or idempotent manual request advances one typed trigger frontier and dirty generation. Irrelevant events and self-produced effects are excluded; a produced effect can dirty a downstream task only through a registered noncyclic dependency or later outcome/feedback event. An evidence-driven job becomes dormant after considered no-relevant/unchanged input or terminal `NoChange` until a relevant frontier advances. A dependency-version change reprocesses prior evidence only when the versioned reevaluation policy authorizes a bounded scope/window.

For each key, application loads the expected cursor version and current per-shard frontiers, coalesces the dirty generation, and proves scope-local quiescence from a finalized Turn/session boundary, no relevant ingress during `min_quiet`, and a sealed active-writer registry generation/frontier/freshness/coverage receipt, bounded by `max_debounce`. It then enforces minimum eligible event/token delta and seals `AutomationInputManifestV1` with trigger frontier, typed selector/contract and dependency digests, expected cursor/dirty generation, current/considered/consumed/included frontiers, boundary, predecessor, coverage, semantic effective-input digest, and distinct evaluation-snapshot digest. Unknown/stale activity or partial coverage is deferred rather than guessed idle. Immediately before launch it revalidates the entire manifest, config/policy, lease, and writer snapshot.

Policy evaluation, expected-frontier comparison, unique admitted-semantic-input claim, admission receipt, generic operation/run creation, and scheduler checkpoint commit atomically in the owner shard. `NoRelevantChange`, `IdenticalTerminalInput`, and `DependencyUnchanged` call no model/search/tool, CAS-advance only the considered frontier, and close only their expected dirty generation; they never write a consumed frontier or terminal outcome. Quiet/minimum-delta/lock/backoff/budget/pause/defer reasons advance neither considered nor consumed and retain dirty eligibility. Equivalent observations update one input-bound skip episode and shared metrics, not a row per tick. A retryable failure resumes the same operation/run/input through the generic attempt/backoff/deadline/circuit contract; deterministic poison input is quarantined. Uncertain effects leave the operation nonterminal and block retry/cursor movement until one typed reconciliation receipt proves the effect state and finalizes exactly once. Only committed effects or legitimate terminal `NoChange` atomically advance considered plus consumed frontiers and clear the run's expected dirty generation, so concurrent new activity remains pending. Combined curator/reflector/skill-writer execution may share one immutable evidence batch, but every job keeps its own admission, validation, outcome, and cursor.

After admission, the application curation worker consumes policy decisions, revalidates exact candidate/version/evidence/validation/config/privacy/ownership state transactionally, and autonomously creates/updates/supersedes/archives/quarantines/materializes eligible owned facts, memories, and skills. It monitors staged outcomes and automatically revises/recovers when thresholds fire. No `approve`, `reject`, `preview`, `apply`, or user-triggered `rollback` command exists for a curation item; operators configure policy, inspect history, pause/resume/run-now, pin/protect/exclude, or submit feedback. `run_now` shortens cadence/quiet wait for an already-dirty evidence-driven scope; on a `Manual` job it appends an idempotent request frontier as declared input. Neither form can force an identical successful input; the hermetic playground is the deliberate unchanged/historical replay surface.

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
- Command fixtures compare operation-specific inspection/confirmation, validation, durable domain effects, audit, idempotent retry, version conflict, side effects, and recovery behavior; never run a destructive parity command against live user data.
- #410 representative query behavior is a named internal V1 parity profile, never a post-cutover live mode. Native rows are the completeness authority; representative output differences require rule/version/source evidence.
- #405 adoption and #407 profile consolidation fixtures assert no duplicate scope/project/session/fact exposure and preserve migration provenance.
- Migration-only shadow dispatch is selected by versioned feature state per use case, not one global flag, and is unreachable after its cutover receipt closes. A V2 cursor/preview/subscription is never interpreted by V1.

### 12.2 Cutover order

1. Register every use case and generate catalog/schema fixtures with no executable V2 default.
2. Land read-only system/query/session vertical slice, including sanitized-native/representative messages and partial coverage.
3. Shadow read use cases and compare typed semantic results.
4. Land command kernel and no-op fixture commands; prove idempotency/version/audit/workflow recovery.
5. Move domains independently: sessions, graph/code, Git/delivery, knowledge, policy/hints, automation/skills, accounting/operations, saved/export/experiments.
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

**Files:** `src/kernel/{access,response}.rs`; `src/features/{capabilities,scopes,query,search,settings}/{queries,views,ports}.rs`; `tests/{authorization_privacy,query_coverage}.rs`.

- [ ] Add tests `catalog_default_is_materialized_before_execution`, `brain_default_is_active_profile_all`, `current_invocation_is_reported_and_never_overrides_explicit_target`, `cwd_and_last_project_never_narrow_scope`, `same_name_scope_returns_ordered_candidates`, `candidate_token_retries_original_request_once`, `scope_result_is_identical_across_cli_mcp_http`, `denies_before_scope_expansion`, `binds_access_digest_to_query`, `locked_shard_returns_metadata_coverage`, `partial_query_preserves_every_disposition`, `query_does_not_write_usage_counter`, `reasoning_requires_explicit_grant`, `settings_report_effective_source_and_owner`, `environment_setting_is_not_writable`, `foreign_doctor_finding_has_no_update_action`, and `partial_provider_is_not_healthy`.
- [ ] Add freshness regressions `query_never_triggers_provider_ingest`, `stale_query_returns_coverage_and_refresh_operation_descriptor`, `identical_refresh_requests_join_one_fenced_operation`, `joined_waiters_share_terminal_receipt_and_error`, `changed_target_watermark_does_not_false_join`, `cancel_preserves_committed_source_head`, and `provider_refresh_scans_each_source_once_for_many_project_attributions`.
- [ ] Run `cargo test -p tracedecay-application --test authorization_privacy --test query_coverage -- --nocapture`. Expected: tests fail because access/query services are absent.
- [ ] Implement authorization-first execution, `QueryAccess` conversion, response metadata propagation, deadline/cancellation, and capability/scope/query/search/entity use cases.
- [ ] Re-run the command. Expected: all tests pass; denied fixture opens zero shards; read port mutation sentinel remains zero.
- [ ] Commit `feat(application): authorize and compose federated reads`.

### PR 24A3: Native and representative message/session contracts

**Files:** `src/features/{sessions,agents}/{queries,views,ports}.rs`; `tests/message_representation.rs`; redacted #410 compatibility fixtures.

- [ ] Add tests `native_rows_preserve_retained_structure`, `representative_preserves_source_ids_and_rule`, `representative_expansion_cannot_double_count`, `direct_user_excludes_delegated_and_protocol_rows`, `unknown_origin_remains_visible`, and `native_expansion_is_cursor_bounded`.
- [ ] Run `cargo test -p tracedecay-application --test message_representation -- --nocapture`. Expected: tests fail because audience/representation contracts do not exist.
- [ ] Implement Section 9.3 use cases over query/projector classifications; representative projection must carry represented IDs, observations, algorithm version, suppression count, and expansion cursor.
- [ ] Re-run the command. Expected: exact sanitized-native fixture count/manifest digest matches the retained source manifest; representative expansion reconstructs the same retained set once with no deletion.
- [ ] Commit `feat(application): expose complete sanitized message audience views`.

### PR 24A4: Brain, graph-of-graphs, timeline, and domain reads

**Files:** `src/features/{brain,activity,graph,timeline,sessions,agents,coordination,code,delivery,knowledge,automation,observatory,accounting,research}/{queries,views,ports}.rs`; `tests/graph_of_graphs.rs`; `benches/brain.rs`.

- [ ] Add tests `brain_uses_rollups_before_project_shards`, `federated_graph_preserves_repo_snapshot_identity`, `same_name_symbol_never_collapses_cross_repo`, `rspack_rsbuild_react_router_fixture_keeps_provenance`, `each_lens_rejects_illegal_edge_kind`, `selection_pivots_at_same_watermark`, `temporal_correlation_is_not_causation`, `git_drift_blocks_joined_impact`, `turn_hub_preserves_native_semantics`, `codex_goal_updates_remain_first_class`, `research_anchor_survives_cursor_and_handle_expiry`, `recipe_reports_version_and_watermark_drift`, `nearby_parallel_worktree_has_direct_overlap_evidence`, `expired_presence_is_unknown_not_absent`, `safe_summary_contains_no_secret_payload`, and `partial_component_does_not_fail_brain`.
- [ ] Run `cargo test -p tracedecay-application --test graph_of_graphs -- --nocapture`. Expected: tests fail because graph/Brain compositions are absent.
- [ ] Implement Section 9.2/9.4 compositions with bounded query profiles, evidence-bearing cross-links, stable inspector/timeline refs, local/live Git reconciliation, and domain response schemas.
- [ ] Re-run the command. Expected: all tests pass; irrelevant shard open counter remains zero; no inferred edge uses observed/causal copy.
- [ ] Run `cargo bench -p tracedecay-application --bench brain -- --save-baseline pr24a4`. Expected: first useful response meets the master two-second current-scale gate and reports shard opens, watermarks, component coverage, bytes, p50/p95.
- [ ] Commit `feat(application): compose Brain and investigation reads`.

### PR 24A5: Command unit of work, idempotency, optimistic versions, and audit

**Files:** `src/kernel/{unit_of_work,idempotency,audit,optimistic}.rs`; per-domain `src/features/<domain>/{commands,ports}.rs`; `tests/{command_pipeline,idempotency_optimistic}.rs`; `benches/commands.rs`.

- [ ] Add tests `identical_retry_returns_stored_receipt`, `changed_payload_same_key_conflicts`, `version_conflict_writes_nothing`, `confirmed_operation_preflight_token_must_match`, `scope_sensitive_create_requires_declared_scope`, `route_scope_never_selects_owner`, `target_owner_conflict_writes_nothing`, `canonical_event_audit_outbox_and_result_commit_atomically`, `outbox_cannot_create_domain_truth`, `external_effect_never_runs_inside_uow`, and `writer_takeover_fences_stale_commit`.
- [ ] Run `cargo test -p tracedecay-application --test command_pipeline --test idempotency_optimistic -- --nocapture`. Expected: tests fail because command runner/unit-of-work contracts are absent.
- [ ] Implement Section 8 single-owner pipeline, command receipts, safe error details, preview expiry/revalidation, and audit redaction.
- [ ] Re-run the command. Expected: all tests pass; crash-before/after-commit fixture yields either no effect or one effect and repeatable receipt.
- [ ] Run `cargo bench -p tracedecay-application --bench commands -- --save-baseline pr24a5`. Expected: reports preflight/confirmed-commit/direct-commit/idempotent-retry p50/p95 and transaction duration without external I/O.
- [ ] Commit `feat(application): add audited idempotent commands`.

### PR 24A6: Resumable workflows and operational commands

**Files:** `src/kernel/operations.rs`; `src/features/{operations,capture,projection,delivery,integrations}/{commands,ports}.rs`; `src/ports/host_deployment.rs`; all registered per-domain command handlers; `tests/workflow_recovery.rs`.

- [ ] Add workflow fault cases for process death before/after step effect and receipt, duplicate worker, stale lifecycle lease, drain with active MCP/watch/index work, upgrade process exit before durable drain receipt, update restart/takeover/recovery, version drift, disk pressure, cancelled export, projection publish failure, retention hold, migration ambiguity, #425 split-store open-holder refusal/freeze/write-reservation/backup/staging/verification/cutover/restart recovery, host-integration probe/install/reload/verify interruption, stale manifest/config observation, foreign-owned component refusal, idempotent re-entry, uncertain host effect reconciliation, uninstall compensation and restart-required recovery, remote refresh followed by ref rewrite, provider-refresh leader death/joiner cancellation/partial source failure, coordination target expiry/delivery-without-ack/duplicate handoff/suppression, scope-owner move conflict, share-bundle expiry/revocation, irreversible delete grace, and automation admission races: 1,000 unchanged wakeups, unrelated-project activity, concurrent dirty event after snapshot, active/unknown writer, identical terminal input, quiet/max-debounce boundary, `NoChange` atomic frontier advance, retryable failure retention/backoff/circuit, bounded character/token/evidence preflight, oversized-input digest quarantine with job still enabled/visible, dependency-version-triggered resume, max-dirty-age starvation incident, uncertain-effect reconciliation, self-trigger suppression, 64 concurrent schedulers, crash at every admission boundary, and combined evidence batch with independent job receipts.
- [ ] Run `cargo test -p tracedecay-application --test workflow_recovery -- --nocapture`. Expected: tests fail because workflow runner/definitions are absent.
- [ ] Implement each Section 10 command descriptor, pollable operation status, the workflows named in Section 8.3, and Section 11.6's bounded dirty-scope automation admission; every external effect is a separately receipted step and every owner transaction is idempotent. One thousand unchanged ticks produce zero runs/model/tool calls, no all-scope scan, and only a bounded skip-episode/metric update.
- [ ] Re-run the command. Expected: every fault fixture reaches one named recoverable/terminal state, no duplicate effect receipt, and unaffected shard reads remain available.
- [ ] Commit `feat(application): orchestrate recoverable operational workflows`.

### PR 24A7: Replay labs and Evolution Studio

**Files:** `src/kernel/operations/*`; `src/features/experiments/{queries,commands,views,ports,evaluators}/*`; `tests/experiments_replay.rs`.

- [ ] Add requested/actual exact/recorded/best-effort fixtures for every evaluator plus `selection_forks_typed_experiment_with_source_backlink`, `run_cohort_cells_have_unique_variant_evaluator_case_repetition_sweep_coordinates`, `all_experiment_artifacts_have_stable_anchors`, `branch_ref_is_sole_immutable_merge_free_ancestry`, `typed_sweep_caps_cost_and_resumes_from_receipt`, `paged_comparison_cells_align_added_removed_substituted_and_unaligned_stages`, `running_trace_has_no_sealed_receipt_and_terminal_trace_requires_one`, `hermetic_worker_has_empty_env_closed_fds_read_only_mounts_and_denies_every_production_port`, `resource_budget_kills_and_reaps_process_tree`, `resource_receipt_lists_granted_denied_and_high_water_marks`, `frozen_clock_rng_and_recorded_model_output_reproduce`, `minimizer_preserves_predicate_and_never_promotes`, `search_quality_preserves_cutoff_qrels_and_anchor`, `scope_federation_replays_resolution_and_shard_plan`, `privacy_lab_accepts_synthetic_canary_only`, `coordination_selects_at_most_one_hint`, `coordination_lab_cannot_message`, `evolution_tracks_skill_and_memory_lifecycle`, `simulation_does_not_increment_counters`, and `promotion_requires_scan_and_confirmation`.
- [ ] Run `cargo test -p tracedecay-application --test experiments_replay -- --nocapture`. Expected: tests fail because the shared lifecycle/evaluator registry/hermetic runtime are absent.
- [ ] Implement one experiment/run-cohort operation, explicit cells, requested/actual manifest and side-effect receipts, evaluator registry, sole immutable branch ref, bounded typed sweeps/ablations, paged stage/comparison cells, reduction, all anchors, enforced worker protocol/budgets, and read-only archive ports. Compose policy/query/capture/projector evaluators with immutable refs, preserve fidelity/substitutions/coverage, implement Evolution evaluation, and keep the one typed fixture promotion in the separate command path.
- [ ] Re-run the command. Expected: all tests pass; production write/counter/cache/lease sentinels remain zero; exact digests verify; unavailable artifacts downgrade/refuse explicitly; no evaluator owns lifecycle code.
- [ ] Commit `feat(application): add hermetic experiment and replay workbench`.

Plan 24's PR 24R extends the same application kernel with `features/task_graph/{queries,commands,views,ports}/edit_bundles.rs` and the contained structured-edit adapter. Its red tests cover strict parser/source-span diagnostics, explicit-retire semantics, local-key allocation, semantic diff/rebase/conflicts, stale-base and active-attempt refusal, all-or-none 100,000-item bounded submit, identical retry, unsafe archives, secret rejection, expiry, process death, immediate success purge, TTL/crash cleanup, and byte-for-byte semantic parity across CLI/HTTP/MCP/SDK/UI. It imports plan-01 public edit types and plan-24 graph semantics; it does not add another application kernel, draft store, task model, or transport-specific mutation path.

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
- Managed task-graph editing accepts only contained, strict, bounded bundles; omission never deletes, stale/concurrent/partial submissions write nothing, 100,000-item validation and semantic diff remain bounded, and success/expiry/crash tests prove private staging cleanup without losing content-free receipts.
- Multi-machine requests bind `BrainId`, node/grant, placement generation, authority epoch, requested consistency, and causal frontier. Authority-only commands fail closed offline; cached reads and pending observations remain visibly non-canonical. Revocation closes streams and prevents new operations; restore/promotion cannot admit the old authority.
- `brain.failover.promote` requires a verified recovery receipt plus positive exclusive-fence evidence: graceful old-authority shutdown, verified external exclusive-resource revocation, or an independent quorum lease term. Unreachability/time/operator assertion is never sufficient; without a fence the application offers wait or separate forensic-fork recovery, not same-Brain promotion.
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
- [ ] Run the PR 24R edit-bundle parser/property/fuzz/concurrency/crash/privacy/cross-transport suites. Expected: one semantic candidate and receipt across surfaces, zero partial canonical writes, zero unsafe extracted members, zero retained secret bytes after rejection/submit/expiry, and every terminal workspace has a verified cleanup state.
- [ ] Complete #405/#407/#410 ownership/message migration, #411 doctor authority, #412 drain/update recovery, #413 inventory refresh, stable research anchor/recipe, cross-shard recovery, local/live Git drift, lab read-only, privacy, cutover, stale-client failure, and rollback drills before V2 application becomes default.
