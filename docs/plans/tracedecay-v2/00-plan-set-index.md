# TraceDecay final-V2 plan authority

## Status

This directory defines one fully integrated final-V2 product. It is not a PR
ladder, phase gate, compatibility program, or list of optional future
components. Every retained capability must be reachable through a real
production journey before V2 ships.

This file is the sole roadmap and precedence authority. Component plans supply
design detail only. If a component plan mentions a PR number, stage, shadow
mode, deferred wiring, an old TraceDecay store, branch database, Doctor apply
mode, direct client database access, or SQLite graph/vector authority, this
file and the final decisions below win and the stale wording must be removed.
`NEXT.md` records current implementation outcomes, not another roadmap.

## Final decisions

### Fresh final stores

V2 is a breaking persisted-storage cutover. A selected profile is created in
the final schema. An incompatible TraceDecay profile/project store returns
`ResetRequired` and may be explicitly cleared; V2 has no old-store reader,
migration, upgrade, backfill, consolidation, parity layer, dual write,
cutover marker, archive-merge fact movement, dogfood database flow, or sidecar
database.

Historical data owned by supported agent hosts remains valuable. Existing host
transcripts/logs are acquired through ordinary bounded V2 capture and projected
through the same final authorities as new input. This is source ingestion, not
TraceDecay database migration.

Compatibility aliases are retained only for independently released public
CLI/API/SDK/configuration contracts with concrete evidence. Branch-local or
unreleased V2 shapes change in place.

### One daemon/application authority

MCP, CLI, HTTP, LSP, SDKs, dashboard, hooks, plugins, and host adapters are
clients of the daemon/application layer. They never open SQLite, Grafeo, Git
internals, content payload roots, or writer handles directly.

Reads cause no hidden sync, repair, compaction, migration, retention, access
counter, or projection write. Explicit effects use canonical application
operations with authorization, preconditions, cancellation, idempotency, and
durable receipts. A timed-out effect is reconciled by receipt, never blindly
retried.

Doctor is strictly read-only. It reports typed evidence/coverage and may name a
separate operation the user could choose. It never previews, dispatches,
repairs, refreshes, reclaims, retries, compacts, changes configuration, or
acquires a writer lane.

### Storage placement

- SQLite owns relational records, content locators, immutable events,
  receipts, leases, watermarks, configuration, and transactional state.
- One embedded Grafeo store per canonical project owner shard owns graph
  topology and vector indexes for code, Git, sessions/LCM, Work, workflows,
  automation references, and typed cross-domain relations.
- Holographic fact content, trust, contradictions, retention, and intrinsic
  FHRR operations remain in the project-wide memory authority. Grafeo may store
  typed fact-ID relations and vectors, never a second fact payload authority.
- Branch/ref/worktree/commit/PR/session/agent identity is provenance and query
  selection, not a database or fact-shard boundary.
- Linked worktrees share the project store while retaining exact worktree
  snapshot and generation identity.

No production in-memory graph fallback, SQLite node/edge/vector authority, flat
vector scan, `petgraph`, branch DB, or domain-specific Grafeo store remains.

### Capture, indexing, and convergence

Hooks submit small content-free lifecycle signals through daemon
`hook_runtime`. They do not run sync, ingest commands, models, databases, or
long work. The daemon coalesces file/Git/host changes and schedules bounded
incremental capture and projection.

Project admission may open the repository and read its current identity and
reference tips, but it never walks history, computes a whole-repository digest,
or waits for a graph replacement. Git convergence persists ref/OID frontiers
after every bounded batch, reuses native commit-graph/object caches, processes
only changed refs and previously unseen commits, and yields between batches.
Dirty-worktree capture is a separate coalesced snapshot; it never invalidates
or republishes immutable committed history.

Required fail-closed identity/enrollment checks complete before admission.
Historical convergence, host catch-up, semantic model acquisition, rebuilds,
retention, and compaction run in bounded background work. They do not block
ordinary admission or exact, lexical, graph, and standard retrieval.

Every generation is immutable until atomically activated. Partial, indexing,
stale, failed, cancelled, incompatible, unavailable, unsupported, saturated,
timed-out, and effect-unknown are distinct typed states.

### Lossless sessions and LCM

LCM summaries are derived navigation nodes, not replacements for source
content. Summary sources use opaque cursor pagination and hydrate through the
canonical redaction/content authority of each message's owning store. Raw rows
and ranked metadata are not authoritative hydration.

Public session/LCM operations are read-only. Compression, live transcript
projection, and session boundaries use the daemon hook-runtime lifecycle for
all hosts. When a host does not expose compaction content, the configured
daemon context engine may create an auxiliary summary from authorized visible
sources while preserving exact lineage.

### Product-wide facts

Facts are project-wide and survive branch/worktree/ref deletion. Their
provenance may record the branch, ref, worktree, commit, PR, session, agent, or
host where they were created. No branch-only fact fixture, store, fallback,
archive merge, or compatibility mirror remains.

### Work, workflows, and delivery

Work/task topology and workflow DAG topology use Grafeo. SQLite retains their
events, payloads, attempts, leases, budgets, fences, activation CAS,
watermarks, receipts, and provider artifacts.

The product includes create/change/history, evidence, readiness, Kanban/DAG/
timeline/critical path, decomposition/sizing/routing, explicit review and
admission, provider execution, progress/cancellation/recovery, integration,
outcome review, calibration, handoff, and live replanning. No capability ships
as schema-only, adapter-only, mountless, shadow, discovery-only, or a dormant
SDK method.

## End-to-end product journeys

V2 is complete only when these journeys work through production composition:

1. **Enroll and open.** A clean profile enrolls a project, opens canonical
   SQLite/Grafeo authorities, starts bounded watchers/capture/projectors, and
   returns promptly without scanning the entire repository or Git history.
2. **Edit to evidence.** A host edit signal produces a new exact worktree/code
   generation; exact/lexical/graph reads remain available while semantic
   projection catches up.
3. **Git to review.** A changed hunk resolves through Git topology to code
   symbols, callers, impact, tests, diagnostics, tasks, CI, review, and release
   evidence.
4. **Lossless recall.** A session/LCM query finds summaries/messages by several
   retrieval modes, pages sources with an opaque cursor, and hydrates exact
   authorized content across owning stores.
5. **Memory.** Store/update/remove/retrieve a project-wide holographic fact,
   link it to code/Git/session/task evidence through Grafeo, and preserve it
   across branch/worktree deletion.
6. **Work execution.** Create and review work, admit a provider run, observe
   progress/cancellation/recovery, integrate its result, and independently
   review the outcome.
7. **Host lifecycle.** Install/update/uninstall each supported host bundle with
   receipt-backed ownership, bounded hooks, daemon routing, truthful
   capability availability, and no store access.
8. **Operate.** Dashboard/CLI/MCP/HTTP/SDK show consistent health, status,
   configuration, storage, costs, automation, workflow, and Doctor results;
   chosen effects use separate receipt-bearing operations.
9. **Backup/restart/shutdown.** A fenced SQLite snapshot and matching Grafeo
   checkpoint restore one consistent generation; graceful shutdown drains or
   cancels work and releases every writer lane.
10. **Remote authority.** Authenticated enrollment, replay, partial coverage,
    backup/staged restore, fencing, failover, revocation, and rejoin preserve
    the same single-writer and privacy invariants.

## Component ownership

- [01](01-domain-crate.md) — stable identities, values, invariants, problems,
  receipts, and evidence envelopes.
- [02](02-store-crate.md) — transactional relational/content ports and
  final-schema storage composition.
- [03](03-capture-crate.md) — bounded current/historical host and source
  acquisition.
- [04](04-projectors-crate.md) — resumable immutable projections and atomic
  activation.
- [05](05-query-crate.md) — exact, lexical, graph, temporal, semantic, fusion,
  cursor, and hydration behavior.
- [06](06-policy-crate.md) — authorization, privacy, capability, effect, and
  resource policy.
- [07](07-hooks-crate.md) — bounded host event adapters and hook-runtime
  lifecycle.
- [08](08-tool-catalog-crate.md) — canonical contextual operation metadata,
  schemas, effects, deadlines, idempotency, and bindings.
- [09](09-application-crate.md) — transport-neutral use cases and read-only
  Doctor.
- [10](10-api-crate.md) — generated public wire authority and adapters.
- [11](11-dashboard-frontend.md) — embedded thirteen-workspace dashboard.
- [12](12-final-store-and-release-interface-boundary.md) — released public interface
  compatibility only; no persisted-state migration.
- [13](13-research-provenance-and-context-anchors.md) — durable cited research
  evidence and anchors.
- [14](14-historical-failure-regression-matrix.md) — falsifiable regression
  behavior, not historical implementation preservation.
- [15](15-search-quality-evaluation-and-retrieval-research.md) — measured
  retrieval quality and evaluation.
- [16](16-cross-project-repository-worktree-scope.md) — exact multi-root,
  project, repository, and linked-worktree identity.
- [17](17-official-public-api-and-sdks.md) — complete Rust/TypeScript SDK
  operation support.
- [18](18-secret-detection-redaction-and-private-data-safety.md) — privacy and
  sink firewalls.
- [19](19-system-defragmentation-convergence-and-extensibility.md) — authority
  convergence and deletion of duplicate seams.
- [20](20-configuration-control-plane.md) — desired/effective configuration and
  explicit changes.
- [21](21-cli-mcp-tool-surface-and-output-unification.md) — semantic parity,
  effects, cancellation, receipts, cursors, and output.
- [22](22-incremental-context-scout-and-suggestion-envelopes.md) — bounded
  contextual hints.
- [23](23-session-lcm-temporal-retrieval-and-evaluation.md) — lossless temporal
  session/LCM retrieval.
- [24](24-canonical-task-plan-graph-and-multi-agent-executor.md) — Work/task
  graph and reviewed execution.
- [25](25-code-intelligence-indexing-crate.md) — deterministic incremental code
  generations and graph publication.
- [26](26-observability-accounting-and-usage.md) — bounded truthful metrics,
  traces, costs, and usage.
- [27](27-cross-host-agent-plugin-bundles.md) — supported host lifecycle and
  capability parity.
- [28](28-remote-multi-machine-shared-brain.md) — authenticated remote
  single-writer authority and failover.
- [31](31-native-fastembed-semantic-code-search.md) — local model admission and
  Grafeo semantic projection.
- [32](32-dynamic-workflow-runtime-and-sdk.md) — workflow/provider execution,
  recovery, and SDK.
- [33](33-end-to-end-performance-optimization.md) — measured deadline and
  resource optimization without weakened semantics.
- [34](34-workspace-refactoring-and-api-cutover.md) — complete crate/API
  cutovers and deletion of superseded custom code.
- [35](35-daemon-lsp-gateway-and-universal-diagnostics.md) — daemon gateway,
  LSP, diagnostics, cancellation, and capacity.
- [36](36-git-aware-change-context-and-index-transactions.md) — native Git
  authority, change context, and index/edit transactions.
- [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) —
  read-only feedback/review/proximity evidence.
- [38](38-storage-retention-size-and-efficiency.md) — reachability retention,
  compaction, backup, and storage telemetry.
- [39](39-embedded-grafeo-graph-database.md) — sole embedded graph/vector
  authority and cross-domain production cutover.

Plans 29 and 30 were never assigned distinct final capabilities; no placeholder
files or empty stages are created for them.

## Verification

Acceptance is behavioral and measured:

- focused crate/integration tests during implementation;
- clean-checkout workspace/all-feature tests;
- dashboard typecheck, tests, generated-contract check, and production build;
- all supported host bundle install/update/uninstall and hook journeys;
- Rust/TypeScript SDK generation and operation coverage;
- all public tools exercised under canonical metadata/effect journeys and
  deadlines, including every mutation's receipt/reconciliation path;
- isolated-profile restart, crash, cancellation, backup/restore, retention,
  watcher, shutdown, privacy, multi-root, and concurrency tests;
- production-size benchmarks for MCP dispatch, retrieval, Git/code projection,
  Grafeo traversal/vector search, storage, and memory;
- CI/package/release artifact verification.

No fixed test count, source-shape scan, exact-name gate, timeout increase,
ignored failure, or synthetic lookalike substitutes for direct evidence. Tests
and development daemons use isolated profiles and never dogfood the operator's
installed stable TraceDecay.
