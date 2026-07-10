# TraceDecay V2 Store Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a `tracedecay-store` crate that durably implements the V2 catalog, profile activity, project evidence, graph generation, privacy-domain blob, outbox, migration, retention, integrity, backup, recovery, and V1 import contracts under concurrent live ingest.

**Architecture:** One logical brain uses federated SQLite shards and privacy-domain blob roots. Each live shard has one cross-process writer lease, a bounded in-process queue, WAL-backed read snapshots, transactionally coupled domain rows/outbox/source cursors, idempotent replay, and per-shard sequences combined as vector watermarks; immutable graph generations and staged blobs use manifest-driven atomic publication and crash recovery.

**Tech Stack:** Rust 2024; synchronous `rusqlite` API through the already-linked `libsql-rusqlite` 0.9.30 package during V1/V2 coexistence; SQLite WAL/FTS5; `crossbeam-channel`; `fs2`; `serde`; `sha2`/HMAC-SHA-256; `chacha20poly1305`; `zstd`; `thiserror`; `tracing`; `tempfile`, `proptest`, and `criterion` for tests. No `libsql::Database`, remote libSQL, replica, or network API is allowed in V2 store code.

---

## Goals

- Persist the exact `tracedecay-domain` contracts without exposing SQLite row IDs.
- Keep catalog, activity, project, graph, and blob failure/privacy boundaries explicit.
- Support many concurrent host hooks, agents, subagents, automation workers, projectors, queries, and commands without silent loss or a false global order.
- Acknowledge canonical source progress only after observation, event/outbox, and source cursor commit atomically.
- Make duplicates safe, late records visible, gaps durable, rewrites explicit, and cross-shard reads snapshot-addressable.
- Persist deterministic identity evidence and UUIDv7 allocations so rebuild/restore never remints canonical IDs.
- Own all V2 SQLite schema and forward migration chains in this crate.
- Keep catalog rows content-free and blob deduplication inside one privacy/key/retention domain.
- Make retention, deletion, projection replay, backup, repair, graph swap, and blob GC crash-recoverable.
- Import V1 stores, merged PR #405 repository-identity adoption, future PR #407 Hermes profile consolidation, PR #410 native-row/origin/representative semantics, PR #411 foreign-skill ownership, and merged PR #412 lifecycle receipts as read-only, idempotent parity inputs.

## Non-goals

- No provider parsing, classification/redaction policy, projection semantics, query planning/ranking, HTTP/MCP/CLI rendering, dashboard logic, or remote sync.

## Convergence boundary

This crate is the only V2 physical persistence owner inside [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md). It implements domain contracts from [`01-domain-crate.md`](01-domain-crate.md), scope candidate/storage semantics from [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), and the sanitized/protected split from [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md).

| Boundary | Contract |
|---|---|
| Enters | Domain IDs/owners/eligible payloads, sanitized observations, canonical projection commits, registered read fragments, commands, manifests, and explicit frozen watermarks. |
| Exits | Durable allocation/append/projection/command/migration/repair receipts, catalog candidate inventories, read snapshots, graph/blob/quarantine refs, and shard/vector watermarks. |
| Upstream owners | Domain owns values; capture owns sanitization and observation construction; projectors own derived semantics; application owns scope resolution and workflows. |
| Downstream consumers | Capture/projector/query/application ports consume storage capabilities; no transport or UI opens SQLite or paths. |
| Extension seam | New bounded storage capability requires a domain registry entry, typed repository port, migration chain, fault/backup/restore tests, manifest fields, and application adapter; no ad hoc SQL in consumers. |
| Scale/concurrency | One cross-process writer lease per live shard, bounded fair queues, WAL snapshots, immutable graph generations, manifest publication, cancellation, and vector—not global—progress. |
| Migration/retirement | V1 opens read-only through import adapters; after per-domain parity/cutover receipts, V1 serving paths retire and remain bounded rollback evidence only. Duplicate stores remain conflict evidence, never silently selected. |

Store errors describe physical failure only (`busy`, `disk_full`, `corrupt`, `permission_denied`, `missing_key`, `schema_incompatible`, `stale_lease`, `watermark_unavailable`). Application maps them to the canonical public problem vocabulary from Plans 09/17. Store never invents user remediation, rank evidence, or transport retry objects.
- No distributed transaction or scalar sequence spanning shards.
- No canonical message copy in project shards and no memory/fact ownership in branch graph databases.
- No direct mutation of V1 stores, deletion of V1 files, or automatic merge of conflicting legacy identities.
- No one-SQLite-file-per-commit graph layout.
- No trusting advisory blob refcounts as the only GC authority.
- No asynchronous database API. Application/runtime crates call synchronous repositories on owned writer/read workers.

## SQLite and driver decision

V2 uses the synchronous `rusqlite` API. During coexistence, the dependency is declared exactly as:

```toml
rusqlite = { package = "libsql-rusqlite", version = "=0.9.30", features = ["backup", "blob", "functions", "limits", "trace", "uuid"] }
```

This choice shares the SQLite C runtime already linked by V1 `libsql = "0.9.30"`, avoiding two SQLite implementations in one binary. V2 code imports only `rusqlite::{Connection, Transaction, OpenFlags}` and cannot import `libsql`, its async connection, remote URLs, replicas, or sync APIs. A dependency lint and link-smoke test enforce the boundary. After V1 removal, changing the package provider requires a measured storage-driver ADR, on-disk compatibility/integrity proof, backup/restore drill, and performance parity; it is not part of the first V2 default.

Required SQLite behavior:

- `PRAGMA journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `trusted_schema=OFF`, `busy_timeout=5000`, `wal_autocheckpoint=1000`, and bounded negative `cache_size` selected by file size.
- Read-only opens use `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`, `query_only=ON`, and a deferred transaction only for the duration of one request page.
- The writer connection is owned by one thread and is never shared through a mutex.
- Every mutation uses `BEGIN IMMEDIATE`; retry uses bounded jittered backoff and never hides a timeout behind success.
- WAL checkpoints are passive during normal work and `TRUNCATE` only under backup/maintenance lease after proving no busy frames.
- FTS5 is used only for classified, index-eligible content.

## Current V1 and incoming migration seams

| Seam | Exact source | Store-plan consequence |
|---|---|---|
| V1 physical layout | `src/storage.rs`: `StoreLayout`, `StoreManifest`, `PrivateStoreIo`, `resolve_layout`, `write_store_manifest`, response-handle and LCM payload roots | Import manifest/path identity as provenance. Reuse private/root-contained I/O rules; publish a new V2 layout manifest without rewriting V1. |
| Repository identity adoption | PR #405: `src/storage.rs::matching_legacy_profile_layouts`, `retire_identity_cutover_manifest`; `src/tracedecay/lifecycle.rs::resolve_store_layout_with_identity_migration`, `choose_identity_layout`, `store_identity_inventory`; tests in `tests/storage_suite/storage_resolver_test.rs` | Import candidate inventories and adoption/retirement receipts. Healthy/pristine cutover can map one source to one V2 repository; ambiguous or nonempty conflicts create separate identities plus a parity blocker. |
| Global catalog/activity | `src/global_db.rs::GlobalDb`, `open_at_unsynchronized`, project/store/scope tables, sessions/messages/turns, analytics, parse offsets, workflows, Git correlation | Split into `catalog.db` and `activity.db`; preserve V1 table/hash/count/source offsets in receipts. Canonical transcripts stay in activity. |
| Graph store | `src/db/connection.rs::Database`; `src/db/migrations.rs::{LATEST_VERSION,create_schema,migrate}`; `src/db/{nodes,edges,files,coverage,fingerprints,search}.rs` | Read V1 graph DBs read-only, import snapshot occurrences/edges, and write packed immutable V2 graph generations. |
| LCM duplicate/native store | `src/sessions/lcm/schema.rs::{LCM_SCHEMA_VERSION,ensure_lcm_schema}`, `raw.rs`, `dag.rs`, `query.rs` | Import every sanitized native message once into activity observations/entities; import summary DAG as derived lineage with exact source coverage. |
| Session query dedupe/origin filters | merged PR #410: `src/sessions/message_noise.rs`, global message/LCM query paths, CLI/MCP filter schemas | Store every native row once, then persist versioned origin/representative projections and membership evidence. A human/representative view cannot mutate canonical content or hide its excluded-copy count. |
| V1 payload/GC/doctor | `src/sessions/lcm/payload.rs::LcmStore`, `payload_dir`, `validate_payload_ref`; `gc.rs`; `doctor.rs::{checkpoint_wal_for_backup,backup_database}` | Hash-verify and import payloads into privacy-domain blobs. Preserve old refs in migration evidence; use SQLite backup API and signed manifests instead of copying a live WAL family. |
| V1 retention | `src/retention.rs::{RetentionConfig,RetentionTable,prune_table}` | Preserve strict older-than cutoff while anchoring V2 retention on required `ingested_at`; keep skeleton/audit rows and enforce holds. |
| Hermes consolidation | PR #407: `src/migrate/hermes.rs::{LegacyHermesMigration,LegacyHermesMigrationReport,MigrationMarker,migrate_legacy_hermes_stores}`, `logical_source_fingerprint`, session/LCM/fact copy functions | Consume migration ledgers/fingerprints and copied facts as parity evidence. `~/.hermes` is source-only; sessions/LCM and profile/unresolved histories target activity, while explicitly project-scoped histories target the canonical project shard. Facts-only stores are mandatory inputs. |
| Runtime lifecycle drain | merged PR #412: `src/lifecycle_lease.rs`, daemon/service/update shutdown, writer drain and WAL checkpoint order | Import/emit fenced lifecycle leases and shutdown receipts. Store maintenance/update cannot checkpoint, migrate, replace, or reopen until owned background writers and clients are drained; preserve stopped/disabled/masked service state. |
| Foreign skill ownership | merged PR #411: shared doctor/removal ownership predicate and `ForeignOrphan` | Persist installation owner/source manifest and remediation classification for skill materialization evidence. Foreign or legacy-owner packages are never deletion/update candidates for this installation without explicit ownership transfer. |

Planning began at `99ad19bc`; publication master `66584b4d` includes #410/#411/#413/#414/#415/#416/#417/#419. Open #407/#418/#420 remain refresh inputs; #417 identity-split visibility is a required catalog/store conflict fixture, #419 requires snapshot/same-file-safe edit receipts, and #420 requires daemon authority selection before local store open. PR #409 remains historical. Every implementation/import PR refreshes current master/open state and exact store/schema/protocol inventories before generating manifests.

## Proposed crate tree

```text
crates/tracedecay-store/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── config.rs
│   ├── layout.rs
│   ├── permissions.rs
│   ├── manifest.rs
│   ├── sqlite/
│   │   ├── mod.rs
│   │   ├── connection.rs
│   │   ├── pragmas.rs
│   │   ├── read_pool.rs
│   │   ├── writer.rs
│   │   ├── lease.rs
│   │   └── transaction.rs
│   ├── migrations/
│   │   ├── mod.rs
│   │   ├── runner.rs
│   │   ├── receipt.rs
│   │   └── sql/
│   │       ├── catalog/0001_core.sql
│   │       ├── activity/0001_core.sql
│   │       ├── project/0001_core.sql
│   │       └── graph/0001_generation.sql
│   ├── catalog/
│   │   ├── mod.rs
│   │   ├── repository.rs
│   │   ├── identity.rs
│   │   ├── scope.rs
│   │   ├── shards.rs
│   │   ├── locators.rs
│   │   └── privacy.rs
│   ├── identity/
│   │   ├── mod.rs
│   │   ├── resolver.rs
│   │   ├── aliases.rs
│   │   ├── candidates.rs
│   │   └── conflicts.rs
│   ├── journal/
│   │   ├── mod.rs
│   │   ├── ingress.rs
│   │   ├── spool.rs
│   │   ├── append.rs
│   │   ├── source_head.rs
│   │   └── quarantine.rs
│   ├── activity/
│   │   ├── mod.rs
│   │   ├── repository.rs
│   │   ├── entities.rs
│   │   ├── events.rs
│   │   ├── relations.rs
│   │   ├── sessions.rs
│   │   └── coordination.rs
│   ├── project/
│   │   ├── mod.rs
│   │   ├── repository.rs
│   │   ├── entities.rs
│   │   ├── events.rs
│   │   ├── relations.rs
│   │   ├── activity_locators.rs
│   │   └── histories.rs
│   ├── graph/
│   │   ├── mod.rs
│   │   ├── manifest.rs
│   │   ├── generation.rs
│   │   ├── overlay.rs
│   │   ├── compaction.rs
│   │   └── recovery.rs
│   ├── blob/
│   │   ├── mod.rs
│   │   ├── id.rs
│   │   ├── crypto.rs
│   │   ├── staging.rs
│   │   ├── repository.rs
│   │   ├── integrity.rs
│   │   └── gc.rs
│   ├── outbox/
│   │   ├── mod.rs
│   │   ├── repository.rs
│   │   ├── lease.rs
│   │   └── checkpoint.rs
│   ├── projection/
│   │   ├── mod.rs
│   │   ├── repository.rs
│   │   └── rows.rs
│   ├── retention/
│   │   ├── mod.rs
│   │   ├── preview.rs
│   │   ├── apply.rs
│   │   └── holds.rs
│   ├── integrity/
│   │   ├── mod.rs
│   │   ├── sqlite.rs
│   │   ├── catalog.rs
│   │   └── report.rs
│   ├── backup/
│   │   ├── mod.rs
│   │   ├── snapshot.rs
│   │   ├── restore.rs
│   │   └── verify.rs
│   ├── recovery/
│   │   ├── mod.rs
│   │   ├── startup.rs
│   │   └── killpoint.rs
│   └── import/
│       ├── mod.rs
│       ├── inventory.rs
│       ├── v1_catalog.rs
│       ├── v1_activity.rs
│       ├── v1_graph.rs
│       ├── v1_payload.rs
│       ├── legacy_identity.rs
│       └── hermes.rs
├── tests/
│   ├── sqlite_runtime.rs
│   ├── migration_contract.rs
│   ├── catalog_contract.rs
│   ├── identity_resolution.rs
│   ├── scope_resolution.rs
│   ├── evidence_relation.rs
│   ├── journal_concurrency.rs
│   ├── outbox_contract.rs
│   ├── graph_generation.rs
│   ├── blob_contract.rs
│   ├── retention_contract.rs
│   ├── recovery_contract.rs
│   ├── import_parity.rs
│   └── fixtures/
│       ├── v1-store-manifest.json
│       ├── pr405-identity-inventory.json
│       ├── pr407-hermes-ledger.json
│       └── expected-import-receipt.json
└── benches/
    ├── concurrent_ingest.rs
    ├── read_write_contention.rs
    └── graph_generation_policy.rs
```

## Dependency direction and ownership

`tracedecay-store` depends only on `tracedecay-domain` plus infrastructure libraries. It cannot import the root crate, provider adapters, projectors, query planner, policy, application, CLI, MCP, HTTP, or dashboard modules.

Write ownership:

| Owner | Writes | Readers |
|---|---|---|
| Catalog writer | profile/global allocations, shard registry/capabilities/health, safe locators/rollups, migration receipts, projection/vector watermarks | scope resolver, planner, Observatory, backup |
| Activity writer | transcript observations, canonical activity entities/events/relations, source heads/gaps, session/tool/agent/workflow/goal histories, activity outbox | projectors, query, policy replay, import parity |
| Project writer | project observations/evidence, project entities/relations, activity locators, Git/code locators, knowledge/policy/automation histories, project outbox | projectors, query, application |
| Graph generation builder | private staging generation/overlay | project manifest publisher, query |
| Blob service | staged/final files and blob metadata/ref commands | authorized payload hydration, integrity, GC |

Canonical graph-of-graphs history must include threads/sessions, actors/agent instances, turns, messages/content parts, provider-exposed reasoning, tools/results/approvals, goals/tasks/handoffs, Claude workflow runs/agents, Codex goals/plan updates, Git/worktree/ref/commit/PR/check/review/release, code snapshots/files/symbols/tests/diagnostics, holographic memory facts/entities/trust/feedback, managed skills and skill versions, curation proposals/versions/approvals/applies/outcomes, and automation runs/artifacts. Mutable domain state is represented as immutable versions/events/relations plus an explicit current projection. Memory, skills, and curation never live in branch graph generations.

## Physical layout

```text
<profile-root>/v2/
├── catalog.db
├── catalog.manifest.json
├── activity/
│   ├── activity.db
│   ├── activity.manifest.json
│   ├── writer.lock
│   └── inbox/<source-id>/<spool-sequence>.bin
├── projects/<project-shard-id>/
│   ├── project.db
│   ├── project.manifest.json
│   ├── writer.lock
│   └── graphs/
│       ├── graph-manifest.json
│       ├── generations/<generation-id>.db
│       ├── overlays/<overlay-id>.db
│       └── staging/
├── privacy/<privacy-domain-id>/
│   ├── blobs/<key-epoch>/<retention-class>/<prefix>/<blob-id>
│   ├── staging/
│   └── orphan-quarantine/
├── backups/<backup-id>/
│   ├── backup-manifest.json
│   ├── catalog.db
│   ├── activity.db
│   ├── projects/
│   ├── graph-manifests/
│   └── blob-inventory.json
└── quarantine/
    ├── shards/
    ├── observations/
    └── imports/
```

All managed directories are `0700` and regular files `0600` where supported. Creation rejects symlinked parents, path traversal, non-normal components, and roots outside the configured profile. Temporary files use exclusive create in the destination filesystem, fsync file and directory, then atomic rename.

## Public store interfaces

Every value type in these ports is imported unchanged from `01-domain-crate.md`; the repository traits and signatures below are store-owned contracts and must not be duplicated by capture, projectors, query, or application.

```rust
pub trait StoreFactory: Send + Sync {
    fn open_catalog(&self, mode: OpenMode) -> Result<Box<dyn CatalogRepository>, StoreError>;
    fn open_activity(&self, mode: OpenMode) -> Result<Box<dyn ActivityRepository>, StoreError>;
    fn open_project(
        &self,
        shard_id: ShardId,
        mode: OpenMode,
    ) -> Result<Box<dyn ProjectRepository>, StoreError>;
    fn open_graph_generation(
        &self,
        generation_id: GraphGenerationId,
    ) -> Result<Box<dyn GraphGenerationRepository>, StoreError>;
    fn blob_store(
        &self,
        domain: BlobDomainId,
    ) -> Result<Box<dyn BlobRepository>, StoreError>;
}

pub trait IdentityAllocationRepository: Send + Sync {
    fn resolve_or_allocate(
        &self,
        request: &AllocationRequest,
    ) -> Result<EntityRef, StoreError>;
    fn resolve(&self, allocation_key: NaturalKeyDigest) -> Result<Option<EntityRef>, StoreError>;
}

pub trait ObservationJournal: Send + Sync {
    fn append_transaction(&self, batch: AppendBatch) -> Result<AppendReceipt, StoreError>;
    fn source_head(&self, source_id: SourceInstanceId) -> Result<Option<SourceHead>, StoreError>;
    fn list_gaps(&self, source_id: SourceInstanceId) -> Result<Vec<SourceGap>, StoreError>;
}

pub trait Ingress: Send + Sync {
    fn submit(&self, batch: AppendBatch, deadline: std::time::Instant)
        -> Result<IngressAck, StoreError>;
}

pub trait EvidenceRepository: Send + Sync {
    fn put_entity_version(&self, value: EntityVersionV1) -> Result<ShardWatermark, StoreError>;
    fn put_event(&self, value: CanonicalEventV1) -> Result<ShardWatermark, StoreError>;
    fn put_relation(&self, value: RelationAssertionV1) -> Result<ShardWatermark, StoreError>;
    fn tombstone_relation(
        &self,
        relation_id: RelationId,
        superseded_by: Option<RelationId>,
    ) -> Result<ShardWatermark, StoreError>;
}

pub struct RegisteredReadModelId(pub u32);
pub struct RegisteredFieldProjection(pub Vec<u32>);
pub struct RegisteredStorePredicate {
    pub field_id: u32,
    pub operator: RegisteredPredicateOperator,
    pub value: ProtectedQueryValue,
}
pub struct RegisteredSortKey {
    pub field_id: u32,
    pub direction: SortDirection,
    pub nulls: NullPlacement,
}
pub struct StoreResumePosition(pub Vec<u8>);
pub struct RegisteredProjectionRow {
    pub read_model: RegisteredReadModelId,
    pub row_key: NaturalKeyDigest,
    pub payload: PayloadRef,
    pub source_sequence: u64,
}
pub struct StoreReadPage {
    pub rows: Vec<RegisteredProjectionRow>,
    pub next: Option<StoreResumePosition>,
    pub scanned: u64,
    pub watermark: ShardWatermark,
}
pub struct ProjectionCheckpointRef {
    pub projector: NaturalKeyDigest,
    pub projector_version: ComponentVersion,
    pub shard_id: ShardId,
    pub contiguous_sequence: u64,
    pub generation: NaturalKeyDigest,
}
pub enum RegisteredProjectionEffect {
    Entity(EntityVersionV1),
    Event(CanonicalEventV1),
    Relation(RelationAssertionV1),
    PutRow(RegisteredProjectionRow),
    DeleteRow { read_model: RegisteredReadModelId, row_key: NaturalKeyDigest },
}
pub struct RegisteredOutboxMutation {
    pub mutation_kind: u32,
    pub entity: Option<EntityRef>,
    pub payload: Option<PayloadRef>,
}
pub struct ProjectionCommitReceipt {
    pub checkpoint: ProjectionCheckpointRef,
    pub effects: u64,
    pub duplicates: u64,
    pub first_outbox_sequence: Option<u64>,
    pub last_outbox_sequence: Option<u64>,
    pub watermark: ShardWatermark,
}

pub trait ReadSnapshot: Send {
    fn watermark(&self) -> ShardWatermark;
    fn read(&self, request: StoreReadRequest) -> Result<StoreReadPage, StoreError>;
    fn entity_versions(
        &self,
        entities: &[EntityRef],
        projection: &RegisteredFieldProjection,
    ) -> Result<Vec<EntityVersionV1>, StoreError>;
}

pub struct StoreReadRequest {
    pub read_model: RegisteredReadModelId,
    pub predicates: Vec<RegisteredStorePredicate>,
    pub projection: RegisteredFieldProjection,
    pub sort: Vec<RegisteredSortKey>,
    pub after: Option<StoreResumePosition>,
    pub limit: u16,
    pub at_or_below: ShardWatermark,
}

pub trait ProjectionRepository: Send + Sync {
    fn apply_projection(
        &self,
        commit: ProjectionCommit,
    ) -> Result<ProjectionCommitReceipt, StoreError>;
}

pub struct ProjectionCommit {
    pub consumer: ConsumerLease,
    pub expected_checkpoint: ProjectionCheckpointRef,
    pub consumed_sequences: std::ops::RangeInclusive<u64>,
    pub effects: Vec<RegisteredProjectionEffect>,
    pub emitted_outbox: Vec<RegisteredOutboxMutation>,
    pub next_checkpoint: ProjectionCheckpointRef,
}

pub trait OutboxRepository: Send + Sync {
    fn high_watermark(&self) -> Result<ShardWatermark, StoreError>;
    fn claim(
        &self,
        consumer: &ConsumerLease,
        after: u64,
        limit: usize,
    ) -> Result<OutboxBatch, StoreError>;
    fn checkpoint(
        &self,
        consumer: &ConsumerLease,
        sequence: u64,
        projector_version: &str,
    ) -> Result<(), StoreError>;
}

pub trait CatalogRepository:
    IdentityAllocationRepository + OutboxRepository + Send + Sync
{
    fn register_shard(&self, manifest: &ShardManifest) -> Result<ShardWatermark, StoreError>;
    fn shard_inventory(&self) -> Result<Vec<ShardInventory>, StoreError>;
    fn scope_candidates(&self, selector: &ScopeSelectorV2) -> Result<ScopeCandidateInventoryV2, StoreError>;
    fn record_locator(&self, locator: CatalogEntityLocator) -> Result<ShardWatermark, StoreError>;
    fn route_retrieval_anchor(&self, route: RetrievalAnchorRoute) -> Result<ShardWatermark, StoreError>;
    fn retrieval_anchor_owner(&self, id: &RetrievalAnchorId) -> Result<AnchorOwnerRoute, StoreError>;
    fn record_projection_watermark(
        &self,
        projector: &str,
        watermark: &VectorWatermark,
    ) -> Result<(), StoreError>;
}

pub trait RetrievalAnchorRepository: Send + Sync {
    fn put_anchor(&self, record: &RetrievalAnchorRecordV1) -> Result<(), StoreError>;
    fn resolve_anchor(&self, id: &RetrievalAnchorId, access: &AccessContext) -> Result<RetrievalAnchorResolutionV1, StoreError>;
    fn tombstone_anchor(&self, id: &RetrievalAnchorId, reason: RetentionTombstoneReason) -> Result<(), StoreError>;
}

pub struct ScopeCandidateInventoryV2 {
    pub selector_digest: ScopeSelectorDigest,
    pub catalog_generation: ManifestDigest,
    pub candidates: Vec<ScopeResolutionCandidateV2>,
    pub unmatched: Vec<ScopeRootV2>,
    pub watermark: ShardWatermark,
}

pub trait ActivityRepository:
    IdentityAllocationRepository + ObservationJournal + EvidenceRepository + OutboxRepository + ProjectionRepository + RetrievalAnchorRepository + Send + Sync
{
    fn read_snapshot(&self, at: &ShardWatermark) -> Result<Box<dyn ReadSnapshot>, StoreError>;
}

pub trait ProjectRepository:
    IdentityAllocationRepository + ObservationJournal + EvidenceRepository + OutboxRepository + ProjectionRepository + RetrievalAnchorRepository + Send + Sync
{
    fn read_snapshot(&self, at: &ShardWatermark) -> Result<Box<dyn ReadSnapshot>, StoreError>;
}

pub trait BlobRepository: Send + Sync {
    fn stage(&self, request: BlobWriteRequest, payload: &SanitizedPayload) -> Result<StagedBlob, StoreError>;
    fn publish(&self, staged: StagedBlob, owner: BlobOwnerRef) -> Result<PayloadRef, StoreError>;
    fn open_verified(&self, payload: &PayloadRef) -> Result<VerifiedSanitizedReader, StoreError>;
    fn release(&self, owner: &BlobOwnerRef, payload: &PayloadRef) -> Result<(), StoreError>;
}

pub trait ProtectedQuarantineRepository: Send + Sync {
    fn preserve(
        &self,
        request: ProtectedQuarantineWrite,
        content: ProtectedQuarantineIngress,
    ) -> Result<ProtectedSecretRef, StoreError>;
    fn metadata(&self, value: &ProtectedSecretRef) -> Result<ProtectedQuarantineMetadata, StoreError>;
    fn destroy(
        &self,
        value: &ProtectedSecretRef,
        receipt: SanitizationReceiptId,
    ) -> Result<SecureRetireReceipt, StoreError>;
}
```

Repository methods are synchronous. `ShardWriter` and `ReadPool` are implementation details; callers do not receive `Connection`, `Transaction`, SQL, or path access. `StoreReadRequest` accepts only registry-known read models, fields, predicates, and sort keys and caps `limit` at 1,000; the application-owned query/store adapter lowers query fragments into this storage-neutral contract. `ProjectionRepository::apply_projection` commits idempotency effects, typed rows, emitted outbox rows, and the contiguous checkpoint in one SQLite transaction; the application-owned projector/store adapter lowers projector effects into it. `CatalogRepository::scope_candidates` performs indexed catalog lookup only and returns evidence-bearing candidates at one catalog generation; the application scope resolver owns normalization, authorization, scoring, ambiguity, current-default disclosure, relationship expansion, and the final `ScopeResolutionV2`. `ProtectedQuarantineRepository` is the sole exception to ordinary sanitized payload ingress: it accepts only the sanitizer-created, move-only `ProtectedQuarantineIngress` after an explicit detector/policy decision, encrypts before any durable write, cannot return plaintext through general store/query ports, and follows Plan 18's key/TTL/audit contract.

## Concurrency, ordering, backpressure, and acknowledgement

### Writer topology

- One `writer.lock` advisory lock permits one writer owner per live shard across processes. Lock release on process death makes takeover possible.
- The writer owner records a monotonic `lease_epoch` in `writer_leases`; every commit/ack includes the epoch. A stale writer cannot publish after takeover because the transaction compare-checks the epoch.
- Inside the owner process, one thread owns the SQLite writer connection. It drains a bounded `crossbeam_channel` of 4,096 batches and at most 64 MiB of accounted payload metadata.
- It groups up to 256 observations, 2 MiB of indexed metadata, or 5 milliseconds, whichever arrives first, while retaining FIFO order per source. It may interleave sources; it never claims cross-source causation.
- External hook processes append private framed spool records under `activity/inbox/<source-id>/` if they cannot hand off to the lease owner within the caller deadline. A spool frame contains the complete validated observation batch, CRC32 frame check, SHA-256 digest, writer schema version, and monotonically allocated source-local spool sequence.
- `IngressAck::Committed` is returned only after SQLite commit. `IngressAck::DurablyQueued` is returned only after the spool file and parent directory are fsynced. Queue overflow, disk-full, permission, or fsync failure returns an error; no event is dropped.
- Capture advances the canonical V2 source cursor only on `Committed`. A durably queued batch can be reread from the source; deterministic IDs make replay a no-op after spool drain.

### Atomic append

One `BEGIN IMMEDIATE` transaction performs:

1. Validate registry/schema digest and writer lease epoch.
2. Insert provenance and observation rows with `INSERT ... ON CONFLICT DO NOTHING`.
3. Compare an existing ID's record and payload digests; mismatch is quarantined as `identity_collision` and aborts canonical publication.
4. Classify source continuity against `source_heads`; insert/update `source_gaps` without deleting late evidence.
5. Insert canonical entities/events/relations included in the batch.
6. Insert one outbox row per logical mutation with a shard-local sequence.
7. Advance `source_heads.contiguous_offset` only across committed contiguous ranges; never jump over a gap.
8. Commit, then send `AppendReceipt` containing lease epoch, per-observation disposition, first/last outbox sequence, committed timestamp, and `ShardWatermark`.

Duplicates with identical digests return `Duplicate` and do not emit a second outbox record. A record below the contiguous head returns `Late`; it remains queryable by occurred and ingested time. A record above the expected offset returns `Gap` and does not move the contiguous head. Arrival of missing ranges closes/shortens gaps and advances the head deterministically. A higher rewrite generation starts a new source sequence; a conflicting digest at the same position without a higher generation is quarantined.

### Reads and vector watermarks

- Each shard has a read-only pool capped at eight connections. A query opens at most 32 shards across the profile.
- A read page begins a deferred transaction, reads the shard outbox high watermark, applies `sequence <= captured watermark` predicates to mutable projections, reads rows, and closes the transaction before returning.
- The coordinator combines component watermarks into `VectorWatermark`; no read transaction remains open across pages.
- Frozen cursors resume at captured component watermarks. Live queries use delta positions, duplicate suppression by entity/event ID, and explicit gap/resync records.
- A missing/corrupt/incompatible shard contributes named partial coverage while other shard positions remain resumable.

### Outbox consumers and commands

- Outbox consumption is at-least-once and idempotent by `(shard_id, sequence, projector_version)`.
- Consumer leases carry `consumer_id`, `lease_epoch`, `leased_until`, and batch ceiling. Checkpoint compares lease epoch and cannot skip an unprocessed sequence.
- Dead letters retain original sequence, error class, attempt count, next retry, and payload digest. They block “caught up” status until resolved or explicitly quarantined.
- Outbox lag reports sequence distance and oldest unconsumed commit age. Cutover requires p95 projection visibility at most two seconds for 24 hours and zero unexplained dead letters.
- Writable commands persist `CommandEnvelopeV1`, compare `expected_version`, write mutation/audit/outbox in one transaction, and cache result by idempotency key. Version conflicts return current version with no mutation.

## Schema and migration ownership

Each database has `store_meta(store_kind, shard_id, profile_id, privacy_domain_id, schema_version, registry_version, registry_digest, created_at, migrated_at)` and `schema_migrations(version, name, sql_digest, started_at, committed_at, binary_version)`. SQL files are immutable after release. A changed digest is corruption, not a rerunnable migration.

`tracedecay-domain::SchemaRegistryV1` and `PredicateRegistryV1` own semantic legality. `tracedecay-store` owns physical tables/indexes/triggers and persists the registry version/digest in every shard. Open behavior:

- Equal schema and registry: open normally.
- Older compatible schema: writable open requires maintenance lease, disk preflight, backup, forward migration, integrity check, and receipt.
- Newer schema/registry: refuse writes and expose incompatible read-only coverage.
- Registry digest mismatch at the same version: quarantine the shard and refuse semantic reads.

### Catalog schema

- `identity_allocations(allocation_key BLOB PRIMARY KEY, entity_id BLOB UNIQUE, entity_kind TEXT, owning_shard_id BLOB, created_at INTEGER, source_manifest_id BLOB)`.
- `shards(shard_id BLOB PRIMARY KEY, kind TEXT, privacy_domain_id BLOB, manifest_digest BLOB, schema_version INTEGER, registry_version INTEGER, status TEXT, min_occurred_at INTEGER, max_occurred_at INTEGER, outbox_high_watermark INTEGER, last_verified_at INTEGER)`.
- `shard_capabilities(shard_id, capability, version, PRIMARY KEY(shard_id, capability))`.
- `catalog_locators(entity_id BLOB, entity_kind TEXT, owning_shard_id BLOB, opaque_locator BLOB, PRIMARY KEY(entity_id, owning_shard_id))`.
- `catalog_alias_routes(alias_route_id BLOB PRIMARY KEY, entity_id BLOB, owning_shard_id BLOB, privacy_domain_id BLOB, namespace TEXT, exact_keyed_digest BLOB, key_epoch INTEGER, routing_generation BLOB, alias_version INTEGER, valid_from INTEGER, valid_to INTEGER, status TEXT, provenance_digest BLOB, UNIQUE(entity_id, namespace, exact_keyed_digest, key_epoch, alias_version))`.
- `catalog_alias_route_terms(alias_route_id BLOB, routing_generation BLOB, key_epoch INTEGER, term_kind TEXT, keyed_term_digest BLOB, ordinal INTEGER, PRIMARY KEY(alias_route_id, routing_generation, key_epoch, term_kind, keyed_term_digest, ordinal), FOREIGN KEY(alias_route_id) REFERENCES catalog_alias_routes(alias_route_id))`; `term_kind` is exact-token, quoted-phrase, or bounded n-gram. These tables contain keyed privacy-domain routing digests only—never literal alias/path/remote/display text.
- `retrieval_anchor_routes(anchor_id BLOB PRIMARY KEY, owning_shard_id BLOB, privacy_domain_id BLOB, route_version INTEGER, retention_state TEXT, tombstone_state TEXT, route_digest BLOB)`; it is content-free and routes only to an owner-shard record.
- `catalog_rollups(shard_id, bucket_start, kind, metric, value_integer, source_watermark, PRIMARY KEY(shard_id, bucket_start, kind, metric))`; no text value column.
- `projection_watermarks(projector, shard_id, sequence, projector_version, updated_at, status, PRIMARY KEY(projector, shard_id))`.
- `migration_receipts(receipt_id BLOB PRIMARY KEY, source_manifest_id BLOB, source_digest BLOB, destination_shard_id BLOB, counts_digest BLOB, status TEXT, created_at INTEGER)`.
- `registry_reconciliation(reconciliation_id BLOB PRIMARY KEY, project_id BLOB, repository_id BLOB, store_instance_id BLOB, checkout_id BLOB, worktree_id BLOB, ref_id BLOB, snapshot_id BLOB, graph_generation_id BLOB, registry_watermark INTEGER, index_watermark INTEGER, status TEXT, evidence_digest BLOB)`; conflicts/stale/orphans remain explicit and content-free. The full repository/checkout/worktree/ref/snapshot/generation tuple is indexed; a base checkout and a PR worktree can resolve to different generations without either becoming the default.
- `saved_view_manifests(view_id BLOB PRIMARY KEY, owner_shard_id BLOB, opaque_locator BLOB, version INTEGER, updated_at INTEGER)`; saved-view content and its blob reference remain in the owning activity/project shard.

Catalog migrations contain a privacy lint that rejects columns matching `text`, `content`, `query`, `annotation`, `alias_value`, `path`, `payload`, or `json`, except fixed safe enum/version/status columns audited in `catalog/privacy.rs`.

### Activity/project core schema

Both mutable evidence shards own:

- `identity_allocations` and `identity_aliases(namespace, value_keyed_digest, entity_id, valid_from, valid_to, resolver_version, status, confidence, provenance_id)`; literal alias values remain eligible encrypted owner-shard payloads.
- `provenance(provenance_id PRIMARY KEY, source_id, source_locator_keyed_digest, source_record_fingerprint, parser_version, resolver_version, ingested_at)`; both digest fields are privacy-domain-keyed types from plan 01.
- `observations(observation_id PRIMARY KEY, source_id, artifact_keyed_digest, rewrite_generation, offset, next_offset, source_record_fingerprint, sanitized_output_digest, occurred_at, missing_time_reason, ingested_at, schema_version, parser_version, payload_blob_id, sensitivity, sanitization_receipt_id, retention_class)`.
- `entities(entity_id PRIMARY KEY, kind, owning_shard_id, natural_key_hash, created_at, retired_at)`.
- `entity_versions(entity_id, version_id, schema_version, valid_from, valid_to, observed_at, sanitized_output_digest, attrs_blob_id, PRIMARY KEY(entity_id, version_id))`.
- `events(event_id PRIMARY KEY, kind, schema_version, owning_shard_id, session_id, actor_id, run_id, snapshot_id, occurred_at, ingested_at, correlation_id, causation_id, provenance_id, payload_blob_id, attrs_blob_id, sensitivity, retention_class, supersedes_event_id)`.
- `relation_assertions(relation_id PRIMARY KEY, subject_id, predicate, object_id, scope_id, valid_from, valid_to, observed_from, observed_to, evidence_class, confidence, confidence_reason_code, confidence_rationale_blob_id, producer_version, provenance_id, sensitivity, supersedes_relation_id, tombstone)` plus evidence link tables. `confidence_rationale_blob_id` is nullable and may reference only an eligible `LogSafeText` payload in the relation's privacy domain.
- `source_heads(source_id, rewrite_generation, ordering, contiguous_offset, last_source_record_fingerprint, lease_epoch, updated_at, PRIMARY KEY(source_id, rewrite_generation))`.
- `source_gaps(source_id, rewrite_generation, gap_start, gap_end, first_seen_at, last_seen_at, status, PRIMARY KEY(source_id, rewrite_generation, gap_start))`.
- `quarantined_writes(quarantine_id PRIMARY KEY, source_id, reason, source_record_fingerprint, protected_secret_ref, sanitization_receipt_id, first_seen_at, attempt_count)`; this table contains no candidate bytes and the protected reference is nullable and opaque.
- `outbox(sequence INTEGER PRIMARY KEY AUTOINCREMENT, tx_id BLOB, mutation_kind TEXT, entity_id BLOB, sanitized_output_or_manifest_digest BLOB, projector_targets TEXT, committed_at INTEGER, lease_epoch INTEGER)`; the digest is never an unkeyed raw-source checksum.
- `consumer_leases`, `projection_checkpoints`, `dead_letters`, `command_results`, `blob_refs`, and `retention_holds`.
- `retrieval_anchor_records(anchor_id PRIMARY KEY, target_kind, target_ref, resolved_scope_id, privacy_domain_id, access_policy_digest, source_identity_class, immutable_source_refs_blob_id, source_observations_blob_id, vector_watermark_blob_id, schema_registry_digest, capability_catalog_digest, data_version_digest, projection_version, view_algorithm_version, retrieval_view, expansion_recipe_blob_id, canonical_request_digest, provenance_blob_id, payload_access, retention_class, created_at, durability, tombstoned_at)` in the owning activity/project shard. Every blob field uses the record's privacy domain and a sink-eligible typed schema.

Anchor creation commits owner-shard `retrieval_anchor_records`, catalog `retrieval_anchor_routes`, and outbox intent through a resumable application workflow; an unrouteable half-created anchor is not returned. Resolution authorizes the content-free route, pins the owner snapshot, then loads exactly one record. Retention replaces the owner record with a typed tombstone state and updates the route; it never reuses the ID or redirects to a similar entity. Backup/restore, key rotation, adoption/move, and shard reconciliation verify anchor route/record counts and digests.

Activity/profile canonical tables: `actors`, `agent_instances`, `agent_presences`, `work_claims`, `work_claim_scope_entities`, `work_claim_retrieval_anchors`, `coordination_events`, `threads`, `thread_sessions`, `sessions`, `workflow_runs`, `turns`, `messages`, `content_parts`, `message_origin_assertions`, `message_representative_memberships`, `reasoning_artifacts`, `tool_invocations`, `tool_results`, `approvals`, `goals`, `tasks`, `handoffs`, `installations`, profile-scoped `skill_materializations`, `doctor_findings`, `remediation_events`, fact/skill/policy/automation histories, `lifecycle_leases`, `drain_receipts`, `checkpoint_receipts`, `service_state_events`, encrypted saved-view/annotation content, and `activity_search_documents`. `threads` preserves provider-native conversation/thread identity independently of execution/session identity; `thread_sessions` is temporal, evidence-bearing, and many-to-many. Presence/work claims are canonical activity because an agent can span zero/many projects; project shards receive safe claim locators. TTL status uses immutable heartbeat/expiry events and never deletes history. Project attribution is relation evidence. Message-origin and representative rows never delete or overwrite a native message row.

Coordination indexes cover `(status, expires_at)`, agent/session/parent/goal, each canonical scope entity kind, intent, redundancy mode, and retrieval-anchor digest. Safe summaries are activity payload fields, never catalog/metric/project-locator text. Expiry is an indexed current-view predicate plus explicit event, not a cleanup race.

Project-only canonical/history tables: `repositories`, `projects`, `checkouts`, `worktrees`, `refs`, `commits`, `pull_requests`, `checks`, `reviews`, `releases`, `activity_locators`, plus project-scoped `facts`, `fact_versions`, `knowledge_entities`, `decisions`, `contradictions`, `trust_events`, `retrieval_events`, `feedback_events`, `policy_bundles`, `policy_evaluations`, `hint_evaluations`, `automation_jobs`, `scheduler_decisions`, `automation_runs`, `run_events`, `automation_artifacts`, `skill_versions`, `skill_materializations`, `doctor_findings`, `remediation_events`, `curation_proposal_versions`, `approvals`, `apply_events`, `outcome_events`, `project_search_documents`, and rollups. The same typed histories may be owned by activity when their declared scope is profile/zero-project/cross-project or unresolved. Current views are projections over immutable histories.

## Identity allocation and repository ownership

- Exact source/native IDs derive in `tracedecay-domain`; the store verifies derivation before insert.
- Canonical alias history and literal values remain authorized evidence in their activity/project owner shard. A projector publishes versioned exact/token/ngram keyed digests plus owner/provenance/tombstone state to the content-free catalog routing index. Application computes comparable digests only after selecting an authorized privacy domain/key epoch, uses the catalog to prune candidate shards, then verifies current literal/evidence state in each selected owner shard before resolution. Key rotation rebuilds the routing generation atomically; stale generations return unavailable coverage and never fall back to unkeyed or all-shard scanning.
- `resolve_or_allocate` runs `INSERT ... ON CONFLICT(allocation_key) DO NOTHING`, reads the row in the same transaction, and verifies kind/owner/source manifest. New IDs are UUIDv7 from an injected generator and committed before publication.
- Profile/global allocation rows live in catalog; activity-native and profile-scoped knowledge/policy/automation ambiguous rows live in activity; repository/project/code and project-scoped knowledge/policy/automation ambiguous rows live in project.
- Backups and restores include every allocation table before any projector rebuild. Missing allocation ledgers stop restore; no replacement UUID is minted.
- Repositories, checkouts, worktrees, and monorepo subprojects are views/entities inside one canonical repository/privacy-domain project shard. Branches and worktrees cannot own facts, skills, or canonical activity.
- PR #405 adoption evidence is evaluated before allocation. One unique healthy legacy store can retain lineage; multiple healthy/nonempty candidates remain distinct and create an identity conflict relation plus import blocker.
- PR #407 does not create a Hermes profile. Hermes sessions become activity entities. Facts, skills, policy, and automation resolve by `DeclaredScope`: profile/zero-project/cross-project and unresolved-scope histories live in activity; explicitly project-scoped histories live in the canonical repository project shard. Scope resolution and later supersession remain evidence.

## Graph generation store

`GraphGenerationRepository` exposes:

```rust
pub trait GraphGenerationRepository: Send + Sync {
    fn begin_generation(&self, request: GenerationRequest) -> Result<GenerationWriter, StoreError>;
    fn publish_generation(&self, sealed: SealedGeneration) -> Result<GraphManifest, StoreError>;
    fn open_resolved_snapshot(
        &self,
        resolved: &ScopeResolutionCandidateV2,
    ) -> Result<GraphSnapshotReader, StoreError>;
    fn compact(&self, request: CompactionRequest) -> Result<GraphManifest, StoreError>;
}
```

Generation DBs contain deduplicated file content, file/symbol occurrences, snapshot-scoped code edges, diagnostics, tests/test maps, fingerprints, redundancy, complexity, and rebuildable FTS/vector metadata. Manifests map every `CodeSnapshotId` to its repository, checkout, zero-to-many worktree/ref locators, generation plus overlay chain, checksum, extractor/resolver versions, and source watermark. `open_resolved_snapshot` verifies every nonempty tuple field and freshness watermark against that manifest; mismatch returns typed stale/ambiguous coverage and never opens the active/base/current generation instead. Refs are movable locators, snapshots/generations are immutable, and multiple refs may name the same snapshot.

Publication sequence: build under `staging`, run `quick_check` and row/hash manifest verification, fsync DB and directory, rename to final generation path, then compare-and-swap the project graph manifest under project writer transaction. Startup removes incomplete staging files, quarantines checksum failures, and registers final-but-unreferenced files as orphans for grace-period cleanup.

The storage ADR benchmark runs candidates of 32/64/128 snapshots per pack and 512 MiB/1 GiB/2 GiB target pack sizes at current and 10× corpora. It selects the smallest candidate meeting query/compaction/file-count gates and records it before constants are committed. Overlay depth candidates are 2/4/8; the selected value must keep snapshot-open p95 within gate and profile graph files below 10,000 after compaction. At most eight generation files are open per query process.

## Privacy-domain blob store

Blob identity is exact:

1. Require `SanitizedPayload` plus its domain `SanitizationReceiptV1` before `BlobWriteRequest`; the store never classifies, scans, redacts, or mints the proof.
2. Compute an internal `ContentDigest = SHA-256(canonical uncompressed bytes)` for verification. It never enters catalog rows, public payload references, logs, or cross-domain APIs; protected mode stores it only inside the encrypted blob header.
3. Compute `BlobId = HMAC-SHA-256(K_domain_dedupe, key_epoch || retention_class || content_digest)` and a separately keyed opaque `BlobIntegrityTag` for the public/domain reference.
4. Compress eligible normal content with zstd before encryption; record compression and original/stored sizes.
5. In protected mode encrypt with XChaCha20-Poly1305 using a random nonce and per-domain encryption key supplied by `BlobKeyProvider`; keys never enter SQLite or manifests.
6. Exclusive-create a private staged file, write header+ciphertext, fsync, reread/verify header/hash/size, rename atomically to final path, fsync parent, then publish metadata/ref/outbox in the owning shard transaction.

Because privacy domain, key epoch, and retention class enter the keyed ID/domain path, equality and lifetime do not leak across those boundaries. Refcounts are advisory. GC mark-and-sweeps from signed shard/backup manifests, committed owner refs, protected holds, and committed outboxes. Unreferenced final files wait 24 hours in recovery grace; missing referenced files quarantine the ref and make coverage partial. Key rotation creates a new epoch and rewrap/re-encrypt job; destructive retirement requires a verified encrypted recovery export.

## Retention, deletion, and holds

- Eligibility uses domain `RetentionPolicyV1`: required `ingested_at < evaluated_at - horizon`; exact-cutoff content remains. Normal content is indefinite, reasoning 30 days, secret quarantine 24 hours, response cache 7 days, raw telemetry 180 days, automation intermediates 90 days.
- A retention worker holds one fenced lease per privacy domain and operates in bounded batches of 1,000 owners or 64 MiB.
- Preview resolves descendant entities/projections/blob refs, holds, expected bytes, and the exact vector watermark.
- Apply transaction writes canonical tombstone/deletion receipt, removes FTS/vector/search rows, releases blob refs, emits outbox, and advances retention watermark. Physical blob GC occurs after 24-hour recovery grace.
- Pinned/legal holds name owner, scope, reason, issuer, creation/expiry, and optimistic version. Expired holds remain audit events.
- Canonical tombstone and FTS/vector removal must complete within one minute of approved deletion. Non-content provenance skeleton and deletion receipt persist.
- Queries/cursors whose evidence crossed the captured retention watermark receive `RetentionCrossedSnapshot` and a restart/incompleteness reason.

## Backup, integrity, crash recovery, and repair

- A backup lease freezes migrations/retention/graph manifest swaps, records a vector watermark, and uses SQLite online backup API per mutable shard. Live ingest may continue above the captured component watermark.
- Backup manifest includes schema/registry versions, shard and identity-allocation digests, SQLite page counts/hashes, graph manifests, blob inventory, outbox high watermarks, source heads/gaps, privacy/key epochs, redaction report, and coverage.
- Restore verifies all hashes, restores allocation ledgers first, opens databases read-only, checks `quick_check`, registry digest, foreign keys, outbox/checkpoint bounds, graph manifests, and blob refs, then publishes a new catalog manifest.
- Catalog rebuild reads signed shard/graph/blob manifests and committed outboxes. It never reconstructs content or missing allocation IDs.
- Startup recovery drains committed spools idempotently, removes incomplete stages, completes or reverts migration receipts, verifies lease epochs, detects outbox/checkpoint gaps, and quarantines corrupt shards without blocking healthy shards.
- Repair actions are preview/apply commands with idempotency and optimistic version; they never silently drop evidence.
- Killpoints cover spool fsync, observation insert, outbox insert, source-head update, commit-before-ack, blob rename, ref publish, graph rename, manifest swap, migration step, backup, retention tombstone, ref release, and GC unlink.

## Import, parity, and cutover

`V1Importer` opens all V1 SQLite databases read-only and writes only V2 destinations. Each source gets a `ManifestId`, logical digest, schema version, table counts/hashes, source offset range, payload inventory, identity aliases, quarantine list, and import watermark. Re-running the same manifest produces no additional observations/entities/events/outbox records.

Import order:

1. Inventory V1 `src/storage.rs` manifests, global registry, project aliases, branch metadata, graph DBs, sessions/LCM DB, payload/response/artifact roots, automation files, and retention configuration.
2. Ingest PR #405 repository identity markers/candidate inventories/adoption receipts. Preserve every conflicting store and block repository cutover on ambiguity.
3. Ingest PR #407 Hermes migration ledgers/logical source fingerprints, including facts-only stores and moved/renamed/symlinked project resolution evidence. Do not scan or route new canonical data to a Hermes-owned profile.
4. Import PR #410 native rows and expected origin/representative query outcomes as parity evidence; do not rewrite or drop copied rows.
5. Import catalog/global identity evidence and persist allocation mappings.
6. Import canonical activity observations/sessions/messages/turns/reasoning/tools/goals/workflows and LCM source/DAG lineage.
7. Import project Git/delivery, knowledge/facts/trust/feedback, skills/curation/automation histories, and activity locators.
8. Import graph snapshot generations and payload blobs.
9. Compare counts, hashes, aliases, ordinals, timestamps, source offsets, summary coverage, native/representative/origin counts, fact versions, skill/proposal versions/outcomes, and quarantine. Emit a signed parity receipt.

V1 remains authoritative until the bounded context's freeze watermark. Cutover requires dual capture success, zero unexplained parity gaps, no identity conflict, no dead letter, p95 visibility at most two seconds for 24 hours, backup/restore proof, and rollback drill. V1 stores remain read-only for one release after verified cutover. Rollback restores V1 source-offset ownership from the receipt, stops V2 writer leases, retains V2 read-only evidence, and does not reverse-delete V2 files.

## Cross-crate consumes/produces contracts

| Direction | Contract |
|---|---|
| Consumes from `tracedecay-domain` | IDs/ownership, allocations, observations/events/relations, `AgentPresenceV1`, `WorkClaimV1`, registry digests, retention/protocol policy, source continuity, watermarks, commands |
| Produces to capture | `Ingress`, `IngressAck::{Committed,DurablyQueued}`, `AppendReceipt`, durable gap/quarantine status |
| Produces to projectors | `OutboxBatch`, consumer lease/checkpoint, dead-letter and lag status, immutable evidence reads |
| Produces to query | catalog shard inventory/capabilities/statistics, read snapshots at component watermarks, partial/incompatible/locked coverage, authorized blob readers |
| Produces to application/operations | command receipts, migration/import/parity receipts, retention preview/apply, integrity/backup/restore reports |

## Implementation and PR/TDD sequence

This sequence refines master-plan PR 5 into PRs 5A–5B, PR 6 into PRs 6A–6D, and the store-owned slices of PRs 33/35. It does not renumber or move the program's capture/projector/query PRs. Every red test must fail for the named missing behavior before production implementation. Commands run from the repository root with the checkout-local `target/` and no `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` override unless Cargo reports actual target-lock contention.

### PR 5A: Crate boundary, private layout, SQLite runtime, and migration runner

**Files:** modify workspace `Cargo.toml`; create `crates/tracedecay-store/Cargo.toml`; create `src/{lib,error,config,layout,permissions,manifest}.rs`, `src/sqlite/{mod,connection,pragmas,read_pool,writer,lease,transaction}.rs`, `src/migrations/{mod,runner,receipt}.rs`; create `tests/{sqlite_runtime,migration_contract}.rs`.

- [ ] Add failing tests `store_has_no_root_or_transport_dependency`, `opens_one_sqlite_runtime`, `applies_required_pragmas`, `read_only_open_is_query_only`, `rejects_symlinked_or_public_root`, `writer_lease_fences_stale_epoch`, `migration_digest_is_immutable`, `newer_schema_is_read_only_incompatible`, and `failed_migration_keeps_previous_schema`.
- [ ] Run `cargo test -p tracedecay-store --test sqlite_runtime --test migration_contract -- --nocapture`. Expected: package/types are absent and tests fail for that reason.
- [ ] Add the package, strict dependency lint, private layout/path validation, `StoreFactory`, synchronous connection factory, writer/read ownership, fenced lease, forward-only migration runner, disk preflight, backup hook, and signed migration receipt. `libsql` async/remote symbols are forbidden.
- [ ] Re-run the focused tests. Expected: all named tests pass; `journal_mode=WAL`, `synchronous=FULL`, foreign keys, trusted-schema, busy-timeout, and query-only assertions match Section 3 exactly.
- [ ] Run `cargo tree -p tracedecay-store --edges normal` and `rg -n 'axum|rmcp|clap|dashboard|src/sessions|src/hooks|libsql::Database|Builder::new_remote' crates/tracedecay-store/src`. Expected: no forbidden edge/match.
- [ ] Commit `feat(store): establish private federated sqlite runtime`.

### PR 5B: Catalog/activity/project schemas and durable identity allocation

**Files:** create `src/migrations/sql/{catalog,activity,project}/0001_core.sql`; create `src/catalog/{mod,repository,identity,shards,locators,privacy}.rs`, `src/activity/{mod,repository,entities,events,relations,sessions}.rs`, `src/project/{mod,repository,entities,events,relations,activity_locators,histories}.rs`; create `tests/catalog_contract.rs`; extend `tests/migration_contract.rs`.

- [ ] Add failing tests `catalog_has_no_content_columns`, `catalog_rejects_literal_payload`, `canonical_message_owner_is_activity`, `project_shard_has_locator_not_message_copy`, `presence_and_claim_owner_is_activity`, `project_claim_has_locator_not_summary_copy`, `claim_summary_rejects_over_160_or_secret`, `claim_ttl_expires_current_view_not_history`, `multi_repo_selector_opens_exact_shards`, `scope_resolution_preserves_repository_checkout_worktree_ref_snapshot_generation_tuple`, `stale_registry_store_is_quarantined_not_selected`, `base_checkout_does_not_replace_pr_worktree`, `profile_scoped_histories_are_activity_owned`, `project_scoped_histories_are_project_owned`, `unresolved_scope_remains_activity_evidence`, `allocation_is_stable_under_64_writers`, `allocation_owner_conflict_fails`, and `raw_410_rows_are_never_replaced_by_representatives`.
- [ ] Run `cargo test -p tracedecay-store --test catalog_contract --test migration_contract -- --nocapture`. Expected: schema/ports are missing.
- [ ] Implement the Section 9 schemas, exhaustive `DeclaredScope` routing, content-free catalog lint, opaque locators, UUIDv7 insert-or-read allocation, alias validity/provenance, immutable entity/event/relation histories, and origin/representative membership tables. Catalog stores no saved-view/query/message literal or direct content blob reference.
- [ ] Add migration fixtures for PR #405 adopted/ambiguous identities, PR #407 profile-versus-project scope, and PR #410 eight-child native rows. Every native row remains addressable; representative membership is a versioned derived relation with hidden-copy counts.
- [ ] Re-run tests. Expected: all pass; a schema introspection dump contains zero forbidden catalog columns and ownership fixtures enumerate every scope-sensitive kind.
- [ ] Commit `feat(store): add v2 shard schemas and identity ledger`.

### PR 6A: Observation journal, writer queue, spool ingress, outbox, and commands

**Files:** create `src/journal/{mod,ingress,spool,append,source_head,quarantine}.rs`, `src/outbox/{mod,repository,lease,checkpoint}.rs`, `src/projection/{mod,repository,rows}.rs`; extend `src/sqlite/{writer,lease,transaction}.rs`; create `tests/{journal_concurrency,outbox_contract}.rs`; begin `tests/recovery_contract.rs`; create `benches/{concurrent_ingest,read_write_contention}.rs`.

- [ ] Add failing tests `observation_outbox_cursor_commit_atomically`, `duplicate_digest_is_noop`, `conflicting_digest_is_quarantined`, `late_record_is_retained`, `gap_does_not_advance_contiguous_head`, `gap_fill_advances_exactly`, `rewrite_starts_new_generation`, `stale_writer_cannot_ack`, `durably_queued_requires_file_and_directory_fsync`, `projection_effects_outbox_checkpoint_commit_atomically`, `checkpoint_cannot_skip_sequence`, `read_snapshot_never_exceeds_captured_watermark`, `read_request_rejects_unregistered_fields`, `command_result_is_idempotent`, and `version_conflict_writes_nothing`.
- [ ] Add deterministic 32-producer/10,000-observation and 64-reader workloads; inject kills before/after spool fsync, observation insert, outbox insert, source-head update, commit, acknowledgement, lease takeover, and checkpoint.
- [ ] Run `cargo test -p tracedecay-store --test journal_concurrency --test outbox_contract --test recovery_contract -- --nocapture`. Expected: failures identify absent atomic append/fencing/recovery behavior.
- [ ] Implement one owned writer thread per shard, bounded frame/byte queue, fair source interleaving, fallback spool, lease-epoch compare, `BEGIN IMMEDIATE` append, per-mutation outbox sequence, continuity/gap state, at-least-once claims, checkpoint CAS, dead letters, and optimistic/idempotent command results. Use the domain `AppendReceipt`/`IngressAck`; do not create a store-local semantic variant.
- [ ] Re-run tests. Expected: zero acknowledged loss/divergent duplicate; every kill yields complete commit or safe retry; no checkpoint advances over a gap/dead letter.
- [ ] Run `cargo bench -p tracedecay-store --bench concurrent_ingest --bench read_write_contention`. Expected: report reference machine/corpus/queue/WAL/p50/p95/p99/RSS and meet Section 14 append/contention gates.
- [ ] Commit `feat(store): add fenced journal and outbox`.

### PR 6B: Sanitized blob staging, protected quarantine/key service, publication, and GC

**Files:** create `src/blob/{mod,id,crypto,staging,repository,integrity,gc}.rs`, `src/{quarantine,privacy_manifest,key_service,secure_retire}.rs`; create `tests/{blob_contract,protected_quarantine}.rs`; extend `tests/recovery_contract.rs`.

- [ ] Add failing tests `ordinary_blob_rejects_unclassified_or_classified`, `receipt_must_match_sanitized_payload`, `same_content_same_domain_dedupes`, `privacy_domain_changes_blob_id`, `key_epoch_changes_blob_id`, `retention_class_changes_blob_id`, `publish_requires_verified_stage`, `protected_quarantine_uses_random_id_and_separate_key`, `locked_keyring_fails_to_sanitized_only_or_drop`, `quarantine_plaintext_has_no_sqlite_wal_temp_log_or_backup_copy`, `crash_after_rename_recovers_orphan`, `missing_referenced_blob_is_partial`, `gc_marks_from_manifests_not_refcount`, `hold_blocks_gc`, and `secret_bytes_never_enter_sqlite_or_log`.
- [ ] Run `cargo test -p tracedecay-store --test blob_contract --test recovery_contract -- --nocapture`. Expected: blob repository is absent.
- [ ] Implement HMAC domain IDs for sanitized blobs, optional zstd, ordinary protected-blob encryption, exclusive private staging, hash/size/header verification, file/directory fsync, atomic rename, transactional owner refs/outbox, key epochs, manifest-based mark/sweep, and 24-hour recovery grace. Add Plan 18's separate random-ID per-record-DEK protected quarantine/key service with TTL/access audit/cryptographic deletion; it never dedupes or joins the ordinary blob/backup path.
- [ ] Re-run tests plus permission/secret scans. Expected: byte/hash/permission assertions pass; no cross-domain equality leak; killed publication never creates a false committed ref; unclassified bytes are impossible in every ordinary storage API.
- [ ] Commit `feat(store): add privacy-domain payload storage`.

### PR 6C: Packed immutable graph generations and snapshot publication

**Files:** create `src/migrations/sql/graph/0001_generation.sql`, `src/graph/{mod,manifest,generation,overlay,compaction,recovery}.rs`; create `tests/graph_generation.rs`; create `benches/graph_generation_policy.rs`; extend recovery tests.

- [ ] Add failing tests `generation_is_immutable_after_seal`, `snapshot_maps_to_one_generation_chain`, `resolved_tuple_opens_only_named_generation`, `base_checkout_mismatch_never_opens_pr_worktree_graph`, `two_refs_can_share_one_snapshot_generation`, `failed_validation_never_swaps_manifest`, `reader_survives_manifest_swap`, `orphan_final_is_quarantined`, `non_sqlite_generation_header_is_quarantined`, `disk_full_generation_never_publishes`, `overlay_depth_is_bounded`, `compaction_preserves_snapshot_hashes`, and `branch_ref_does_not_create_database_copy`.
- [ ] Run `cargo test -p tracedecay-store --test graph_generation --test recovery_contract -- --nocapture`. Expected: generation/manifest APIs are absent.
- [ ] Implement staged generation builders, sealed manifests, overlay chains, project-writer compare-and-swap publication, pinned readers, orphan recovery, deferred generation GC, and bounded file handles. Branch/worktree/ref is a pointer/entity, never physical database ownership.
- [ ] Run the 32/64/128-snapshot, 512 MiB/1 GiB/2 GiB pack, and 2/4/8-overlay matrix from Section 10 at current and 10x corpora. Record the selected policy in the storage ADR before constants are fixed.
- [ ] Re-run tests and benchmark. Expected: identical snapshot row/hash manifests before/after compaction; failed/killed publication leaves the old manifest active; performance/file-count gates pass.
- [ ] Commit `feat(store): add packed graph generations`.

### PR 6D: Retention, integrity, backup/restore, startup recovery, and repair

**Files:** create `src/retention/{mod,preview,apply,holds}.rs`, `src/integrity/{mod,sqlite,catalog,report}.rs`, `src/backup/{mod,snapshot,restore,verify}.rs`, `src/recovery/{mod,startup,killpoint}.rs`; create/extend `tests/{retention_contract,recovery_contract}.rs`.

- [ ] Add failing tests `exact_cutoff_is_retained`, `hold_precedes_deletion`, `preview_binds_vector_watermark`, `apply_tombstones_before_blob_gc`, `reasoning_defaults_to_30_days`, `backup_uses_captured_vector`, `restore_allocations_before_projection`, `missing_allocation_ledger_stops_restore`, `corrupt_project_does_not_hide_healthy_shards`, `update_waits_for_writer_drain_before_checkpoint`, `service_state_survives_maintenance`, `catalog_rebuild_uses_safe_manifests`, and `repair_requires_preview_and_expected_version`.
- [ ] Run `cargo test -p tracedecay-store --test retention_contract --test recovery_contract -- --nocapture`. Expected: missing services/receipts fail.
- [ ] Implement leased preview/apply deletion, holds, exact `< cutoff`, descendant index cleanup, tombstone/deletion receipts, online SQLite backups, graph/blob inventories, restore verification, startup spool/stage/lease/outbox recovery, catalog rebuild, quarantine isolation, and preview/apply repair commands.
- [ ] Inject every killpoint in Section 12 and restore from copied multi-shard backup with one corrupt project, one locked privacy domain, active gaps, old graph readers, and live ingest above the frozen vector.
- [ ] Re-run tests. Expected: healthy shards remain queryable; no identity remint, early deletion, false completeness, or unverified repair; restore reproduces manifest digests at the captured vector.
- [ ] Commit `feat(store): add retention backup and recovery`.

### PR 8: Identity and alias resolver persistence

**Ordering:** execute after capture PR 7 has frozen provider/source alias evidence. This is the store-owned slice of master PR 8; application/root composition supplies authorization and presentation, while domain owns canonical IDs/evidence types.

**Files:** create `src/identity/{mod,resolver,aliases,candidates,conflicts}.rs`; extend `src/catalog/identity.rs`, activity/project alias repositories, `tests/identity_resolution.rs`, and copied-store fixtures.

- [ ] Add failing tests for exact native IDs, repository remote/common-history candidates, checkout/worktree Git-admin identity, moved/symlink/case/path aliases, detached HEAD, ref move/rebase/force-push, rewritten transcript generations, actor/session/message provider collisions, PR #405 adopted/split stores, and PR #407 source-only Hermes aliases.
- [ ] Persist alias values only in their authorized activity/project privacy domain; catalog receives keyed alias digests, kind/owner/status/validity/provenance, never path/name/remote literals. Resolution accepts protected query digests plus authorized shard candidates and returns stable IDs, evidence, alternatives, validity, and conflict status.
- [ ] Preserve zero/one/many candidates. Exact stable/native identity may resolve directly; path, time, proximity, or newest-mtime evidence alone never collapses ambiguity. A later correction appends a superseding assertion and leaves the earlier candidate history queryable.
- [ ] Add concurrency and recovery cases for 64 resolvers racing the same allocation, conflicting owners, alias validity change during read, stale writer lease, and crash between allocation/alias/evidence/outbox writes. The transaction is all-or-retry.
- [ ] Run `cargo test -p tracedecay-store --test identity_resolution --test recovery_contract`; expected: all identities are stable, ambiguity is visible, catalog contains no literals, and killed writes cannot publish partial identity state.
- [ ] Run the PR #405/#407 identity compatibility fixtures and `cargo clippy -p tracedecay-store --all-targets -- -D warnings`; expected: zero reminted healthy identities and every collision has a disposition.
- [ ] Commit `feat(store): persist identity and alias resolution evidence`.

### PR 8A: Canonical cross-project scope-candidate substrate

**Ordering:** master PR 8A is a cross-crate slice. Domain contributes `ScopeSelectorV2`; this section supplies content-free registry reconciliation and exact typed candidates. The authorized natural-language/token/alias/relationship orchestration and final `ScopeResolutionV2` are the shared application service specified by the application and cross-project plans, not store SQL or transport code.

**Files:** create `src/catalog/scope.rs`; extend catalog/project repositories and migrations; create `tests/scope_resolution.rs`; add the Rspack/Rsbuild/React Router, moved-repo, duplicate-store, same-name, non-Git, stale-registry, and base-checkout-versus-PR-worktree fixtures.

- [ ] Add failing tests for explicit one/many/all-authorized profile roots; repository/project/checkout/worktree/ref/snapshot/generation/session/agent/workflow selections; saved sets; exact stable handle; token-channel candidate input; alias/remote/path candidate input; related-scope proposals; authorization filtering; and typed retry candidates.
- [ ] Implement `CatalogRepository::scope_candidates` over the full selector and reconciliation tuple. It returns catalog evidence, aliases, statuses, and physical capability health at one generation; empty explicit roots, `sessions.project_key`, first Claude CWD, process CWD, active base checkout, current graph, ignored dependency hints, and registry first-match never become fallback scope.
- [ ] Return typed candidates/missing aliases, registry/index/ref watermarks, and store/capability health. The store does not produce `ScopeResolutionV2`, open activity shards, authorize, default current scope, score ambiguity, or rank relationship evidence; the application scope service composes these candidates with authorized cross-project activity relations and emits the final resolution. An absent graph does not make activity, memory, Git, catalog, or automation unavailable.
- [ ] Prove search hit -> exact cross-project session/message/Turn/entity locator -> adjacent context/source observation works without caller store/CWD switching, using opaque locators and authorization at every hop.
- [ ] Run `cargo test -p tracedecay-store --test scope_resolution`; expected: exact tuple and coverage assertions pass for one project, saved related-system set, and explicit All.
- [ ] Run privacy/schema introspection and the shared transport/SDK schema-generation contract from PR 8A; expected: no public `project_key`, store path, graph filename, or catalog literal.
- [ ] Commit `feat(store): expose canonical federated scope candidates`.

### PR 9: Evidence relation store

**Files:** finalize `EvidenceRepository`; extend `src/activity/relations.rs`, `src/project/relations.rs`, migrations, outbox/projection rows; create `tests/evidence_relation.rs`; extend recovery/import tests.

- [ ] Add failing tests for legal/illegal predicate endpoints, bitemporal half-open validity/knowledge intervals, evidence classes, supporting observation/event IDs, finite confidence/rationale, producer version, provenance, sensitivity, supersession, tombstone, inverse lookup, and scope ownership.
- [ ] Add copy-lint fixtures proving inferred/heuristic/candidate relations cannot render causal verbs such as created, changed, caused, produced, or modified. Direct/provider-declared evidence is required by predicate registry for those labels.
- [ ] Implement append-only relation assertions, evidence-link rows, registered bounded subject/object/predicate/as-of reads, transactional relation/outbox append, supersession without mutation, and retention tombstones without deleted literals.
- [ ] Inject concurrent contradictory assertions and kills before/after assertion/evidence/outbox/checkpoint writes; prove either the complete assertion publishes or retry is safe and earlier knowledge remains queryable.
- [ ] Run `cargo test -p tracedecay-store --test evidence_relation --test recovery_contract`; expected: bitemporal, copy-lint, atomicity, and privacy assertions pass.
- [ ] Run PR 9 differential fixtures over V1 Git/session/code/memory correlations; expected: each link is exact, expected evidence-version change, candidate, quarantined, or unexplained, with unexplained blocking PR 10.
- [ ] Commit `feat(store): persist bitemporal evidence relations`.

### PR 33S: Store-owned read-only V1 import, incoming-master parity, and resumable receipts

**Files:** create `src/import/{mod,inventory,v1_catalog,v1_activity,v1_graph,v1_payload,legacy_identity,hermes}.rs`; create `tests/import_parity.rs` and the four fixtures listed in Section 4; extend migration/recovery tests.

- [ ] Add failing copied-store cases for every V1 table/sidecar/payload family, interrupted resume, repeated import, unknown schema, missing payload, PR #405 unique/adopted/conflicting identities, PR #407 moved/facts-only/collision scopes, and PR #410 native/direct/subagent/tool-result/protocol/representative counts.
- [ ] Run `cargo test -p tracedecay-store --test import_parity -- --nocapture`. Expected: importer and receipts are absent.
- [ ] Implement the nine-step import order in Section 13 with read-only source opens, logical source manifests, stable allocation mapping, per-domain checkpoints, counts/hashes/offsets/payload inventory/quarantine, and signed parity receipts. Never scan a Hermes runtime destination, mutate a V1 file, or collapse copied prompt rows at ingest.
- [ ] Re-run from publication base `66584b4d` with merged #405/#410/#411/#412/#413/#414/#415/#416/#417/#419 and open-assumed #407/#418/#420 fixtures recorded separately, then regenerate manifests from that exact base. Expected: second import emits zero canonical additions; every difference has a named disposition; sanitized-native #410 counts, #411 ownership/remediation, #417 conflict state, #419 edit receipts, #420 routing authority, and lifecycle receipts all match.
- [ ] Commit `feat(store): import v1 evidence with parity receipts`.

### PR 35A: Store cutover, rollback window, and deletion proof

**Files:** root composition/feature-flag adapters owned by the execution PR; store cutover receipt schema; extend `tests/{import_parity,recovery_contract}.rs`; generated PR 34/35 manifests.

- [ ] Shadow one bounded context at a frozen vector and compare V1/V2 source heads, identities, native rows, event/relation counts, graph snapshots, payload hashes, ownership scopes, outbox/projector lag, and query parity.
- [ ] Require zero unexplained parity/identity conflict/dead letter, 24 hours within visibility/latency gates, backup/restore proof, and a successful route/lease rollback drill before changing one context's effect/read owner.
- [ ] Roll back by fencing V2 writer/effect ownership, restoring V1 source-offset/read routing from the receipt, and preserving V2 stores read-only. Never reverse-delete V2 evidence.
- [ ] Keep V1 stores read-only through the declared data rollback window, but expose no live old-client/store-protocol fallback. Stale MCP/daemon/plugin/hook/CLI clients fail the exact protocol handshake with restart/update/current-capability remediation. Before V1 data deletion, require signed archive/export, closed rollback window, no catalog locator/backup/replay/hold referencing the V1 source, zero unexplained parity, and an explicit user-approved delete command with preview.
- [ ] Run the complete store crate plus named V1 storage/session/LCM/graph/memory/automation compatibility suites. Expected: cutover and rollback receipts verify and raw PR #410 rows remain available in both native and representative views.
- [ ] Commit per bounded context using `refactor(store): cut over <context> to v2`.

## Performance and load gates

- Hook handoff/spool acknowledgement p95 at most 10 ms; committed append p95 at most 20 ms excluding blob I/O.
- 32 concurrent producers × 10,000 observations each yield zero loss, zero divergent duplicate, per-source order preservation, and bounded memory at queue capacity; 1,000 concurrent presence/claim heartbeats update current TTL views without writer starvation or history loss.
- A stalled writer causes durable spool/backpressure, not drops or unbounded heap growth; recovery drains at least 10,000 messages/second excluding embeddings.
- 64 concurrent readers plus live activity/project writers preserve read correctness and keep writer p95 within gate.
- Projected visibility p95 at most two seconds; outbox lag and oldest age are observable.
- WAL remains at most 1 GiB per shard before controlled checkpoint; one query opens at most 32 shards; read pool at most eight connections per shard.
- Catalog is at most 5% of canonical content size and contains zero secret corpus hits or user/query literals.
- Migration disk amplification at most 2.25× source size with 25% preflight headroom.
- GC reclaims at least 95% of eligible bytes per pass without deleting held/referenced/staged/grace blobs.
- Corrupt/missing/incompatible one-project shard does not prevent catalog, activity, or other project reads.

## Definition of done

- The proposed module tree, public repository ports, SQLite schemas, migrations, consumes/produces contracts, and every PR/TDD task above exist without a forbidden dependency.
- Catalog, activity, project, graph, and blob ownership match `tracedecay-domain`; canonical transcript bodies exist only in activity, while profile-scoped and project-scoped knowledge/policy/skill/automation histories follow `DeclaredScope`.
- Catalog scope candidate lookup preserves all explicitly selected repositories/projects/checkouts/worktrees/refs/snapshots/generations; application resolution returns ambiguity/stale/unavailable/quarantine evidence, and graph open verifies the resolved tuple. No current-project/CWD/first-row/base-checkout/current-generation fallback exists.
- Every ordinary content write requires the domain `SanitizedPayload` or sink-eligible wrapper and matching receipt. Only the isolated Plan 18 protected-quarantine port may accept transient `Unclassified` bytes, and it cannot feed general reads, indexes, exports, or backups.
- One fenced writer per shard, bounded ingress/spool, atomic observation/outbox/source-head commits, vector-watermark reads, outbox leases, and command idempotency survive the full concurrent/kill matrix without silent loss or false acknowledgement.
- Identity allocation, backup/restore, graph publication/compaction, blob publication/GC, retention, repair, and importer reruns are deterministic, crash-safe, privacy-safe, and manifest-verifiable.
- Master PR 8/8A/9 identity, federated-scope, and bitemporal-relation slices preserve ambiguity/provenance/authorization, keep catalog content-free, and pass moved-repo/provider-collision/cross-project/copy-lint/recovery fixtures.
- #405 adoption, #407 profile consolidation, #410 raw/native plus representative behavior, #411 foreign ownership, and #412 lifecycle drain are in the actual base, fixture-locked, and cutover-proven; #413 contributes its actual release/protocol version; #409 remains historical only.
- V1 deletion is not part of import or cutover. It requires the declared data rollback window, archive, zero unexplained parity, no live references/holds, rollback closure, preview, and explicit approval. This data safety does not activate old runtime clients or names.
- All focused tests, full crate tests, clippy, docs, current/10x benchmarks, copied-store parity, backup/restore, shadow, cutover, and rollback gates pass with recorded manifests.
