# TraceDecay V2 rewrite

Status: active product rewrite. PR7 is complete, PR8 implementation is active,
and PR #421 remains open.

The authoritative delivery order is [00-plan-set-index.md](00-plan-set-index.md).
The active PR8 execution slice is [NEXT.md](NEXT.md). These are contributor
documents only and never product runtime input. Numbered plans define component
requirements and boundaries, not separate crate-first work queues.

## Current product foundation

- `tracedecay-domain` contains the first executable V2 domain contracts.
- `tracedecay-store` defines canonical transcript persistence while the
  already-open `GlobalDb` remains the physical connection and transaction
  authority.
- Transcript ingest, startup catch-up, restart recovery, daemon, MCP, and
  dashboard paths use that authority without a fallback writer.
- Transcript batches atomically update messages, projections, durable cursors,
  and monotonic offsets. Replay and exact retries are idempotent.
- Transcript and LCM mutations use fresh RAII transactions. Failure or
  cancellation rolls back database rows and newly created payload files.
- Direct tests cover Claude, Cursor, Cline-like input, partial records, replay,
  rollback, restart, concurrency, and Windows behavior.
- Existing Doctor, daemon, storage, hooks, MCP, and CLI remain product code.
- Claude production capture now emits path-independent sanitized observations,
  typed receipts, durable cursors, and deterministic searchable projections.
- Observation, receipt, cursor, enqueue, projection effects, and checkpoints
  preserve atomic restart/retry behavior; exact no-op replay performs no writes.
- The committed PR5 workload and clean-commit acceptance artifact record the
  production parse/sanitize/commit/project/replay baseline for PR20.
- PR6 extends that path across the supported Claude, Codex, Cursor, Hermes,
  Kiro, and Cline-family sources through one host-neutral catalog and provider
  observation contract.
- Non-replayable events use bounded daemon-owned host admission; replayable
  sources use bounded fair scheduling, atomic cursor/projection commits, and a
  staged bounded projection rebuild rather than provider-local durable state.
- Executable native host fixtures and typed hook-telemetry readiness now replace
  prose-only provider claims. The clean attested PR6 benchmark acceptance is
  recorded by commit `05da230e`; PR6 is complete.

## Storage and authority

- Before remote delivery, one local daemon is the sole mutable SQLite
  authority; PR16 preserves exactly one fenced daemon authority per mutable
  shard. Hooks, clients, workers, MCP servers, dashboard handlers, and remote
  nodes send typed operations to the owning authority.
- Project facts and project session/LCM data live in one canonical project-wide
  store shared across branches and worktrees.
- Profile-wide user activity lives in the user/profile store.
- Only code indexes are branch/worktree/snapshot scoped.
- Worktrees resolve their project through the project registry and Git common
  directory. Missing or ambiguous authority fails closed.
- No path may create a worktree-local, source-adjacent, in-memory, recovery, or
  direct-database fallback writer.

## Delivery rules

- Ship executable product behavior and direct tests in every PR.
- Prefer one end-to-end vertical slice over broad scaffolding.
- Component plans may contribute to the same PR. A plan name does not require a
  new crate, generator, registry, or standalone implementation phase.
- One typed kernel owns each mechanism. Public names and compatibility aliases
  are bindings, never alternate query, edit, storage, rendering, health, or
  workflow implementations.
- Consumers compose owner results without duplicating authority: Plan 05 owns
  shared query execution, Plan 23 temporal session/LCM retrieval, Plan 14 the
  Doctor/health/remediation kernel, Plan 20 configuration resolution,
  snapshots, and source policy metadata (including mandatory local privacy),
  Plan 26 measurements and labels, and Plan 32 workflow clocks, scheduling,
  attempts, effects, and runtime receipts. Plan 11 renders Plan 14 findings
  and legal actions without becoming another health or remediation authority.
- External-source contracts separate definition/binding identity
  ([Plan 01](01-domain-crate.md)), Plan 20 policy metadata, Plan 06 pure
  authorization, Plan 03/27 capture and connectors, and Plan 09 sink recheck.
  Definitions never own local privacy or sinks.
- Federated retrieval and `QueryCollection` / `WorkspaceCollection` are Plan 16
  scope contracts executed through Plan 05; membership never grants ownership
  or authorization. Evidence spans are Plan 23 session-derived for PR8 and Plan
  13 `EvidenceSpanRecordV1` for cross-domain anchors; Plan 24 references them
  from TaskId-rooted packets without copying span authority.
- Task investigation/evidence is Plan 24; optional synthesis and runtime
  execution are Plan 32. Canonical task retrieval rejects demonstrated
  expertise; expertise may exist only in an authorized ephemeral interactive
  view and never enters durable evidence, completion, or routing. LSP
  investigation handoff is Plan 35 cue/token encoding over Plan 09
  transport-neutral results. Dashboard investigation UX is Plan 11 over those
  envelopes. Observability descriptors are Plan 26; PR20 performance
  optimization is Plan 33. Active executable work remains PR8 temporal
  retrieval in [NEXT.md](NEXT.md).
- High-confidence architecture and explicit rejection findings are normative.
  Medium-confidence model, ranking, topology, renderer, calibration, and
  optimization mechanisms are versioned candidates that ship only after
  TraceDecay-specific locked evidence. Low-confidence or causal product-effect
  claims are neither requirements nor acceptance gates without direct
  intervention evidence.
- Code and product quantifiers retain raw values, units, denominators,
  coverage, cohort, temporal delta, provenance, uncertainty kind, and
  calibration validity. Universal quality/health/reward/readiness scores,
  uncalibrated probabilities, and dashboard-local truth are prohibited.
- PR9 exact identifiers, paths, quoted phrases, errors, tool names, and
  configuration keys form a non-demotable lexical tier. PR10 semantic search
  starts with an exact flat-vector baseline/oracle; rerankers, ANN, late
  interaction, and quantization remain measured candidates, never mandatory
  defaults or identity/equivalence evidence.
- [Git intelligence and safe repository operations](36-git-aware-change-context-and-index-transactions.md)
  progress from PR7 provenance anchors through PR9 read-only semantic evidence,
  PR11 daemon-serialized index/commit transactions, and PR12 shared surface
  bindings. They never autonomously mutate branches, worktrees, refs, or
  published history.
- PR17 delivers the [canonical product task/work graph and Kanban/DAG/timeline/
  causal/workload projections](24-canonical-task-plan-graph-and-multi-agent-executor.md)
  plus advisory task-shape/decomposition/sizing/model-routing intelligence,
  independent outcomes/calibration, and human-reviewed live resize/re-route
  proposals. Every opaque `TaskId` is an authorized retrieval root whose compact
  context losslessly expands through Plan 23 narrative retrieval, Plan 13
  anchors, Plan 25 code generations, and owning Git, CI, diagnostic, review,
  artifact, and runtime stores. [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)
  remains the sole runtime clock, scheduler, history, lease, attempt, effect,
  and artifact authority.
  Recommendations never auto-mutate the graph or silently choose a model.
  These are typed product data, never an execution model for this developer
  roadmap.
- [The branch-aware feedback cycle, read-only GitHub review-comment
  ingestion/surfacing, CI-failure localization, and tiered concurrent-agent
  proximity](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  compose the existing semantic-evidence, query, Git
  ([Plan 36](36-git-aware-change-context-and-index-transactions.md)), temporal
  retrieval ([Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)), Scout, host, and
  observability owners behind one typed read-only/advisory cycle. PR11–PR13 is
  the first coherent milestone with all four pillars; PR13 is first availability
  of GitHub/CI/proximity adapters; PR14 dashboard/Doctor; PR15 multi-root;
  PR16 remote; PR17 composes already-shipped advisory operations into
  [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) workflows without GitHub
  writes. TraceDecay never posts, updates, resolves, replies to, or dismisses
  GitHub comments. It introduces no second diagnostic store, provider contract,
  suggestion channel, or executor.
- Preserve stock Cargo compatibility. Developer-local build wrappers and cache
  layouts are never repository or CI requirements.
- Use explicit cancellation and typed progress for long operations. Do not add
  an automatic rewrite, workflow, agent, or no-progress timeout.
- Keep privacy, recovery, concurrency, cross-platform, migration, and deletion
  gates with the product behavior they protect.
- PR12 transport and PR18 SDK contracts are revisioned and tested for
  structural, semantic, and lifecycle compatibility. Generated schemas,
  clients, or successful compilation do not establish product conformance.
- PR14 ships Plan 11's shared shell and original twelve workspaces, including
  renderer-neutral flagship Brain/Explorer/Loom surfaces, with a permissive
  default renderer and accessible semantic parity. PR17 adds the Work task-
  graph/Kanban/DAG/timeline/causal/workload UI. Optional GPU or commercial
  adapters draw and accelerate only; they never own graph, query, storage,
  health, readiness, scheduling, ranking, or remediation.
- PR15 freezes one authorized scope-set digest and per-shard state vector for
  federation. PR16 enforces a higher fence at every durable mutation/publication
  sink and admits duplicate offline delivery through idempotent receipts; CRDT,
  wall-clock, or replicated-SQLite convergence never becomes mutation authority.
- PR19 migration is forward-only after publication: V1 archives are bounded
  recovery input, restoration produces verified V2 under a new fence, and no
  reverse cutover, long-lived dual write, lazy read migration, or production
  shadow read remains.
- Instrument each production path when it ships and retain a representative
  baseline for [PR20 performance optimization](33-end-to-end-performance-optimization.md).
  Promotion uses frozen workloads, A/A noise floors, paired effects and
  intervals, practical margins, worst-stratum/resource/tail evidence,
  open-loop overload accounting, and recomputation equivalence—not a universal
  score, public benchmark rank, or paper-derived threshold.
- PR #421 merges only after PR20 completes and aggregate verification is stable.

## Removed permanently

- compatibility and architecture inventory implementations;
- plan Markdown parsers, PR-ID normalizers, developer-roadmap slice DAGs,
  completion ledgers, progress trackers, next-ready controllers, and rewrite
  executors;
- generated plan views, owner maps, baseline packets, and planning-artifact CI;
- large agent checklists or Claude workflow JavaScript for executing the rewrite;
- parallel YAML/JSON/Markdown models that generate product declarations.

Real product generation remains legal only when it removes duplicate product
authorities and follows [RUST-METAPROGRAMMING.md](RUST-METAPROGRAMMING.md).
Real task/work graphs and dynamic workflows are daemon-owned typed product
operations. They never parse or execute this roadmap.

## Release

V2 library crates publish through the workspace release flow while the root
package owns the Git tag and GitHub release. A new crate's first crates.io
publication may require one-time trusted-publisher or token bootstrap; this is a
release setup step, not an alternate development workflow.
