# TraceDecay V2 Projectors Crate Implementation Plan

> **Accepted-base refresh delta (audit 29 / packet 30):** preserve the
> user-canonical plus per-project projection fan-out over `[None, *project_roots]`
> (PR #453); **add** per-shard idempotent receipts and catch-up reconciliation so
> a mid-loop shard failure is later reconciled with the same message ID and no
> duplicate. See
> [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
> §5, §7.1 and FM-162.

**Goal:** Build `tracedecay-projectors`, the deterministic framework and complete domain projector registry that turns immutable observations/events into versioned, evidence-bearing, rebuildable activity and project read models.

**Architecture:** A canonical-event projector converts captured observations into immutable typed events; independent domain projectors then consume shard outboxes at least once and commit output rows plus checkpoints atomically. Registry versions, per-shard contiguous checkpoints, vector watermarks, bounded gap handling, dead letters, build generations, validation manifests, and atomic pointer swaps make every projection replayable without stopping capture or corrupting the active generation.

**Tech Stack:** Rust workspace; `tracedecay-domain`; store ports implemented by `tracedecay-store` over SQLite/WAL and graph generations; `serde`; deterministic canonical encoding/hashing; property, differential, copied-store, crash/recovery, concurrency, and Criterion tests.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) adds plan/work-item/readiness/dependency/topological/critical-path/attempt/executor/workspace/context-packet/cost/status and cross-graph materiality projectors under this crate. They derive current views from the activity ledger and typed evidence; no board column, executor, or dashboard writes projected truth directly.

---

## Goals

- Convert every supported capture record family into a registered typed event or an explicit dead letter; no silent discard.
- Project complete sessions, messages, tools/results, exposed reasoning, provider-native goals/tasks/plans, canonical initiative/plan/work-item/attempt relations, parent/subagents, inter-agent events, LCM lineage, Git/delivery, code, knowledge/policy, hooks/hints, automation/skills, and accounting evidence. Observed provider task/plan state cannot grant canonical readiness or execution authority.
- Project agent presence/work claims, safe scope overlap features, redundancy declarations, TTL/current state, acknowledgements, handoffs, and coordination outcomes without turning proximity into causation or authority.
- Keep canonical provider transcripts in profile `activity.db`; place only project locators/scoped read models in `project.db`.
- Consume outboxes at least once while producing idempotent rows and exactly-once checkpoint advancement per projector transaction.
- Preserve per-source/per-agent order, gaps, late arrivals, and provider-declared causation without inventing one global total order across concurrent agents.
- Rebuild any projector within retained evidence, validate it, then atomically swap the active generation while readers continue on the previous generation.
- Expose vector watermarks and coverage for every cross-shard/aggregate projection.
- Differential-test V1 behavior and tool/event surfaces before each bounded-context cutover, with a one-step generation rollback.

## Non-goals

- No source discovery, raw framing, redaction, hook synchronous spooling, or source-offset ownership; `tracedecay-capture` owns those concerns.
- No mutable canonical observations/events, no destructive correction, and no in-place historical rewrite.
- No query parsing, shard planning, ranking, transport rendering, HTTP, MCP, CLI, dashboard, or policy evaluation execution.
- No inference from temporal proximity alone and no hidden chain-of-thought reconstruction.
- No requirement that unrelated projectors or independent agent lanes serialize behind one global lock.
- No deletion of V1 stores or previous valid projection generations during cutover.

## Convergence boundary

In a multi-machine Brain, only the current fenced authority for a shard runs canonical projectors, builds canonical code/Git packs, or advances checkpoints. Remote nodes may capture and sanitize observations; replicas/caches consume authority-signed published generations and never project or submit packs independently. Every projector transaction compare-checks plan 28's placement and authority epoch in addition to its ordinary lease/checkpoint.

Projectors are the sole observation-to-canonical/read-model derivation owner in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md). They consume domain/capture/store contracts from Plans [`01`](01-domain-crate.md)–[`03`](03-capture-crate.md), enforce the Plan [`18`](18-secret-detection-redaction-and-private-data-safety.md) sink firewall, and produce the only read models consumed by [`05-query-crate.md`](05-query-crate.md). Plans [`22`](22-incremental-context-scout-and-suggestion-envelopes.md) and [`23`](23-session-lcm-temporal-retrieval-and-evaluation.md) add scout lifecycle/outcome and message-occurrence/copy/summary-horizon/temporal-assertion projections here; projectors never schedule scout work or choose relevance/current truth.

| Boundary | Contract |
|---|---|
| Enters | Sanitized immutable observations, prior canonical events, registered schema/predicate/privacy rules, outbox sequences, explicit source/vector watermarks, and frozen rebuild manifests. |
| Exits | Canonical events/entity versions/bitemporal relations, sink-eligible read rows/representations, checkpoints, dead letters, graph/read-model generations, manifests, and lag/coverage receipts. |
| Upstream owners | Domain owns legal values/relations; capture owns parsing/sanitization/observation truth; store owns atomic persistence and publication. |
| Downstream owners | Query reads projections; policy/application/API/UI interpret them. No downstream consumer recreates session/code/Git/knowledge/automation semantics from raw observations. |
| Extension seam | A new event family requires registry kind/predicate/privacy entries, one projector owner, declared dependencies/ordering/target, deterministic fixture, rebuild/parity manifest, query capability, and retirement mapping. |
| Scale/concurrency | One lease per `(projector, version, shard, generation)`, bounded batches, atomic effects+outbox+checkpoint, independent shards/projectors, and vector progress with cancellation/backpressure. |
| Migration/retirement | V1 tables feed backfill observations only. Each V1 derived table retires after its projector generation reaches parity and shadow readers cut over; no second mutable projection authority remains. |

Projector errors are stable internal reason codes and safe fields only. Application maps them to public problems; dead letters never embed source content, query text, paths, detector candidates, or raw `serde_json::Value`.

## Cross-crate contract

### Consumes

- `tracedecay-domain`: `ObservationEnvelopeV1`, canonical event/entity/relation contracts, provenance, evidence/sensitivity/retention classes, temporal intervals, schema/predicate registry definitions, and IDs.
- Store/projector ports: immutable observations plus domain `ReplayManifestRef`, gap/fill/rewrite/late/quarantine events, source watermarks, and shadow migration receipt references written by capture. This crate does not import `tracedecay-capture`.
- `tracedecay-store` through projector-owned ports: shard outbox reads, projection transactions, identity allocation, event/relation append, checkpoints, dead letters, generation build/validation/publish, manifests, and catalog watermark publication.
- Domain policy-evaluation events written through application/store ports; projectors never execute policy and do not import `tracedecay-policy`.

### Produces

- Immutable canonical events and superseding correction events.
- `EntityVersionV1` rows, aliases/candidates, bitemporal `RelationAssertionV1` rows, activity/project domain tables, search/representation source rows, facets/rollups, the `read_models` family (facet rollups, timeline density, observatory status) consumed by plan 05, and safe catalog locators.
- Per-projector/shard checkpoints, vector watermarks, lag metrics, dead letters, rebuild manifests, active-generation receipts, and rollback pointers.
- Query-ready data consumed by `tracedecay-query`, `tracedecay-application`, CLI/MCP/HTTP adapters, dashboard workspaces, exports, and replay labs.

The dependency boundary is `tracedecay-domain <- tracedecay-projectors`; store implementations are injected by application/root composition. Projectors cannot import V1 modules, transport modules, or dashboard code.

## Exact crate and module layout

| File | Responsibility |
|---|---|
| `crates/tracedecay-projectors/Cargo.toml` | Crate dependencies and test/benchmark targets. |
| `crates/tracedecay-projectors/src/lib.rs` | Public exports and built-in registry constructor. |
| `crates/tracedecay-projectors/src/error.rs` | Typed registry, input, schema, ordering, transaction, dead-letter, rebuild, and validation errors. |
| `crates/tracedecay-projectors/src/projector.rs` | Object-safe `Projector` contract, descriptors, inputs, context, outcomes, and idempotency keys. |
| `crates/tracedecay-projectors/src/registry.rs` | Complete versioned projector registry, dependency DAG, event-family ownership, and cycle/coverage validation. |
| `crates/tracedecay-projectors/src/outbox.rs` | Bounded outbox reads, duplicate suppression, contiguous sequence/gap tracking, leases, and retries. |
| `crates/tracedecay-projectors/src/checkpoint.rs` | Checkpoint compare-and-set, source vectors, status, pause/resume, and compatibility. |
| `crates/tracedecay-projectors/src/progress.rs` | Algorithms over domain `VectorWatermark`, dominance/comparability, partial coverage, and catalog publication; defines no watermark type. |
| `crates/tracedecay-projectors/src/coordinator.rs` | Dependency-aware scheduling across shards/projectors, bounded batches, cancellation, and backpressure. |
| `crates/tracedecay-projectors/src/dead_letter.rs` | Stable reason codes, retry/block/advance policy, inspection, and replay. |
| `crates/tracedecay-projectors/src/rebuild.rs` | Lease, disk preflight, build generation, replay, validation, atomic publish, retirement, and rollback. |
| `crates/tracedecay-projectors/src/manifest.rs` | Deterministic row/hash/count/source/dead-letter manifests and parity receipts. |
| `crates/tracedecay-projectors/src/canonical_event.rs` | Observation-to-canonical-event conversion, supersession, provenance, and registry enforcement. |
| `crates/tracedecay-projectors/src/identity.rs` | Durable allocation/alias/candidate/split-identity projection and evidence relations. |
| `crates/tracedecay-projectors/src/activity/mod.rs` | Activity projector registration and shared activity keys. |
| `crates/tracedecay-projectors/src/activity/sessions.rs` | Sessions, turns, messages, content parts, actors, models, and roles. |
| `crates/tracedecay-projectors/src/activity/tools.rs` | Tool definitions, invocations, results, approvals, retries, errors, and provider-specific original kinds. |
| `crates/tracedecay-projectors/src/activity/reasoning.rs` | Provider-exposed reasoning artifacts and unavailable/encrypted/redacted coverage markers. |
| `crates/tracedecay-projectors/src/activity/goals.rs` | Goals, tasks, plan updates, status transitions, budgets, and supersession. |
| `crates/tracedecay-projectors/src/activity/agents.rs` | Agent instances, parent/child, spawn, inter-agent message, handoff, workflow/run, and lifecycle events. |
| `crates/tracedecay-projectors/src/activity/coordination.rs` | Agent presence, work claims/scopes/anchors, heartbeat/TTL/current status, redundancy declarations, overlap evidence, acknowledgements, handoffs, and outcome counters. |
| `crates/tracedecay-projectors/src/activity/project_locators.rs` | Evidence-bearing session/activity-to-project/repository/worktree/branch/snapshot locators. |
| `crates/tracedecay-projectors/src/lcm/mod.rs` | LCM/context projector registration. |
| `crates/tracedecay-projectors/src/lcm/raw.rs` | Raw-message locators, content parts, source positions plus privacy-domain-keyed fingerprints, and payload coverage. |
| `crates/tracedecay-projectors/src/lcm/dag.rs` | Summary nodes, exact source ranges/messages, fan-in, supersession, and DAG invariants. |
| `crates/tracedecay-projectors/src/lcm/compression.rs` | Compression decisions/boundaries, context assembly, lifecycle, and replay state. |
| `crates/tracedecay-projectors/src/lcm/payloads.rs` | Payload refs, range lineage, retention/tombstone/locked/missing state; no blob ownership. |
| `crates/tracedecay-projectors/src/git_delivery/mod.rs` | Git/delivery projector registration, shared identities, and evidence invariants. |
| `crates/tracedecay-projectors/src/git_delivery/core.rs` | Repositories, checkouts, worktrees, refs, commits, PRs, checks, reviews, releases, and fetched-at state. |
| `crates/tracedecay-projectors/src/git_delivery/related_systems.rs` | Fork/upstream/downstream, patch/backport, generated/published artifact, reproduction, and benchmark relations across repositories. |
| `crates/tracedecay-projectors/src/code/mod.rs` | Code projector registration and repository/checkout/ref/snapshot/generation ownership invariants. |
| `crates/tracedecay-projectors/src/code/snapshots.rs` | Immutable snapshot/dirty-overlay identity and explicit scope bindings. |
| `crates/tracedecay-projectors/src/code/graph.rs` | File/symbol occurrences, edges, diagnostics, tests, results, and packed generation output. |
| `crates/tracedecay-projectors/src/code/lineage.rs` | Rename/move/split/merge candidates and evidence-bearing symbol lineage. |
| `crates/tracedecay-projectors/src/code/federation.rs` | Repository/checkout/worktree/ref/snapshot/generation tuples, cross-generation joins, ambiguity, freshness, and partial coverage. |
| `crates/tracedecay-projectors/src/knowledge.rs` | Facts/versions, entities, decisions, contradictions, trust, retrieval, feedback, curation, and deletion lineage. |
| `crates/tracedecay-projectors/src/policy.rs` | Hint/retrieval/routing/diagnostic/correlation/curation/scheduler/memory evaluation records and outcomes. |
| `crates/tracedecay-projectors/src/automation.rs` | Jobs, effective config, registered field-level dependency selectors and typed trigger frontiers, per-scope dirty generations plus current/considered/consumed/included frontiers and last-terminal input/outcome refs, admission/skip receipts, runs/reconciliation, agents, artifacts, curation candidates, autonomy decisions/effects/recovery, skills, outcomes, and clearly labeled imported historical approvals/applies. |
| `crates/tracedecay-projectors/src/operations.rs` | Installations, skill materialization/ownership/drift/remediation, daemon/update lifecycle leases, drain/checkpoint/service-state receipts, doctor/repair outcomes. |
| `crates/tracedecay-projectors/src/accounting.rs` | Tokens, latency, model/tool usage, costs, savings methodology, adoption denominators, and data-quality signals. |
| `crates/tracedecay-projectors/src/search.rs` | Redaction-gated lexical documents and representation eligibility/source metadata. |
| `crates/tracedecay-projectors/src/privacy.rs` | Plan 18 sink firewall, receipt validation, descendant lineage, and checked conversions to `SearchEligibleText`/other sink types; never scans or redacts. |
| `crates/tracedecay-projectors/src/aggregates.rs` | Project/day/kind/provider/model/tool/hint/automation/health/cost rollups with source watermarks. |
| `crates/tracedecay-projectors/src/read_models/mod.rs` | Query read-model family registration (facets, timeline density, observatory status) consumed by [`05-query-crate.md`](05-query-crate.md) through query ports. |
| `crates/tracedecay-projectors/src/read_models/facets.rs` | Precomputed per-scope facet bucket rows behind plan 05 facet requests. |
| `crates/tracedecay-projectors/src/read_models/timeline.rs` | Bitemporal timeline density buckets behind plan 05 timeline pages and the dashboard density brush. |
| `crates/tracedecay-projectors/src/read_models/observatory.rs` | Subsystem health/lag/conflict status rows behind the dashboard Observatory. |
| `crates/tracedecay-projectors/src/read_models/profile_atlas.rs` | Manifest-built profile-atlas tile pyramid, label priority, neighbor/prefetch graph, and cross-generation anchor lineage; canonical entities/relations remain in owner shards. |
| `crates/tracedecay-projectors/tests/framework_suite.rs` | Registry, checkpoint, outbox, dead-letter, concurrency, rebuild, and atomic-swap contracts. |
| `crates/tracedecay-projectors/tests/activity_suite.rs` | Provider/session/tool/reasoning/goal/agent/LCM domain fixtures. |
| `crates/tracedecay-projectors/tests/domain_suite.rs` | Git/code/knowledge/policy/automation/accounting/search/aggregate fixtures. |
| `crates/tracedecay-projectors/tests/backfill_parity.rs` | Copied V1 stores, backfill manifests, PR #405/#407 identity ownership, PR #410 origin/representative semantics, and differential V1/V2 behavior. |
| `crates/tracedecay-projectors/tests/recovery_suite.rs` | Kill-at-boundary, corrupt/missing shard, rebuild resume, failed validation, rollback, and restore. |
| `crates/tracedecay-projectors/benches/projectors.rs` | Visibility lag, batch throughput, rebuild, vector merge, and concurrent-agent workloads. |

Root-composition companion glue is `src/v2_adapters/projector_store.rs`: it implements projector-owned `ProjectorStore`/generation ports over store `OutboxRepository`, `ProjectionRepository`, read snapshots, and graph publication. Neither projectors nor application imports a concrete store implementation, and the adapter cannot add projector semantics or advance a checkpoint outside the store's atomic projection commit.

## Public API and fixed signatures

```rust
pub trait Projector: Send + Sync {
    fn descriptor(&self) -> &'static ProjectorDescriptor;
    fn project(
        &self,
        input: ProjectionInput<'_>,
        context: &ProjectionContext,
        tx: &mut dyn ProjectionTransaction,
    ) -> Result<ProjectionOutcome, ProjectorError>;
    fn validate_generation(
        &self,
        generation: &dyn ProjectionGeneration,
    ) -> Result<ValidationReport, ProjectorError>;
}

pub enum ProjectionInput<'a> {
    Observation(&'a ObservationEnvelopeV1), // receipt-bound sanitized payload only
    Event(&'a CanonicalEventV1),
}

pub struct ProjectorDescriptor {
    pub id: ProjectorId,
    pub version: ProjectorVersion,
    pub input_kinds: &'static [RegistryKind],
    pub output_kinds: &'static [RegistryKind],
    pub target: ProjectionTarget,
    pub dependencies: &'static [ProjectorDependency],
    pub ordering: OrderingRequirement,
    pub rebuild_policy: RebuildPolicy,
}

pub enum ProjectionTarget {
    OwningActivityShard,
    OwningProjectShard,
    GraphGeneration,
    CatalogMetadata,
}

pub enum OrderingRequirement {
    OutboxSequence,
    SourceSequence,
    EntityVersion,
    Commutative,
}
```

```rust
pub trait ProjectionTransaction {
    fn idempotency_guard(&mut self, key: ProjectionEffectKey) -> Result<bool, ProjectorError>;
    fn append_event(&mut self, event: CanonicalEventV1) -> Result<EventId, ProjectorError>;
    fn upsert_entity_version(&mut self, version: EntityVersionV1) -> Result<(), ProjectorError>;
    fn assert_relation(&mut self, relation: RelationAssertionV1) -> Result<RelationId, ProjectorError>;
    fn put_row(&mut self, row: ProjectionRow) -> Result<(), ProjectorError>;
    fn delete_derived_row(&mut self, key: ProjectionRowKey) -> Result<(), ProjectorError>;
    fn enqueue_outbox(&mut self, output: ProjectorOutput) -> Result<u64, ProjectorError>;
}

pub struct ProjectionEffectKey {
    pub projector: ProjectorId,
    pub version: ProjectorVersion,
    pub input_id: ProjectionInputId,
    pub output_kind: RegistryKind,
    pub output_key: Vec<u8>,
}

pub struct ProjectionOutcome {
    pub effects: u64,
    pub duplicates: u64,
    pub emitted_outbox: Vec<u64>,
    pub coverage: ProjectionCoverage,
}
```

The store commits projection effects, output outbox rows, and the next contiguous checkpoint in one transaction. `idempotency_guard` returns `false` on replayed effects, allowing the checkpoint to advance without writing duplicates. Projectors cannot open their own database connections.

### Registry and canonical-event contract

```rust
pub struct ProjectorRegistry;

impl ProjectorRegistry {
    pub fn builtin() -> Result<Self, RegistryError>;
    pub fn register(&mut self, projector: Box<dyn Projector>) -> Result<(), RegistryError>;
    pub fn validate(
        &self,
        schema: &SchemaRegistryV1,
        predicates: &PredicateRegistryV1,
    ) -> Result<RegistryReport, RegistryError>;
    pub fn plan(&self, changed: &[RegistryKind]) -> Result<ProjectionPlan, RegistryError>;
}

pub struct CanonicalEventProjector;

impl Projector for CanonicalEventProjector {
    fn descriptor(&self) -> &'static ProjectorDescriptor;
    fn project(
        &self,
        input: ProjectionInput<'_>,
        context: &ProjectionContext,
        tx: &mut dyn ProjectionTransaction,
    ) -> Result<ProjectionOutcome, ProjectorError>;
    fn validate_generation(
        &self,
        generation: &dyn ProjectionGeneration,
    ) -> Result<ValidationReport, ProjectorError>;
}
```

- Registry validation runs against plan 01's `SchemaRegistryV1` and `PredicateRegistryV1` (the two registries are distinct types; this crate defines no combined registry) and fails for duplicate projector ID/version, dependency cycle, unknown input/output kind, illegal target ownership, missing sensitivity/retention rule, or any captured structured family with no canonical-event owner.
- Unknown forward schema is dead-lettered as `unsupported_schema` and blocks that projector checkpoint; it is never coerced into a known event.
- Corrections append a superseding canonical event and bitemporal relation; they do not mutate an earlier event.
- `causation_id` is accepted only from direct/provider-declared evidence that passes predicate rules. Temporal proximity yields no causal edge.

### Outbox, checkpoints, watermarks, and concurrency

Authority transfer freezes projection at a vector watermark, verifies the recovery manifest, increments `AuthorityEpoch`, and resumes from the exact checkpoint under a new lease. An old authority, stale replica, or local cache cannot publish projection output after promotion. Deterministic rebuild from the same retained observations must produce the same manifest on another node.

```rust
use tracedecay_domain::{ProjectionCheckpointKeyV1, ProjectionCheckpointV1};

pub struct ProjectionCoordinator<R, S> {
    registry: R,
    store: S,
    config: CoordinatorConfig,
}

impl<R: RegistryPort, S: ProjectorStore> ProjectionCoordinator<R, S> {
    pub fn run_once(&self, request: RunProjectionRequest) -> Result<RunProjectionReport, ProjectorError>;
    pub fn run_until(&self, target: VectorWatermark) -> Result<RunProjectionReport, ProjectorError>;
}
```

`VectorWatermark` above is the domain type, not a projector-local copy. Coordination uses `partial_cmp_components`, `dominates`, and `merge_max`; incomparable vectors stay incomparable and no scalar/global ordering is introduced.

Plan 01's `OutboxConsumerLeaseV1<K>`/`OutboxConsumerCheckpointV1<K>` are the single mechanical outbox-consumer lease/checkpoint core shared with scout and scheduler consumers. `ProjectionCheckpointV1` is the sole projection-specific extension, keyed by `ProjectionCheckpointKeyV1`, and adds only highest-seen, schema/builder version, and projection status. Plan 02's one `ProjectionRepository` owns projection lease acquire/renew/release, checkpoint read/initialization, dead-letter read/resolution, and atomic projection application over that core. Initialization requires an absent zero checkpoint; every later committed advance occurs only through `apply_projection` or an atomic replay resolution with the full expected checkpoint and exact lease epoch ([`02-store-crate.md`](02-store-crate.md)). No crate defines another epoch/CAS/checkpoint codec, directly writes dead-letter state, or moves progress through a side channel.

- One lease exists per `(projector, version, shard, generation)`. Different projectors/shards run concurrently; independent agent sources do not serialize.
- The worker obtains that exact-key lease through `ProjectionRepository::acquire_lease`, reads/initializes the exact checkpoint, renews before its TTL safety margin, and releases on a clean stop. Store CAS—not host liveness guesses—fences a stale worker.
- A leased worker reads a bounded batch after `consumer.last_committed_sequence`. Duplicate outbox rows are idempotent. Missing outbox sequence stops committed advancement and records `projector.outbox_gap`; the worker may advance only examined position and may process explicitly commutative rows beyond the gap into a nonpublished staging generation.
- Source-sequenced activity preserves `(producer, sequence)`. Cross-agent display order uses occurred time, ingested time, producer, sequence, and event ID as a deterministic sort, but it does not assert causation.
- Parent/child, spawn, inter-agent message, handoff, tool-result, and goal-transition relations use provider/host IDs or direct event references. Unresolved targets remain candidate relations; later resolution supersedes the candidate.
- Batch size defaults to 1,000 events or 50 ms of projector CPU, whichever occurs first. Store/WAL backpressure halves the next batch down to 10; healthy drains increase it additively to 10,000.
- Cross-shard rollups publish only with their full input vector. Queries can distinguish dominated, incomparable, stale, unavailable, and redacted components.

### Dead letters and advancement policy

Plan 01 is the sole semantic owner of `DeadLetterReasonV1`, `DeadLetterDispositionV1`, `DeadLetterRecordV1`, `DeadLetterAttemptV1`, `DeadLetterResolutionReceiptV1`, `DeadLetterCompactionV1`, and `DeadLetterPageV1`. This crate selects those values; it does not redefine them. Plan 02 is the sole repository/physical owner and derives the operator view's attempt count, next retry, and terminal resolution by joining the immutable record to its append-only attempts/resolution.

Dead letters persist in plan 02's `dead_letters` family in the owning shard ([`02-store-crate.md`](02-store-crate.md)), keyed by the full `(projector, projector_version, shard, generation)` checkpoint key plus sequence/input. Growth is bounded without deletion of live evidence: resolved dead letters older than the evidence-retention watermark compact into immutable per-`(projector, reason, day)` rollup counts, unresolved blocking records are never compacted, and a per-shard live envelope of 100,000 records or 256 MiB raises backpressure on the offending projector — never silent discard.

Before any `put_row`, graph label/snippet, FTS document, representation source, aggregate label, replay artifact, or emitted outbox payload, `privacy.rs` verifies the source receipt/descendant lineage and requires the corresponding domain sink-eligible wrapper. Receipt verification reads the durable `SanitizationReceiptV1` rows in plan 02's per-shard `sanitization_receipts` table ([`02-store-crate.md`](02-store-crate.md)); capture mints receipts per [`03-capture-crate.md`](03-capture-crate.md), and the table's expiry/revocation state is what "expired" and "revoked" mean here. `privacy.rs` never turns raw/classified text into an eligible value. A missing, incomplete, incompatible, expired, or revoked receipt blocks the checkpoint or emits a non-content coverage row according to the registry; it can never be coerced into empty text and counted as complete.

- Registry, sensitivity, identity, evidence, invariant, corrupt-input, ownership, and outbox-gap failures block by default.
- `DeadLetterDispositionV1::QuarantineAndAdvance` is legal only for a registry-declared optional forensic family whose omission is surfaced in projection coverage; canonical messages, tools, reasoning markers, goals, agents, LCM lineage, Git, and automation cannot use it.
- Dead-letter create/attempt/resolution/compaction are typed `DeadLetterMutationV1` values committed by plan 02's projection repository. A blocking failure and unchanged checkpoint commit together; a replay resolution, its registered effects, its receipt, and any now-legal checkpoint advance commit together. Deleting or editing a live dead letter is forbidden; the only removal path is the retention-watermark compaction above, which preserves the rollup counts.
- Rebuild validation fails on unresolved blocking dead letters and reports quarantined omissions by kind/count/hash.

### Rebuild and atomic swap

```rust
pub struct RebuildRequest {
    pub projectors: Vec<ProjectorId>,
    pub shards: Vec<ShardId>,
    pub target_watermark: VectorWatermark,
    pub reason: RebuildReason,
    pub retain_previous_generations: u8,
}

pub struct ProjectionRebuilder<S> {
    store: S,
}

impl<S: ProjectorStore> ProjectionRebuilder<S> {
    pub fn preflight(&self, request: &RebuildRequest) -> Result<RebuildPreflight, ProjectorError>;
    pub fn build(&self, request: RebuildRequest) -> Result<BuildGeneration, ProjectorError>;
    pub fn validate(&self, build: &BuildGeneration) -> Result<ProjectionManifest, ProjectorError>;
    pub fn publish(&self, build: BuildGeneration, manifest: ProjectionManifest) -> Result<SwapReceipt, ProjectorError>;
    pub fn rollback(&self, receipt: &SwapReceipt) -> Result<SwapReceipt, ProjectorError>;
}
```

- Preflight requires old generation + new generation + 25% disk headroom and records evidence-retention limits before work starts.
- Build reads a frozen vector watermark, writes only a new generation, checkpoints every bounded batch, and resumes by manifest hash.
- Validation checks deterministic counts/hashes, registry coverage, referential/bitemporal/DAG invariants, privacy gates, dead letters, and domain parity.
- Publish atomically updates one manifest pointer after fsync; existing readers retain the old generation until their handles close. A failed publish leaves the old pointer active.
- Rollback atomically points to the previous validated generation. Garbage collection keeps at least two validated generations and honors the bounded data rollback window; generation retention does not expose an old client protocol.

## Built-in registry and complete event-family ownership

| Projector ID | Inputs | Outputs/target |
|---|---|---|
| `canonical_event_v1` | Every registered `ObservationEnvelopeV1.payload_kind` | Immutable canonical events in the observation's owning shard. |
| `identity_alias_v1` | Profile/repository/project/checkout/worktree/provider/actor/agent/session/message/source alias and PR #405/#407 migration events | Entity allocations, aliases, ambiguity candidates, split-identity conflicts, evidence relations. |
| `session_activity_v1` | Session/turn/message/content/model/role/usage/compact markers | `sessions`, `turns`, `messages`, `content_parts`, actor/model relations in `activity.db`. |
| `tool_activity_v1` | Tool catalog/call/result/approval/retry/error events from transcript, hook, MCP analytics, and automation traces | Tool definitions, invocations/results/approvals and direct call-result relations in `activity.db`; project locators separately. |
| `reasoning_artifact_v1` | Provider-exposed summary/analysis/structured plus encrypted/redacted/unavailable markers | `reasoning_artifacts` with actual format, visibility, sensitivity, retention, and coverage; never hidden CoT. |
| `goal_task_v1` | Goal create/update/complete/blocked, task/plan/budget/status events | Immutable goal/task versions, transitions, supersession, agent/session relations. |
| `task_review_lineage_v1` | Plan 24 active PlanVersion/acceptance-edge, review record/correction/validity, failed-transition, remediation/successor, typed-anchor, readiness, and invalidation events | Exact sealed `ReviewLineageViewV1` with source plan/event cursor, effective heads, exclusion reasons, coverage, legal-capability inputs, and nested remediation. Rebuilds from canonical authority, never authors a current cycle, validity decision, or readiness fact. |
| `agent_workflow_v1` | Spawn/start/stop, parent/child, inter-agent message, handoff, roster/run/status events | Agent instances, workflow runs, lifecycle, declared relations, unresolved candidates. |
| `agent_coordination_v1` | Presence/claim/heartbeat/scope/ack/suppress/handoff/completion events | Canonical activity presence/claim histories, current TTL views, scope indexes, declared redundancy, evidence-bearing overlap candidates, project claim locators, and coordination outcomes. |
| `activity_project_locator_v1` | CWD/project/repository/worktree/branch/snapshot hints and Git evidence | Zero-to-many evidence-bearing locators in activity plus safe locator rows in project shards; no message copy. |
| `lcm_context_v1` | Raw/source/summary/compression/context/payload/lifecycle/tombstone events | LCM raw locators, summary DAG/source ranges, compression/replay lineage, payload state in activity. |
| `git_delivery_v1` | Repository/checkout/worktree/ref/commit/PR/check/review/release/fetched events | Canonical project delivery rows and evidence-scored activity attribution. |
| `code_evidence_v1` | Snapshot/file/symbol/edge/diff/diagnostic/test/build/result/impact events | Project rows and immutable graph generations; build plans are produced by plan 25's `tracedecay-code-index` and executed here. |
| `knowledge_v1` | Fact/version/entity/decision/contradiction/trust/retrieval/feedback/curation/deletion events | Immutable knowledge/version/deletion lineage in activity for profile/zero-project/cross-project/unresolved scope or project for explicitly project scope. |
| `policy_hint_v1` | Hook invocation; hint candidate/emitted/suppressed/deduped/cooldown/escalated/budget; retrieval/routing/diagnostic/correlation/curation/scheduler/memory evaluations; terminal outcomes | Versioned policy/hint rows, terminal state, adoption/outcome horizon, and provenance in the `DeclaredScope` owner. |
| `automation_v1` | Job/input-contract/dependency/schedule/relevant-frontier/dirty/admission/deferred/skip-episode/run/agent/artifact/candidate/autonomy-decision/automatic-effect/recovery/skill/fact/use/outcome events plus imported legacy approval/apply events | Automation/skill/curation lifecycle rows, per-job/scope dirty index and current/considered/consumed/included frontier cursor, coalesced skip-episode projection, and immutable artifact locators in the `DeclaredScope` owner; V2 approval queues, fake tick runs, and periodic all-scope scans are forbidden. |
| `operations_v1` | Host bundle/package/component generation, install/enable/trust/restart/cache/capability-probe/compatibility state, installation/package owner, skill drift, doctor finding/remediation, lifecycle lease, drain, checkpoint, service state, daemon/update/repair events | Versioned installation/component/probe current views, omissions/difference ledger, ownership-aligned actionable/info findings, and operation lifecycle read models. Paths/config bodies/credentials remain protected root artifacts; no remediation is emitted unless its capability and effect owner match. |
| `accounting_v1` | Token/context/latency/model/tool/cost/savings/cap/error/data-quality events | Evidence-bearing ledgers and denominator-aware accounting rows in activity or project according to source/scope, with All rollups separate. |
| `search_document_v1` | Eligible non-code entity/message/knowledge/automation event versions | Redaction-gated `search_documents` and representation eligibility/source metadata in the canonical entity owner only. Code file/symbol text is indexed exclusively in generation-local `gen_fts`; registry validation rejects dual representation at the same entity grain. |
| `all_scope_rollup_v1` | Domain outboxes above | Project/day/kind/provider/model/tool/hint/automation/health/cost facets with full vector watermark. |
| `profile_atlas_v1` | Registered structural entity/relation/aggregate projections at a sealed profile vector watermark | Stable multiscale atlas generation, zoom-band tiles, aggregate membership, label candidates, neighbors, and predecessor anchor lineage. Ordinary activity changes do not rebuild it. |
| `read_model_facets_v1` | Domain outboxes above | Per-scope `facet_rollup_rows` in the owning shard for plan 05 facet requests. |
| `read_model_timeline_v1` | Canonical events and gap/late markers | Bitemporal `timeline_density_rows` in the owning shard for plan 05 timeline pages. |
| `read_model_observatory_v1` | Checkpoint/lag/dead-letter/identity-conflict/coverage/operations events | `observatory_status_rows` (activity for profile-wide, project for per-project) for the dashboard Observatory. |

### Query read models: facets, timeline density, and observatory status

The `read_models/{facets,timeline,observatory,profile_atlas}` family produces the projected read models plan [`05-query-crate.md`](05-query-crate.md) consumes through query ports. Facets, timeline density, and observatory are derived current-generation rows. The atlas is an immutable manifest generation rebuilt only when the registered structural-change threshold, algorithm/ontology version, or explicit maintenance operation requires it; ordinary snapshot/activity deltas overlay current evidence without moving geometry. All carry their full source `VectorWatermark` and hold no payload text — labels are `CatalogSafeText`/`LogSafeText` sink-eligible refs only.

- `profile_atlas_v1` seals the Section plan-02 §11.5 row family: deterministic seed and algorithm version, zoom-band tile geometry, parent/child and neighbor sets, exact or declared aggregate membership/counts, importance-ranked label refs, scope contours, coverage, and predecessor-generation anchor mapping. Publication fails unless every predecessor tile has a mapped or explicit retired/split/merged disposition and the object-constancy budget is computed. It does not duplicate node/edge truth or produce a dashboard-specific graph response.

- `facet_rollup_rows(facet_key_id: FacetKeyId, scope_digest: ScopeSelectorDigest, entity_kind: RegistryKind, bucket_value_hash: PrivacyDomainBoundLocatorDigest, bucket_label: CatalogSafeText, count: u64, source_watermark: VectorWatermark, projector_version: ProjectorVersion, updated_at: UtcMicros)`. Primary key `(facet_key_id, scope_digest, entity_kind, bucket_value_hash)`; required index `(scope_digest, entity_kind, count DESC)`. The scope digest binds the canonical resolved selector recorded by the build manifest; the bucket hash is keyed within the owning privacy domain and cannot correlate scopes/domains. Owning shard: the activity/project shard owning the counted rows; All-scope facets remain `all_scope_rollup_v1` output. Size envelope: at most 1,000 buckets per `(facet_key_id, scope_digest, entity_kind)` — plan 05's facet-bucket cap — with an explicit `other` overflow bucket; retention: replaced in place per generation, no history.
- `timeline_density_rows(scope_digest: ScopeSelectorDigest, lane_kind: TimelineLaneKind, time_basis: TimeBasis /* occurred | ingested */, bucket_width: BucketWidth /* minute | hour | day | month */, bucket_start: UtcMicros, event_count: u64, first_event_id: EventId, last_event_id: EventId, source_watermark: VectorWatermark, projector_version: ProjectorVersion, updated_at: UtcMicros)`. Primary key `(scope_digest, lane_kind, time_basis, bucket_width, bucket_start)`; required index `(scope_digest, bucket_width, bucket_start)`. Owning shard: the shard owning the bucketed events. Size envelope: four widths over the event horizon (bounded by retention), sized for plan 05's server-side density buckets and the dashboard's 250k-density-mark budget; retention: derived, rebuilt, no history.
- `observatory_status_rows(subsystem: ObservatorySubsystem /* capture | spool | journal | projector | graph | blob | catalog | migration | privacy | provider_integration | daemon */, component_id: ComponentId, scope_digest: Option<ScopeSelectorDigest>, status: ObservatoryStatus /* healthy | degraded | stale | blocked | unavailable | foreign_owned | unknown */, lag_events: u64, lag_seconds: u64, open_dead_letters: u64, identity_conflicts: u64, coverage: ProjectionCoverage, evidence_anchors: Vec<RetrievalAnchorId>, last_verified_at: Option<UtcMicros>, source_watermark: VectorWatermark, projector_version: ProjectorVersion, updated_at: UtcMicros)`. Primary key `(subsystem, component_id, scope_digest)`; required index `(status, updated_at)`. Owning shard: activity for profile-wide rows, project for per-project rows. Size envelope: one row per live component (thousands, not millions); retention: current view only — history stays in the underlying events. Counts, IDs, and coverage only; it feeds the Observatory surfaces in [`11-dashboard-frontend.md`](11-dashboard-frontend.md) and never renders a metric from missing denominators as zero.

### Tool surface completeness

`tool_activity_v1` must preserve the normalized kind and the original provider kind for:

- Codex `function_call`, `function_call_output`, `custom_tool_call`, `custom_tool_call_output`, `local_shell_call`, `tool_search_call`, and `web_search_call` response items.
- Claude `tool_use` and protocol `tool_result` blocks, including parent tool-use IDs for subagents.
- Cursor agent/composer tool dispatch, invocation, result, edit, and plan rows.
- Hook pre-tool/post-tool events for Codex, Claude, Cursor, and Kiro, including failure/retry/latency metadata.
- MCP tool-call analytics, names/categories, hint-expected tool outcome correlation, and unknown future tool names without schema promotion.
- Automation backend tool traces and artifact references.

Pair calls/results by provider-declared call ID. Missing call or result remains an unpaired invocation/result with coverage state; time adjacency never pairs them. Large arguments/results remain payload references with authorized previews.

### Graph-of-graphs projection model

The product graph is a set of joined, bounded graphs over shared canonical IDs, not one unbounded physical graph. `RelationAssertionV1` is the sole canonical entity-edge authority; thread/session, copy, and assertion-lineage tables are rebuildable typed indexes that cite a source relation ID and cannot carry independent evidence/confidence/history:

1. **Thread/session graph:** canonical `ThreadId` -> evidence-bearing `RelationAssertionV1` (`thread_session_index` for lookup) -> `SessionId` -> ordered Turn entities -> messages/content parts. Native thread/session/turn IDs and ordinals remain aliases/queryable; one provider thread may span sessions and one imported session may have ambiguous thread candidates without collapsing either identity. Generic chats require no project.
2. **Agent graph:** actor -> agent instance -> presence/work claim -> parent/child spawn -> inter-agent message -> handoff -> workflow membership/lifecycle. Claim scope connects repo/worktree/ref/PR/file/symbol/query anchors; provider/user declarations remain distinct from inferred material-overlap candidates.
3. **Turn activity graph:** each canonical Turn is the hub for human/assistant messages, provider-exposed reasoning artifacts, tool invocations/results/approvals, file operations, goals/tasks, usage, diagnostics, tests, and produced artifacts.
4. **Provider workflow graph:** Claude workflow runs, roster agents, journal status, results, and handoffs retain Claude/native kinds while also projecting canonical `WorkflowRun`, `AgentInstance`, and `Handoff` entities. Codex goals retain create/update/complete/blocked, objective, budget, and status semantics while also projecting canonical `Goal`/`Task` versions. Neither provider model is forced into the other.
5. **Evidence graph:** Turn/session/agent/workflow/goal entities cross-link through `RelationAssertionV1` to timeline events, worktrees/branches/commits/PRs/checks, code snapshots/files/symbols/diagnostics/tests, facts/retrieval/feedback/memory versions, hints/policy evaluations, and automation artifacts/outcomes.

The timeline is an ordered view over canonical events and graph relations, not a separate source of truth. A Turn uses a provider-native turn ID when available; otherwise its stable identity derives from canonical session ID plus native ordinal/source position. Multiple messages or tool events may belong to one Turn. Late records append versions/relations and keep occurred/ingested ordering evidence; they never renumber established Turns.

Code graph federation has an additional hard key: `(repository, checkout, optional worktree, optional ref, snapshot, graph generation, source watermark)`. `code_evidence_v1` emits and validates that tuple for every graph occurrence/index row. A ref is a movable locator and may share a snapshot/generation with other refs; it never owns a database. Cross-repository and cross-generation edges are separate evidence-bearing relations with both endpoint tuples and independent freshness. Missing, stale, quarantined, or ambiguous tuple components remain coverage/candidates. Neither the active base checkout nor the currently published graph may substitute for a selected PR worktree/ref/generation.

Hermes concepts project at two levels:

- Host/user/agent/automation identities become explicit `Actor` and `AgentInstance` entities with source aliases; the user-profile consolidation from PR #407 governs storage ownership.
- Session reflector, skill writer, memory curator, combined review, and related automation runs become canonical `AutomationRun`/`WorkflowRun` entities while retaining native task/backend/status labels.
- Candidate, validation, autonomy-decision, automatic-effect, archive, fact, skill, artifact, feedback, adoption, outcome, and recovery records form V2 curation/self-improvement evidence chains. Imported historical/provider proposal/approval/rejection/apply events remain distinct labeled predicates with direct evidence but never become a V2 gate.
- Adoption/effectiveness is never inferred merely because a skill/fact existed before a later session; policy/outcome projectors require the recorded usage, retrieval, feedback, or labeled evaluation evidence.

Required canonical Turn relations are `part_of_session`, `performed_by`, `contains_message`, `contains_reasoning_artifact`, `invoked_tool`, `received_tool_result`, `observed_goal`, `touched_file`, `observed_git_object`, `retrieved_fact`, `emitted_hint`, `part_of_workflow`, and `produced_artifact`. Registry endpoint/evidence rules define each inverse and legal evidence class.

### Reasoning and replay manifests

- `reasoning_artifact_v1` accepts `summary`, `analysis_text`, `structured`, `encrypted`, or `unavailable` exactly as captured. It never relabels analysis text as a summary.
- Encrypted/redacted/unavailable inputs produce a coverage row and no plaintext. Secret/reasoning content is rejected from `search_document_v1` and representation eligibility by invariant.
- Every replayable projection records `ProjectionReplayManifestV1`: input vector, observation/event IDs and hashes, capture manifest digest, registry/schema/predicate/projector/builder versions, policy/evaluator/config/index/memory/tool-catalog digests when relevant, output manifest, substitutions, and unavailable inputs.
- `ExactDeterministic` requires matching executable projector/builder and all input hashes. `RecordedResult` exposes the stored generation without re-execution. `CurrentBestEffort` creates a new noncanonical comparison generation and reports each substitution; it never replaces the historical generation.

## V1 seam and compatibility map

| V1 seam/surface | Projector owner | Required parity |
|---|---|---|
| `src/global_db.rs` sessions/messages/parse offsets/analytics and session search | `session_activity_v1`, `tool_activity_v1`, `accounting_v1`, `search_document_v1` | Session/message counts, roles/kinds/order/text hashes, parent fields, tools, search documents, usage, caps. |
| `src/sessions/codex.rs`, `claude.rs`, `cursor.rs`, `cursor_composer.rs`, remaining providers | Activity projectors | Provider-native order, metadata, tools/results, reasoning markers, goals/plans, parents/subagents, Git/project hints. |
| `src/sessions/lcm/{raw,schema,dag,compression,payload,query,gc}.rs` | `lcm_context_v1` | Raw/source/summary enumeration, DAG/ranges, payload hash/coverage, compression decisions, replay, lifecycle/tombstones. |
| `src/sessions/git_correlation.rs` and `src/daemon/git_watch.rs` | `activity_project_locator_v1`, `git_delivery_v1` | Direct commit evidence, worktree spans, inferred/heuristic confidence, fetched-at state, unresolved candidates. |
| `src/sessions/{workflow_ingest,workflow_index,workflow_state}.rs` | `agent_workflow_v1` | Claude/native workflow runs, roster agents, parent/session links, status, result summary, tokens, messages, and handoffs. |
| `src/hooks/{codex,claude,cursor,kiro,analytics,hint_outcomes}.rs` | `policy_hint_v1`, `tool_activity_v1`, `agent_workflow_v1`, `accounting_v1` | Invocation duration, emitted/suppressed/escalated terminal state, expected tool, adoption horizon, parent/child/inter-agent/tool outcomes. |
| `src/automation/{config,scheduler,runner,run_ledger,artifact_payloads,managed_skills,outcomes}.rs` | `automation_v1`, `agent_workflow_v1`, `policy_hint_v1`, `knowledge_v1`, `accounting_v1` | Effective config/source, due/skip/lock, Hermes actors/runs/agents, artifacts/hashes, V2 curation candidate/validation/autonomy-decision/effect/recovery, imported legacy approvals/applies, skill/fact outcomes. |
| Existing session/LCM/Git/workflow/analytics/automation CLI and MCP handlers | Differential fixtures and temporary internal shadow adapters over `tracedecay-query` | Preserve behavior as parity evidence until cutover; publish only current protocol/catalog handlers afterward. Stale clients/names fail exact version/capability checks. |

Projectors must consume the machine-readable PR 3 compatibility inventory. CI fails when a new V1 structured event kind, provider tool kind, CLI/MCP field, LCM table/sidecar, hook terminal state, or automation artifact kind lacks a registry owner and parity disposition.

Planning began at `99ad19bc`. The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). Identity split/consolidation/retirement, branch/session variant preservation, edit conflicts, proxy and lifecycle transitions, registry repair, FTS maintenance, graph-checkpoint safety, catalog notifications, fact retrieval, and aggregate-before-sample behavior require projected receipts/status without inferred success or replay. Before each backfill/cutover PR, refresh master/open state, source/projector registry digests, and actual protocol/schema/tool inventories.

## Ownership and identity rules

- Profile `activity.db` owns provider observations/events, actors, agents, sessions, turns, messages/content, tools/results, reasoning, goals, workflows/handoffs, LCM/context, cross-project hook/policy/accounting, zero-project chats, and profile/zero-project/cross-project knowledge, skills, policy, automation, saved-view content, and annotations.
- Canonical repository/privacy-domain `project.db` owns project observations/events, Git/delivery, code, project-scoped knowledge/facts/policy/automation, project search/rollups, and opaque activity locators.
- Scope-sensitive knowledge/policy/skill/automation kinds require `DeclaredScope`; reuse across projects produces evidence relations, never copies or a fabricated primary project.
- A session can have zero, one, or many project relations. No projector writes a required `project_id` onto canonical transcript rows or copies message bodies into project shards.
- `sessions.project_key`, Claude first CWD, process/current CWD, active base checkout, and current branch are candidate evidence only. PR/worktree graph selection requires the explicit checkout/worktree/ref/generation relation; ambiguity remains candidates and every cross-project relation carries freshness/coverage.
- PR #405 legacy-store adoption supplies the durable manifest-backed identity. `identity_alias_v1` adopts a unique legacy shard, preserves moved/symlink/linked-worktree aliases, atomically retargets only pristine cutover identities, and dead-letters nonempty split identities as `OwnershipConflict` for explicit consolidation.
- PR #407 consolidates Hermes runtime data into the ordinary user profile. `~/.hermes` remains source provenance; no Hermes-profile shard is minted. Sessions/LCM and profile/zero-project/cross-project facts, skills, policy, and automation resolve to `activity.db`; explicitly project-scoped equivalents resolve to that canonical repository's `project.db`; unresolved scope remains activity-owned evidence until superseded. Backfill preserves PR #407's idempotent migration ledger and collision evidence.
- PR #410 preserves all native transcript observations. Projectors emit versioned message-origin classification and `representative_of` relations with classifier version/evidence; representative, human, direct-user, subagent, tool-result, and protocol views retain hidden-row counts and raw locators.

## Deterministic backfill sequence

1. Freeze a copied V1 inventory watermark at the current normative publication snapshot; record every accepted change from master §2.6 and plan 13; run disk preflight and secret scan.
2. Import identity allocations, profile/repository/project/checkout/worktree/provider/source aliases, legacy adoption manifests, and Hermes migration ledgers; resolve no ambiguous identity automatically.
3. Project canonical observations to events, then sessions, turns, messages/content, actors/models, tools/results/approvals, exposed reasoning markers, goals/tasks/plans, agent/workflow/handoff/inter-agent events in `activity.db`.
4. Project LCM raw/source/summary DAG, compression/context assembly, payload state, lifecycle, and tombstones after canonical message IDs exist.
5. Project activity-to-project/repository/worktree/branch/snapshot locators; publish candidate relations separately from resolved relations.
6. Project Git/delivery, then code snapshots/files/symbols/edges/diffs/diagnostics/tests/impact because delivery attribution depends on repository/worktree identity.
7. Project knowledge/facts/trust/retrieval/feedback/deletion, then policy/hint evaluations/outcomes because policy evidence may reference sessions, tools, code, and facts.
8. Project automation/skills/artifacts/candidates/autonomy decisions/effects/recovery/outcomes after session, knowledge, policy, and project ownership exist; import historical approvals/applies as evidence only.
9. Project accounting/data-quality ledgers, search documents/representation eligibility, and All-scope aggregates with the full input vector.
10. Generate whole-system counts/hashes/orphan/quarantine/dead-letter/coverage manifests, differential V1/V2 results, and an atomic-swap receipt; unexplained differences block publication.

Each step is independently resumable by `(projector, version, shard, generation, contiguous sequence)` and leaves the active generation unchanged until final validation.

## PR and task sequence

### PR 10: Projector contract, registry, checkpoints, and outbox consumption

**Files:** create `Cargo.toml`, `src/{lib,error,projector,registry,outbox,checkpoint,progress,coordinator}.rs`, `tests/framework_suite.rs`; modify workspace `Cargo.toml`.

- [ ] Write failing tests named `registry_rejects_dependency_cycle`, `registry_rejects_unowned_input_kind`, `duplicate_input_has_one_effect`, `checkpoint_and_effect_commit_atomically`, `outbox_gap_blocks_contiguous_checkpoint`, `independent_shards_run_concurrently`, `same_source_sequence_is_stable`, and `vector_watermarks_report_incomparable`.
- [ ] Add the public contracts above and register an in-memory fixture projector for observations/events.
- [ ] Implement dependency planning, leases, bounded adaptive batches, idempotency guards, transactional checkpoints/outbox, gap handling, pause/resume, and vector publication.
- [ ] Add architecture lint rejecting V1/transport/dashboard imports and a registry snapshot test with stable IDs/versions.
- [ ] Run `cargo test -p tracedecay-projectors --test framework_suite`; expected: exit 0 and all eight named contracts pass.
- [ ] Run `cargo clippy -p tracedecay-projectors --all-targets --all-features -- -D warnings`; expected: exit 0 with no warnings.
- [ ] Commit `feat(projectors): add deterministic projection framework`.

### PR 10A: Privacy sink firewalls and descendant lineage

**Ordering:** execute after plan 18 PR 4B, store PR 6B, capture PR 7A, and projector PR 10. This is the projector-owned slice of [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md).

**Files:** create `src/privacy.rs`, `tests/privacy_sink_firewall.rs`; extend registry/manifest contracts only through the plan 18 taint and receipt types.

- [ ] Write failing tests `missing_or_incomplete_receipt_blocks_sink`, `search_and_prompt_sinks_require_distinct_eligible_types`, `descendant_lineage_rebuilds_after_finding`, `dead_letter_details_are_log_safe`, and synthetic canaries for session/FTS/representation/code/knowledge/policy/automation/analytics/cache projectors.
- [ ] Require the exact sink-eligible wrapper for every output, validate sanitizer receipt/policy/detector/source identity, record descendant lineage, and invalidate/rebuild all descendants after a finding. Projectors validate eligibility; they never detect, redact, hash, preview, or recover a candidate secret.
- [ ] Run `cargo test -p tracedecay-projectors --test privacy_sink_firewall`; expected: every ordinary eligible projection passes, incomplete/revoked/unknown inputs remain typed blocked coverage, and no synthetic plaintext or candidate digest reaches a forbidden sink/log/error.
- [ ] Commit `feat(projectors): enforce privacy sink firewalls`.

### PR 10B: Dead letters, rebuild generations, atomic swap, and recovery

**Files:** create `src/{dead_letter,rebuild,manifest}.rs`, `tests/recovery_suite.rs`, `benches/projectors.rs`; extend `tests/framework_suite.rs`.

- [ ] Write failing tests for every dead-letter disposition, retry exhaustion, frozen-watermark rebuild, deterministic second rebuild, crash after each batch, validation failure, crash before/after pointer fsync, readers pinned to old generation, rollback, and insufficient disk.
- [ ] Implement immutable dead letters/resolution receipts, disk preflight, resumable build checkpoints, deterministic manifests, validation, atomic publish, generation leases, rollback, and deferred GC.
- [ ] Prove unresolved blocking dead letters prevent publish and quarantined optional omissions remain visible in coverage/manifests.
- [ ] Run `cargo test -p tracedecay-projectors --test recovery_suite`; expected: exit 0; every injected crash leaves the old generation active or a fully validated new generation active.
- [ ] Run `cargo bench -p tracedecay-projectors --bench projectors -- rebuild`; expected: deterministic output digest across two runs and throughput/peak-RSS/disk-amplification recorded.
- [ ] Commit `feat(projectors): add atomic rebuild and recovery`.

### PR 10C: Canonical events, identity, and schema/predicate enforcement

**Files:** create `src/{canonical_event,identity}.rs`; extend `tests/framework_suite.rs` and `tests/backfill_parity.rs`.

- [ ] Write failing tests for exact event IDs, correction supersession, unknown future schema, illegal relation endpoints, causal evidence rules, ambiguous alias candidates, moved/symlink/linked-worktree identity, pristine legacy adoption, nonempty split conflict, and Hermes user-profile ownership.
- [ ] Implement `canonical_event_v1` and `identity_alias_v1` with complete capture payload-kind ownership.
- [ ] Refresh the normative publication snapshot, record every accepted commit/protocol/schema version, and record any newly open implementation inputs before execution.
- [ ] Run `cargo test -p tracedecay-projectors --test framework_suite`; expected: exit 0 with no unowned capture kind.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity identity`; expected: one canonical identity per adopted/Hermes fixture and explicit conflict rows for nonempty collisions.
- [ ] Commit `feat(projectors): project canonical events and identity`.

### PR 17: Sessions, tools, reasoning, goals, and concurrent agents

**Files:** create `src/activity/{mod,sessions,tools,reasoning,goals,agents}.rs`, `tests/activity_suite.rs`; extend `tests/backfill_parity.rs`.

- [ ] Write provider fixtures for every session/message/content/tool kind, reasoning format, goal status, parent/child/inter-agent/handoff event, duplicate/gap/fill/late marker, generic session, copied prompt origin, and unknown provider event.
- [ ] Implement `session_activity_v1`, `tool_activity_v1`, `reasoning_artifact_v1`, `goal_task_v1`, and `agent_workflow_v1` using the ownership rules above.
- [ ] Assert every fixture builds the five graph views above: thread/session, parent-child/handoff, Turn activity, provider workflow/goal, and evidence cross-links to timeline/Git/code/memory.
- [ ] Assert independent agents project concurrently, producer sequence is stable, display order is deterministic, and no timing-only causal link appears.
- [ ] Assert reasoning plaintext exists only for provider-exposed artifacts and never enters search/export eligibility by default.
- [ ] Run `cargo test -p tracedecay-projectors --test activity_suite activity`; expected: exit 0 for Codex, Claude, Cursor, Composer, Cline-like, Hermes, Kiro, Vibe, and hook fixtures.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity sessions`; expected: V1/V2 counts/order/hashes/tools/goals/subagents reconcile with explicit transforms only.
- [ ] Commit `feat(projectors): project sessions and agents`.

### PR 17A: Profile activity, temporal project attribution, and work claims

**Files:** create `src/activity/{coordination,project_locators}.rs`; extend `tests/activity_suite.rs` and `tests/backfill_parity.rs`; add the shared coordination and cross-project scope manifests.

- [ ] Write failing fixtures for profile-canonical activity; zero/one/many repositories per session; per-observation provider CWD/tool workdir/explicit query/worktree/ref/snapshot evidence; parent/subagents in parallel worktrees; copied prompts; `sessions.project_key` conflict; first-CWD drift; base-checkout-versus-PR-worktree graph conflict; and stale/ambiguous registry candidates.
- [ ] Project `produced_in`, `executed_in`, `queried`, `discussed`, `observed`, and bounded `primary` candidate relations with validity/knowledge intervals, evidence class, confidence/rationale, and abstention. Never copy canonical transcript bodies into project shards or write a required session project.
- [ ] Project `agent_coordination_v1`: presence, work claims, repository/worktree/ref/PR/file/symbol/query scopes, intent, optional safe summary, retrieval anchors, redundancy mode, heartbeats/TTL/current view, acknowledgement, suppression, handoff, completion, and outcome counters. TTL changes current state without deleting history.
- [ ] Freeze the current parent prefix `019f4906`, PR #359 children `agent-ac3ce9b1ebf998cfb`, `agent-a245d2442cefc621d`, `agent-a96d21dc6391ceba8`, `agent-a6661fd133491631c`, and Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d`; prove the prefix resolves uniquely in the fixture manifest.
- [ ] Assert deliberate ensemble/diverse review/shared execution/sequential handoff remains planned redundancy; accidental overlap is only an evidence candidate. Same worktree/time never becomes causation, duplicate-work fact, cancellation, lock, reassignment, or messaging authority.
- [ ] Run `cargo test -p tracedecay-projectors --test activity_suite coordination_attribution`; expected: scope/TTL/redundancy/evidence cases pass and no project message copy exists.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity sessions_cross_project`; expected: native/profile counts stay lossless and every historical project attribution has an explicit disposition.
- [ ] Commit `feat(projectors): project profile attribution and work claims`.

### PR 17B: LCM/context lineage and replay state

**Files:** create `src/lcm/{mod,raw,dag,compression,payloads}.rs`; extend `tests/activity_suite.rs` and `tests/backfill_parity.rs`.

- [ ] Write fixtures for sanitized-native message/source enumeration, external payload ranges, summary fan-in/source ranges, nested DAG, compression boundary/decision, context assembly, lifecycle, missing/locked/redacted payload, tombstone, and retention crossing.
- [ ] Implement `lcm_context_v1` after canonical message IDs exist; enforce DAG acyclicity, exact source coverage, payload hash, and tombstone lineage.
- [ ] Publish each V2 summary node, protected content, complete source ranges, claim refs, validated anchor manifest/entries, requested/actual model+effort, model-run receipt, and projector event atomically. A partial node or orphan manifest is quarantined, never queryable.
- [ ] Propagate source correction, redaction, deletion, lock, retention, authorization, and horizon changes through the transitive summary DAG. Mark affected nodes stale/ineligible without rewriting them; a policy-approved refresh creates a successor node whose manifest cites current eligible sources.
- [ ] Build exact/recorded/best-effort projection replay manifests and prove unavailable retained inputs are reported instead of substituted silently.
- [ ] Run `cargo test -p tracedecay-projectors --test activity_suite lcm`; expected: exit 0 and all source ranges/hashes/DAG edges match the fixture manifest.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity lcm`; expected: V1 raw/summary/payload/compression counts and hashes reconcile with zero unexplained omission.
- [ ] Commit `feat(projectors): project lcm lineage`.

### PR 18: Code snapshots, diagnostics, tests, and impact

**Files:** create `src/code/{mod,snapshots,graph,lineage}.rs`; extend `tests/domain_suite.rs` and `tests/backfill_parity.rs`.

- [ ] Write fixtures for immutable snapshots, dirty overlays, file/symbol occurrences, rename/move/split/merge candidates, code edges, diffs, diagnostics, builds, tests/results, coverage/test map, impact evidence, and two refs sharing one immutable snapshot/generation.
- [ ] Define the projector-owned `CodeIndexBuildPortV1` consumer contract and land `code_evidence_v1` against a deterministic fake builder. Project canonical snapshot/file/diagnostic/test evidence into project rows, but do not wire the production language extractor or packed-generation builder in this PR.
- [ ] Exercise the complete two-phase protocol with the fake builder: commit a durable build request and source fence; build, stream, seal, fsync, and verify the immutable generation outside SQLite writer transactions; then use one short idempotent transaction to CAS the request/source fence, publish the verified manifest/pointer, advance the projector checkpoint, and retain the previous generation for rollback. Every row retains the explicit repository/checkout/worktree/ref/snapshot/generation tuple and ambiguous lineage stays candidate evidence.
- [ ] Add `ref_move_does_not_mutate_old_snapshot_binding` and prove base snapshot, dirty overlay, and graph generation coverage remain distinguishable.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite code`; expected: exit 0 and graph manifest is deterministic.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity code`; expected: V1 graph/search/impact/test-map golden cases match or carry explained evidence-version changes.
- [ ] Commit `feat(projectors): project code evidence`.

### PR 18A: Cross-repository graph federation

**Files:** create `src/code/federation.rs`; extend `tests/domain_suite.rs` and `tests/backfill_parity.rs`; extend `benches/projectors.rs`; add redacted Rspack/Rsbuild/React Router upstream/plugin/downstream scope fixtures.

- [ ] Write failing tests `selected_pr_worktree_never_reads_base_generation`, `multi_repo_generations_federate_without_identity_collapse`, `stale_registry_generation_is_coverage_not_fallback`, `base_commit_index_never_claims_dirty_working_copy_coverage`, `cross_repo_edge_keeps_both_endpoint_tuples`, and `federated_merge_preserves_per_repository_diversity`.
- [ ] Implement compatible selection by repository/checkout/worktree/ref/commit/dirty-overlay/snapshot/generation; emit source/freshness/partial explanations and bounded cross-repository joins without copying canonical graph rows.
- [ ] Preserve direct change, structurally impacted, candidate test, and context-only roles separately; a cross-repository edge never increments direct-change counts without direct evidence.
- [ ] Benchmark 2/8/32 repositories and incompatible/stale/partial generations with fixed node/edge/result budgets; record p50/p95, opened generations, peak RSS, truncation, and per-repository result share.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite code_federation`; expected: all exact-scope and coverage cases pass.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity code_federation`; expected: every V1 local graph result has a disposition and no base/current graph fallback appears.
- [ ] Commit `feat(projectors): federate repository graph generations`.

### PR 18G: Wire the production code-index builder into `code_evidence_v1`

**Ordering:** after plan 25 PRs 18B–18F and plan 02 PR 6C; this is the only post-builder integration step. PR 18 is never reopened or resumed after merge.

**Files:** extend `src/code/{mod,snapshots,graph,lineage}.rs`; create the narrow root composition adapter; extend `tests/domain_suite.rs`, `tests/backfill_parity.rs`, and generation fault tests.

- [ ] Keep projector code dependent only on its consumer-owned `CodeIndexBuildPortV1`. Implement the production port in root `src/v2_adapters/code_index.rs` using plan 25's `CodeIndexBuilderV1` plus plan 02's `GenerationWriter`; no `tracedecay-projectors -> tracedecay-code-index` dependency is permitted. The root adapter adds no extraction, identity, digest, publication, or fallback semantics.
- [ ] Keep plan 02 `GenerationWriter` ownership in the projector workflow, never across the long-lived SQLite writer transaction. The production builder receives a durable request, receipt-bound sanitized inputs, and a bounded row sink; it never opens/publishes a store. Build/seal/fsync occur outside SQLite; only verified manifest publication plus checkpoint/source-fence CAS use the short commit transaction. Root never implements a semantic builder port.
- [ ] Require production/fake-builder contract parity, deterministic serial/parallel digests, store-manifest agreement, receipt lineage, fault-safe staging/publication, previous-generation rollback, V1 differential parity, and current/10x performance gates.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite code --test backfill_parity code` plus plan 25's extraction/generation/lineage suites; expected: production integration passes without changing the PR-18 projector contract.
- [ ] Commit `feat(projectors): integrate production code indexing`.

### Native semantic generation scheduling companion

**Owner and ordering:** this companion lands inside plan 05 PR 14E after PR 18G, plan 02 PR 6C semantic-generation support, and PR 14A's accepted profile receipt; it completes before accepted PR 14B/14C semantic fusion or rerank routes activate. This extends the existing `code_evidence_v1` operation lane; it creates no projector, crate, scheduler, queue, writer authority, or additional executable slice.

- [ ] Add a consumer-owned `SemanticCodeEmbeddingPortV1` request/result contract using only plan 25's ordered `SemanticCodeDocumentV1`/`SemanticCodeChunkV1` rows and the complete model/tokenizer/runtime/dimension/metric/normalization/formatter/chunk/privacy/key/source-generation pin tuple.
- [ ] On a committed code-generation advance, compare the eligible input digest and complete pin tuple. Reuse unchanged document/chunk vectors; enqueue only changed eligible inputs; force a staged full semantic rebuild for any incompatible pin; coalesce duplicate triggers by target semantic-generation identity.
- [ ] Drive FastEmbed only through root `src/v2/native_semantic_runtime`. Projector tests use a deterministic fake port. Projectors never import FastEmbed, open model artifacts, persist vectors, or compute similarity.
- [ ] Commit a durable semantic build request and source fence, run inference outside SQLite writer transactions, then ask plan 02 to verify and atomically activate the sealed vector generation with the code-generation checkpoint. Failure leaves the last-good semantic generation active and reports stale/unavailable semantic coverage.
- [ ] Add unchanged-document reuse, formatter/chunker/model/tokenizer/runtime-ABI/privacy-key invalidation, duplicate-trigger coalescing, cancellation/restart, partial-batch failure, and no-mixed-generation tests.

### PR 19: Git and delivery evidence

**Files:** create `src/git_delivery/{mod,core}.rs`; extend `tests/domain_suite.rs` and `tests/backfill_parity.rs`.

- [ ] Write fixtures for repository/remote/checkout/worktree/ref/commit/force-push/rebase/PR/check/review/release/fetched-at events and direct/inferred/heuristic activity attribution.
- [ ] Implement `git_delivery_v1` with bitemporal remote state, explicit fetched-at freshness, evidence classes, confidence rationale, and mandatory abstention below calibrated display threshold.
- [ ] Cover PR #405 aliases and linked worktrees without merging clone identities.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite git_delivery`; expected: exit 0 and causal-copy lint rejects inferred “created/changed/caused” labels.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity git`; expected: direct commit/span/worktree evidence reconciles and calibration metrics are emitted.
- [ ] Commit `feat(projectors): project git and delivery evidence`.

### PR 19A: Related-system and delivery graph

**Files:** create `src/git_delivery/related_systems.rs`; extend `tests/domain_suite.rs` and `tests/backfill_parity.rs`; reuse the redacted upstream/plugin/downstream and support-reproduction fixtures from PR 18A.

- [ ] Write failing fixtures for fork/upstream/downstream repository identity, PR head/base, linked worktrees, force-push, patch/backport/cherry-pick candidates, generated/published artifacts, support reproductions, synthetic benchmarks, checks/releases, and missing live delivery evidence.
- [ ] Project explicit `produced`, `published`, `derived_from`, `backport_of`, `reproduces`, `benchmarks`, `observed`, `encountered`, `directly_changed`, `structurally_impacted`, `candidate_test`, and `context_only` relations with source, evidence class, confidence, freshness, and both repository endpoints.
- [ ] Assert temporal adjacency or shared filenames never create causal/produced/backport relations; unresolved forks/patches remain candidates and remote state always carries fetched-at/cap coverage.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite related_systems`; expected: cross-repository graph roles and abstention cases pass.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity related_systems`; expected: local/live differences are named and no inferred impact appears as a direct modification.
- [ ] Commit `feat(projectors): relate repositories and delivery artifacts`.

### PR 20: Knowledge, facts, trust, retrieval, and policy evidence

**Files:** create `src/{knowledge,policy}.rs`; extend `tests/domain_suite.rs` and `tests/backfill_parity.rs`.

- [ ] Write fixtures for fact versions, entities, decisions, contradictions, trust changes, retrieval/feedback, curation/supersession/deletion, hint decision branches, retrieval/routing/diagnostic/correlation/curation/scheduler/memory evaluations, and terminal outcomes.
- [ ] Implement `knowledge_v1` and `policy_hint_v1`; keep evaluation inputs/results immutable and projector logic free of policy execution.
- [ ] Assert every hint has exactly one terminal state, outcome horizons distinguish observed/unobserved/unresolvable, and false temporal attribution creates no relation.
- [ ] Assert PR #407 facts-only and collision migrations obey `DeclaredScope`: profile/zero-project/cross-project rows resolve to activity, explicitly project-scoped rows resolve to the canonical project shard, unresolved rows remain activity-owned evidence, and every result retains idempotent ledger evidence.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite`; expected: exit 0 and every relation exposes evidence/provenance/version.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity knowledge`; expected: fact/trust/retrieval/feedback/hint counts and hashes reconcile.
- [ ] Commit `feat(projectors): project knowledge and policy evidence`.

### PR 21: Automation, skills, artifacts, and outcomes

**Files:** create `src/automation.rs`; extend `tests/domain_suite.rs` and `tests/backfill_parity.rs`.

- [ ] Write fixtures for job/effective config/input contract/trigger class/typed dependency selectors, evidence/time/external/manual frontier→dirty-scope mapping, schedule/due/skip/lock/stale lock, per-thread/project/profile coalescing, finalized-boundary/writer-registry-generation/freshness/coverage/quiet/max-debounce state, current/considered/consumed/included per-shard frontiers, semantic/evaluation-snapshot digests, unknown/partial deferral, identical-terminal-input suppression, coalesced skip episodes, pre-admission considered advance, terminal `NoChange` consumed-frontier advance, late-ingress preservation, failed-input retry retention, poison quarantine, uncertain-effect reconciliation/finalization, self-effect loop suppression, run/agent/status, artifact/hash/payload ref, candidate/validation/autonomy-decision/automatic-effect/recovery, managed skill version/state/materialization target, fact candidate, skill/fact use/outcome, and imported legacy approval/apply evidence.
- [ ] Implement `automation_v1` after activity/knowledge/policy ownership exists. Declared evidence/time/external/manual frontiers upsert only affected job/scope dirty rows with exact per-shard frontiers/reasons; irrelevant events and an automation's own effects cannot dirty it unless a registered downstream feedback/outcome dependency says so. `EvidenceDriven` jobs become dormant after considered no-relevant/unchanged input or terminal `NoChange` until a relevant frontier advances. Equivalent skip decisions update one input-bound episode projection linked to the latest ordinary policy evaluation. Uncertain effect state remains nonterminal until one reconciliation receipt. JSONL/files remain immutable compatibility sources; no clock-tick full scan, fake run, or per-tick ledger append exists.
- [ ] Assert 1,000 unchanged scheduler wakeups leave dirty/cursor/run counts and model/tool-call evidence unchanged while incrementing only the bounded skip-episode/metric rollup; unrelated-project activity touches no row, late ingress remains as the next dirty generation, and 64 concurrent projectors converge on one deterministic frontier/episode state.
- [ ] Assert concurrent automation agents preserve roster/parent/handoff evidence and artifact ownership without copying payload bodies.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite automation`; expected: exit 0 for every current run-ledger and managed-skill enum variant.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity automation`; expected: config/run/artifact/candidate/autonomy/effect/recovery/skill/outcome manifests reconcile and legacy approvals remain evidence-only.
- [ ] Commit `feat(projectors): project automation lifecycle`.

### PR 22: Accounting, privacy-gated search, query read models, and All-scope rollups

**Files:** create `src/{accounting,operations,search,aggregates}.rs`, `src/read_models/{mod,facets,timeline,observatory,profile_atlas}.rs`; extend `tests/domain_suite.rs`, `tests/backfill_parity.rs`, and `benches/projectors.rs`.

- [ ] Write fixtures for tokens/context/compression/latency/model/tool/cost/savings methodology, missing denominator, caps, hook/hint/tool/fact/skill/automation adoption, coordination eligible/emitted/suppressed/acted/handoff/duplicate-avoided/false-positive/unresolved outcomes, generated host core/facade package sets, component/install generation, trust/restart/cache/capability-probe/difference state, stale and missing host surfaces, #411 foreign/self-owned/legacy skill findings and remediation agreement, #412 lifecycle drain/checkpoint/service-state order, malformed/partial sources, sensitivity/retention changes, cross-shard vector watermarks, atlas stability through ordinary activity, deterministic tile/membership/label/neighbor output, and split/merge/retire anchor lineage.
- [ ] Implement `accounting_v1`, `operations_v1`, `search_document_v1`, and `all_scope_rollup_v1`; `operations_v1` reduces installation/component/probe history into exact desired/observed/effective state and preserves omissions/difference/repair receipts without path/config bodies. Numeric ratios require a known denominator and every rollup stores its full source vector.
- [ ] Implement `read_model_facets_v1`, `read_model_timeline_v1`, `read_model_observatory_v1`, and manifest-built `profile_atlas_v1` with the exact store shapes above; assert bucket caps with `other` overflow, occurred/ingested density parity, observatory unknown denominators, deterministic atlas tiles, zero geometry change on ordinary evidence refresh, and complete lineage/object-constancy report before successor publication.
- [ ] Prove secret, reasoning-default, locked, quarantined, and deleted content creates no search document or representation eligibility row; deletion rebuild removes descendants within one minute.
- [ ] Run `cargo test -p tracedecay-projectors --test domain_suite`; expected: exit 0 with zero forbidden index rows and explicit unknown denominators.
- [ ] Run `cargo bench -p tracedecay-projectors --bench projectors -- visibility`; expected: p95 observation-to-projected visibility at or below two seconds under concurrent capture and current-scale corpus.
- [ ] Commit `feat(projectors): add accounting search and rollups`.

### PR 33/34/35: Final backfill, shadow parity, bounded cutover, and rollback

**Files:** extend `tests/backfill_parity.rs`, `tests/recovery_suite.rs`, generated compatibility/parity manifests, and root composition in the execution PR.

- [ ] Execute the ordered ten-step backfill sequence on copied real stores, resume after injected interruption in each step, and produce one whole-system vector/manifests receipt.
- [ ] Shadow V1/V2 session list/message search/LCM replay/Git/workflow/knowledge/hint/automation/accounting reads at frozen watermarks; compare counts, stable order, hashes, filters, caps, denominators, and coverage.
- [ ] Block each bounded-context cutover on unresolved blocking dead letters, unexplained parity, privacy failure, stale/incomparable vector, visibility p95 above two seconds, or failed rollback drill.
- [ ] Publish projectors independently in order: identity/canonical events, activity/LCM, Git/code, knowledge/policy, automation, accounting/search/rollups.
- [ ] Roll back by atomically restoring the prior validated generation pointer and routing compatibility reads back to V1; retain new observations/events and failed build generations for diagnosis.
- [ ] Run `cargo test -p tracedecay-projectors --test backfill_parity --test recovery_suite`; expected: exit 0 and a zero-unexplained-gap, rollback-proven receipt.
- [ ] Run `cargo test --test session_suite --test transcript_ingest_suite --test mcp_suite --test hooks_lsp_suite --test automation_runner_test --test dashboard_api_test`; expected: V1 compatibility suites exit 0 throughout the window.
- [ ] Commit `feat(projectors): prove v2 projection cutover`.

## Compatibility, shadowing, cutover, and rollback rules

- Domain projectors shadow immutable observations while V1 remains authoritative. They never dual-write through V1 storage APIs.
- Differential reads use the same frozen source/vector watermark. Live drift is reported as drift, not parity failure.
- Every difference is classified `exact`, `expected_normalization`, `redacted`, `quarantined`, `v1_bug_compat`, `late_after_watermark`, or `unexplained`; `unexplained` blocks publication/cutover.
- Temporary V1/V2 adapters are internal shadow/rollback machinery only. At cutover, current CLI/MCP/API/dashboard clients use the new protocol/catalog; stale running clients and retired names fail closed with restart/update/current-capability guidance rather than a live compatibility adapter.
- V1 stores stay read-only for one release after verified cutover. Previous validated projection generations stay available through the rollback window.
- Cutover changes the active read generation/context flag only after a signed manifest and rollback receipt exist. It does not rewrite observations/events or delete V1.
- Rollback is an atomic pointer/route change. Forward resumption builds a new generation from immutable inputs; it never mutates the failed or rolled-back generation.

## Release gates

### Determinism and correctness

- Two rebuilds at the same vector with the same registry/schema/projector/builder versions produce identical ordered row hashes, counts, relation sets, dead-letter manifest, and output digest.
- Every capture payload kind and every V1 compatibility-inventory structured family has exactly one canonical-event owner and at least one golden test.
- Every relation has evidence class, supporting IDs, producer version, confidence/rationale when nonexact, temporal intervals, scope, and sensitivity.
- Alias precision is at least 99.5%, recall at least 99%, unresolved/conflict rates are reported, and 100% of ambiguous identities remain visible.
- Symbol-lineage F1 is at least 98%; inferred Git/PR/code expected calibration error is at most 0.05 on the labeled corpus.

### Performance and concurrency

- Projected event visibility p95 at or below two seconds while 128 agent producers and independent projectors run.
- Backfill sustained throughput at least 10,000 messages/events per second excluding embeddings.
- No query-facing active generation observes a partial batch or checkpoint beyond its committed effects.
- One projector/shard failure does not stop unrelated shards/projectors; coordinator reports partial coverage and backpressure.
- Batch/WAL behavior respects the master gates: WAL at or below 1 GiB before checkpoint, rebuild disk amplification at or below 2.25x source data, and peak RSS reported at current/10x corpora.

### Privacy and retention

- Secret corpus produces zero secret-bearing search/vector/fact/fixture/export/log rows.
- Reasoning is provider-exposed only, 30-day default retention, excluded from search/vector/facts/shares/exports by default, and represented by coverage after deletion.
- Locked/redacted/quarantined/missing payloads remain explicit coverage states; no projector substitutes plaintext or current state.
- Deletion projects canonical tombstone, removes FTS/vector descendants within one minute, releases blob references through store outbox, and retains noncontent audit/provenance.

### Recovery

- Kill tests cover outbox read, effect write, output outbox, checkpoint commit, dead-letter write, batch checkpoint, manifest validation, pointer fsync, old-generation lease, rollback, and GC.
- Corrupt/missing/incompatible shards remain named partial coverage; unaffected shards continue.
- Catalog metadata rebuilds from manifests/outboxes; projections rebuild from retained observations/events; FTS/vectors/rollups/graph generations rebuild independently.
- Rebuild pause/resume, disk preflight, failed validation, backup restore, and rollback pass on copied real stores.

### Observability

- Metrics expose outbox head/contiguous/highest/gaps, projector lag/rate/retries/errors, lease owner/age, batch size/backpressure, dead letters/resolutions, generation/build progress, validation/parity, swap/rollback, vector coverage, late/duplicate events, identity conflicts, and privacy omissions.
- Metrics/logs use safe IDs, kinds, versions, counts, and fingerprints; they never include message/tool/reasoning/fact/artifact/query literals.
- Every projection/query response can report projector versions, active generation, input vector, stale/unavailable/incompatible/redacted/quarantined coverage, and evidence-retention watermark.

## Definition of done

- Registry validation proves every captured/V1 structured family and provider tool surface has a deterministic owner.
- Every content-bearing read model, graph label/snippet, search/representation row, aggregate label, replay output, and dead-letter detail passes the one Plan 18 sink firewall and retains sanitization-descendant lineage; projectors never scan, redact, or mint eligibility.
- Canonical activity and project ownership matches the master architecture; canonical transcript bodies exist only in profile activity storage.
- Concurrent parent/subagents, inter-agent messages, tools/results, goals, hooks/hints, and outcome correlations preserve direct ordering/evidence without fabricated causation.
- PR 17A profile activity/temporal attribution/work claims preserve zero/one/many project relations, per-observation validity, safe coordination anchors, planned redundancy, and current TTL views without copying transcripts or granting agent-control authority.
- LCM, Git/code/delivery, knowledge/policy, automation/skills, accounting/search/rollups, and the query read-model family (facets, timeline density, observatory status) rebuild deterministically with explicit vector watermarks.
- Code graph rows and joins are federated by explicit repository/checkout/worktree/ref/snapshot/generation tuples; ambiguity/staleness is coverage and no active-base/current-generation fallback exists.
- PR 18A/18G/19A cross-repository graph, production code-index integration, and related-delivery projections preserve both endpoint snapshots, source/freshness, bounded diversity, and distinct direct/impact/test/context/produced/observed roles.
- PR #405 identity adoption, PR #407 Hermes user-profile migration, PR #410 native-row/origin/representative behavior, PR #411 ownership/remediation agreement, and PR #412 lifecycle-drain receipts are in the recorded base and parity-tested; #413 contributes its actual release/protocol version only and #409 remains historical.
- Exact, recorded-result, and best-effort manifests expose substitutions/unavailable inputs and never reconstruct hidden reasoning.
- Each bounded context has a zero-unexplained-gap shadow receipt, performance/privacy/recovery approval, atomic cutover, and tested one-step rollback.
