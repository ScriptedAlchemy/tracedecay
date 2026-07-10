# TraceDecay V2 Domain Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a pure `tracedecay-domain` crate that defines the one stable identity, evidence, ownership, scope, privacy/taint, retention, ordering, query, cursor, and optimistic-command vocabulary consumed by every V2 crate and transport.

**Architecture:** The crate contains immutable value types, deterministic ID derivation, validation, and versioned schema/predicate registries; it performs no filesystem, database, network, runtime, or transport work. Exact source identities use deterministic namespaced UUIDs, ambiguous entities use persisted UUIDv7 allocations supplied by `tracedecay-store`, and cross-shard state is represented by vector watermarks rather than a fabricated global sequence.

**Tech Stack:** Rust 2024; `serde`; `serde_json`; `schemars`; `uuid` with `serde`, `v5`, and `v7`; `sha2`; `thiserror`; `proptest` and `jsonschema` for tests.

[`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) consumes this crate for canonical initiative/plan/work-item versions, gates, acceptance, assignments, executor routes, fenced leases/attempts, workspace bindings, context packets, handoffs, artifacts, outcomes, task events, and task-query types. Those remain domain contracts here; plan 24 does not create a monolithic task crate or parallel identity/scope/evidence vocabulary.

---

## Goals

- Make row IDs, file paths, provider-native strings, and transport JSON incapable of becoming canonical public identity by accident.
- Make the profile activity shard, project/privacy-domain shards, graph generations, and catalog ownership rules explicit in types.
- Define deterministic source/observation IDs and the allocation request used to persist UUIDv7 assignments.
- Define immutable observations, canonical events, bitemporal relation assertions, provenance, confidence, and supersession.
- Make legal entity/event/predicate combinations derive from one versioned registry.
- Define exact half-open time, retention-horizon, source-ordering, cursor, vector-watermark, and optimistic-command semantics.
- Keep unknown provider fields lossless as opaque payload content while preventing them from becoming indexed/query-semantic fields without a registry version.
- Produce JSON Schema and stable fixture digests consumed by storage, capture, query, application, HTTP, MCP, CLI, export, and dashboard code.

## Convergence boundary

This plan is the type authority inside the converged system described by [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md). Cross-cutting semantics come from [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md), [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md), [`20-configuration-control-plane.md`](20-configuration-control-plane.md), [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md), and [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md); this file owns their exact Rust value contracts and generated-schema names, not competing implementations. Plan 20 owns configuration product semantics and the registry/resolver contract; this crate owns only the pure config IDs, values, provenance, version, impact, and schema primitives that contract names. Plans 22–23 own scout/temporal product semantics while this crate owns their exact IDs, addresses, occurrence/copy/summary/assertion/answer-mode/envelope schemas, and reason-code registries.

| Boundary | Contract |
|---|---|
| Enters | ADR-locked identity/privacy/scope/evidence semantics and safe primitive values; no provider bytes, I/O handles, ambient state, or transport JSON. |
| Exits | Versioned pure value types, validators, canonical encoders, schema/predicate registry, JSON Schema, and fixture digests. |
| Upstream owner | Master plan and cross-cutting plans 16–19 own product semantics and global phase gates. |
| Downstream owners | Store persists; capture constructs sanitized observations; projectors derive canonical evidence; query plans; policy evaluates; application applies; catalog/API/UI generate or render. |
| Extension seam | Add a versioned registry entry/type plus ADR, golden canonical encoding, migration mapping, privacy eligibility, and generated-schema update; never add a transport-local enum or string alias. |
| Scale/concurrency | Values are immutable and bounded; independent shard/source progress is a vector watermark; no process-local identity or global scalar sequence. |
| Migration/retirement | V1 values enter only through typed import evidence. Once all consumers use generated V2 schemas and parity receipts pass, duplicate V1 model modules are removed rather than wrapped indefinitely. |

## Non-goals

- No SQLite, `rusqlite`, `libsql`, SQL strings, paths, file permissions, locks, queues, threads, async runtime, or clock access.
- No source parsing, provider normalization, redaction engine, identity resolution, projection, ranking, query execution, cursor signing, encryption, or blob I/O.
- No V1 type aliases that expose V1 strings as V2 IDs.
- No hidden reasoning representation. `ReasoningArtifact` represents only provider-exposed material and always carries visibility and format.
- No global total order across independent sources or shards.
- No remote libSQL adapter or multi-tenant authorization model in the first V2 default.

## Authoritative ownership decisions

1. `activity.db` owns provider transcript observations, canonical sessions/messages/content parts, actors, agent instances, tool activity, goals, workflows, handoffs, cross-project activity search, session-to-project relation assertions, and profile/zero-project/cross-project knowledge, skills, policies, automations, saved-view content, and annotations.
2. `project.db` owns repository/project observations, Git/delivery evidence, project-scoped knowledge/policy/automation, project search projections, and opaque activity locators; it never copies canonical message content.
3. Scope-sensitive kinds require an explicit `DeclaredScope`; they are never forced into an arbitrary project or duplicated when reused across projects.
4. `catalog.db` owns profile/global allocations, shard metadata, health/capabilities, safe aggregate statistics, opaque locators, migration receipts, and outbox/projection watermarks. It cannot contain message text, query literals, annotations, raw paths classified as sensitive, or project payloads.
5. Graph generations own immutable snapshot occurrences and edges. Project rows point to explicit `CodeSnapshotId` and `GraphGenerationId`; a physical generation is never an entity identity.
6. Blobs are addressed inside a `BlobDomainId` composed from privacy domain, encryption-key epoch, and retention class. Deduplication never crosses that boundary.
7. An event has one owning shard and zero or more project-attribution relation assertions. Canonical activity never has a required primary `project_id`.

## Current V1 seams and migration inputs

| V1 seam | Current symbol or file | V2 treatment |
|---|---|---|
| Path-derived project identity and layout | `src/storage.rs`: `EnrollmentMarker`, `RepositoryIdentityMarker`, `ProjectIdentity`, `StoreLayout`, `GraphScopeId`, `default_profile_project_id` | Import strings as aliases/evidence. Allocate or derive V2 IDs under the rules below; never reuse the SHA-256 path prefix as `EntityId`. |
| Legacy repository-store adoption | merged PR #405: `src/storage.rs::matching_legacy_profile_layouts`, `src/storage.rs::retire_identity_cutover_manifest`, `src/tracedecay/lifecycle.rs::resolve_store_layout_with_identity_migration`, `StoreIdentityInventory` | Treat adoption receipts, candidate inventories, repository markers, aliases, and conflicts as V2 import evidence. Preserve split identities as candidates until parity resolves them. |
| Global registry plus activity monolith | `src/global_db.rs`: `CodeProjectRecord`, `ProjectAliasRecord`, `StoreInstanceRecord`, `GraphScopeRecord`, `SessionRecord`, `SessionMessageRecord`, `ParseOffset`, `GlobalDb` | Split canonical activity from project/catalog data. Import row identity only as provenance; canonical UUIDs come from deterministic keys or allocation ledger. |
| Graph schema/migrations | `src/db/migrations.rs`: `LATEST_VERSION`, `create_schema`, `migrate`; `src/db/connection.rs::Database` | Map nodes to snapshot occurrences and stable symbol entities. V1 schema version and database hash become import-manifest fields. |
| Session/provider types | `src/sessions/mod.rs`, `src/sessions/shared.rs`, `src/sessions/source.rs`, provider modules under `src/sessions/` | Capture adapters translate these into `ObservationEnvelopeV1`; provider-native identifiers remain aliases and provenance. |
| Session-origin classification and copied-prompt representatives | merged PR #410: `src/sessions/message_noise.rs`, message search/LCM query filters, CLI/MCP schemas and regressions | Model native row, origin classification (`direct_user`, `subagent`, `tool_result`, protocol/unknown), representative membership, and derivation evidence separately. Raw observations are never deleted by representative views. |
| Duplicated LCM message model | `src/sessions/lcm/types.rs`, `src/sessions/lcm/schema.rs::LCM_SCHEMA_VERSION`, `ensure_lcm_schema` | Canonical content stays in activity entities/events; LCM DAG and compression state become derived lineage projections with explicit source coverage. |
| V1 retention semantics | `src/retention.rs`: `RetentionConfig`, `RetentionTable`, `prune_table` | Preserve the strict “older than cutoff” boundary, but use required `ingested_at` as the V2 retention anchor and retain tombstone/provenance skeletons. |
| Hermes legacy consolidation | incoming PR #407: `src/migrate/hermes.rs`: `LegacyHermesMigration`, `MigrationMarker`, `migrate_legacy_hermes_stores`, fact/session copy functions | Import `~/.hermes` only as a source. Sessions/LCM and profile/zero-project/cross-project or unresolved facts/skills/policy/automation land in activity; explicitly project-scoped equivalents land in that canonical project shard. The migration ledger and logical source fingerprint are parity evidence. |
| Runtime drain and lifecycle serialization | merged PR #412: `src/lifecycle_lease.rs`, daemon/service/update shutdown changes | Model lease epoch, drain intent, writer quiescence, checkpoint completion, service state, and shutdown receipt as distinct typed evidence. A restart/update may not imply writers drained or WAL checkpointed without the receipt. |
| Foreign managed-skill ownership and remediation | merged PR #411: `package_is_foreign_to_installation`, `SkillDrift::ForeignOrphan`, doctor/removal agreement | Model installation owner, scope, drift classification, severity, and remediation capability separately. A foreign/legacy package is informative evidence and cannot receive a destructive/update remediation owned by another installation. |

Planning began at `99ad19bc`; publication master `9f7a1108` later merged #410/#411/#413/#414/#415/#416/#417/#419/#420/#422. Open #407/#418/#423 remain live refresh inputs. PR #409 was closed unmerged. Immediately before PR 4, regenerate the exact crate/schema/protocol/tool inventory from current master; #414/#419 require a cataloged race-safe move-symbol capability contract, #417 identity-conflict states must remain visible rather than collapsed, and #423's fact-rank/counter behavior is future input until merge.

## Proposed crate tree

```text
crates/tracedecay-domain/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── id.rs
│   ├── source.rs
│   ├── ownership.rs
│   ├── entity.rs
│   ├── time.rs
│   ├── privacy.rs
│   ├── retention.rs
│   ├── replay.rs
│   ├── protocol.rs
│   ├── payload.rs
│   ├── provenance.rs
│   ├── observation.rs
│   ├── event.rs
│   ├── message.rs
│   ├── coordination.rs
│   ├── relation.rs
│   ├── registry.rs
│   ├── policy/
│   │   ├── mod.rs
│   │   ├── bundle.rs
│   │   ├── evaluation.rs
│   │   └── outcome.rs
│   ├── hooks/
│   │   ├── mod.rs
│   │   ├── request.rs
│   │   └── receipt.rs
│   ├── ordering.rs
│   ├── watermark.rs
│   ├── command.rs
│   └── query/
│       ├── mod.rs
│       ├── scope.rs
│       ├── predicate.rs
│       ├── text.rs
│       ├── semantic.rs
│       ├── relation.rs
│       ├── time.rs
│       ├── traversal.rs
│       ├── aggregate.rs
│       ├── sort.rs
│       └── cursor.rs
└── tests/
    ├── id_contract.rs
    ├── ownership_contract.rs
    ├── observation_contract.rs
    ├── message_origin_contract.rs
    ├── coordination_contract.rs
    ├── policy_hook_contract.rs
    ├── relation_registry_contract.rs
    ├── retention_contract.rs
    ├── ordering_watermark_contract.rs
    ├── query_contract.rs
    ├── schema_contract.rs
    └── fixtures/
        ├── observation-envelope-v1.json
        ├── relation-assertion-v1.json
        ├── trace-query-v1.json
        └── schema-digests.json
```

File ownership is strict:

- `id.rs` defines UUID/hash newtypes and deterministic derivation only.
- `source.rs` defines normalized source identity, source position, rewrite generation, and missing-time reason.
- `ownership.rs` defines profile/shard/privacy/blob domains and the activity-versus-project decision table.
- `entity.rs` defines entity kinds, entity references, versions, deterministic keys, and allocation requests.
- `time.rs` defines UTC microsecond timestamps, half-open intervals, bitemporal bounds, and stable display ordering.
- `privacy.rs` and `retention.rs` define the sole sensitivity, sanitization receipt, taint-state, sink-eligibility, marker, and deletion-eligibility vocabulary without detecting, redacting, storing, or deleting content. Plan 18 owns the security semantics; no other crate defines substitute wrappers or receipts.
- `protocol.rs` defines exact runtime handshake/mismatch/remediation value contracts; it contains no fallback-name table.
- `payload.rs` defines hashes, payload descriptors, reasoning visibility, and typed opaque extension fields.
- `registry.rs` is the only authority for legal entity/event/predicate schemas and indexed attributes.
- `message.rs` defines native message origin, derivation/evidence, representative membership, and lossless query-view semantics; it never classifies content itself.
- `coordination.rs` defines privacy-safe agent presence, work claims, scopes, redundancy modes, TTL/status, acknowledgements, and coordination outcome vocabulary; it never performs overlap inference or sends hints.
- `policy/` defines policy bundle/evaluation/outcome references and proposed-effect value contracts, never evaluator execution.
- `hooks/` defines host-neutral hook request/receipt IDs, origin/effect/durability vocabulary, never wire decoding, host rendering, or orchestration.
- `ordering.rs` defines per-source continuity states; `watermark.rs` defines per-shard progress.
- `query/` defines the bounded AST and unsigned cursor claims. Signing and execution belong to query/application adapters.

## Dependency direction

```text
tracedecay-domain
  ↑
  ├── tracedecay-store
  ├── tracedecay-capture
  ├── tracedecay-projectors
  ├── tracedecay-query
  ├── tracedecay-policy
  └── tracedecay-application
        ↑
        └── CLI / MCP / HTTP / dashboard adapters
```

`tracedecay-domain` imports no workspace crate. A CI architecture test rejects `rusqlite`, `libsql`, `tokio`, `axum`, dashboard, MCP, filesystem, and root-crate dependencies in its manifest or source.

## Public identity and source contracts

The implementation must expose these names unchanged:

```rust
pub struct ProfileId(pub uuid::Uuid);
pub struct ShardId(pub uuid::Uuid);
pub struct PrivacyDomainId(pub uuid::Uuid);
pub struct SourceInstanceId(pub uuid::Uuid);
pub struct EntityId(pub uuid::Uuid);
pub struct RepositoryId(pub EntityId);
pub struct ProjectId(pub EntityId);
pub struct CheckoutId(pub EntityId);
pub struct WorktreeId(pub EntityId);
pub struct RefId(pub EntityId);
pub struct ProviderId(pub EntityId);
pub struct ActorId(pub EntityId);
pub struct AgentId(pub EntityId);
pub struct SessionId(pub EntityId);
pub struct ThreadId(pub EntityId);
pub struct TurnId(pub EntityId);
pub struct MessageId(pub EntityId);
pub struct ProjectSetId(pub EntityId);
pub struct ProjectSetVersionId(pub uuid::Uuid);
pub struct PolicyBundleId(pub EntityId);
pub struct CapabilityId(String); // private, grammar-validated `capability.<domain>.<noun>`
pub struct RetrievalAnchorId(pub EntityId);
pub struct EntityVersionId(pub uuid::Uuid);
pub struct ObservationId(pub uuid::Uuid);
pub struct EventId(pub uuid::Uuid);
pub struct RelationId(pub uuid::Uuid);
pub struct ProvenanceId(pub uuid::Uuid);
pub struct ManifestId(pub uuid::Uuid);
pub struct CommandId(pub uuid::Uuid);
pub struct QueryId(pub uuid::Uuid);
pub struct GraphGenerationId(pub uuid::Uuid);
pub struct CodeSnapshotId(pub uuid::Uuid);
pub struct BlobId(pub [u8; 32]);
pub struct BlobIntegrityTag(pub [u8; 32]);
pub struct ContentDigest(pub [u8; 32]);
pub struct ManifestDigest(pub [u8; 32]);
pub struct NaturalKeyDigest(pub [u8; 32]);
pub struct KeyedSourceRecordFingerprint(pub [u8; 32]); // privacy-domain-keyed; never a raw content hash
pub struct PrivacyDomainBoundLocatorDigest(pub [u8; 32]);
pub struct AccessPolicyDigest(pub [u8; 32]);
pub struct NativeEventLocatorDigest(pub PrivacyDomainBoundLocatorDigest);
pub struct NativeKindCode(String); // bounded grammar-validated registry token, never provider payload text
pub struct ComponentVersion(String); // bounded ASCII semver/build grammar
pub struct MediaTypeCode(String); // allowlisted IANA/media grammar, no parameters with literals
pub struct LegacyBindingCode(String); // bounded historical CLI/MCP/HTTP identifier grammar
pub struct SanitizationReceiptId(pub uuid::Uuid);
pub struct ScopeResolutionId(pub uuid::Uuid);
pub struct ScopeSelectorDigest(pub [u8; 32]);
pub struct DataVersionDigest(pub ManifestDigest); // plan 24's data-version pin; a named view of ManifestDigest, not a new digest family
pub struct RoutingGenerationId(pub uuid::Uuid);   // catalog alias-route rebuild generation (plan 02 routing tables)
pub struct RetrievalRecipeId(pub uuid::Uuid);
pub struct MessageOccurrenceId(pub uuid::Uuid);
pub struct LogicalMessageClusterId(pub uuid::Uuid);
pub struct MessageCopyAssertionId(pub uuid::Uuid);
pub struct TemporalAssertionId(pub uuid::Uuid);
pub struct AssertionRelationId(pub uuid::Uuid);
pub struct SummaryNodeId(pub uuid::Uuid);
// Plan 24 task-graph identities are domain contracts in this crate (plan 24 §4.1):
pub struct InitiativeId(pub EntityId);
pub struct PlanId(pub EntityId);
pub struct PlanVersionId(pub EntityVersionId);
pub struct WorkItemId(pub EntityId);
pub struct WorkItemVersionId(pub EntityVersionId);
pub struct DependencyId(pub EntityId);
pub struct AcceptanceCriterionId(pub EntityId);
pub struct TaskDecisionId(pub EntityId);
pub struct AssignmentId(pub EntityId);
pub struct TaskLeaseId(pub EntityId);
pub struct ExecutionAttemptId(pub EntityId);
pub struct ExecutorRegistrationId(pub EntityId);
pub struct ExecutorInstanceId(pub EntityId);
pub struct WorkspaceBindingId(pub EntityId);
pub struct ContextPacketManifestId(pub EntityId);
pub struct HandoffId(pub EntityId);
pub struct TaskArtifactId(pub EntityId);
pub struct TaskOutcomeId(pub EntityId);
pub struct SavedTaskViewId(pub EntityId);
```

Deterministic derivation uses fixed UUIDv5 namespaces published by `id.rs`. Input encoding is version byte `1`, then big-endian length-prefixed UTF-8 fields and fixed-width hash/integer fields. Enum tags use their registry snake-case names. No locale, platform path syntax, JSON object order, wall clock, or process randomness participates.

```rust
pub struct SourceInstanceKey {
    pub profile_id: ProfileId,
    pub system: SourceSystem,
    pub authority_digest: NaturalKeyDigest,
}

pub struct ObservationKey {
    pub source_id: SourceInstanceId,
    pub artifact_digest: NaturalKeyDigest,
    pub rewrite_generation: u64,
    pub offset: u64,
    pub next_offset: u64,
    pub source_fingerprint: KeyedSourceRecordFingerprint,
}

pub struct DeterministicEntityKey {
    pub owning_shard: ShardId,
    pub namespace: EntityNamespace,
    pub natural_key_digest: NaturalKeyDigest,
}

pub struct AllocationRequest {
    pub allocation_key: NaturalKeyDigest,
    pub kind: EntityKind,
    pub owning_shard: ShardId,
    pub source_manifest_id: ManifestId,
}

pub fn derive_source_instance_id(key: &SourceInstanceKey) -> SourceInstanceId;
pub fn derive_observation_id(key: &ObservationKey) -> ObservationId;
pub fn derive_exact_entity_id(key: &DeterministicEntityKey) -> EntityId;
```

Invariants:

- `offset < next_offset`; row-sequence adapters use `next_offset = offset + 1`.
- A source authority is normalized by its adapter, classified, and hashed before entering the domain crate. Raw paths and credentials are forbidden.
- Exact provider/native identities use `derive_exact_entity_id`. Repository moves, ambiguous aliases, inferred symbol lineages, and entities lacking an exact native key use `AllocationRequest`; `tracedecay-store` atomically insert-or-reads one UUIDv7 and must restore that ledger from backup.
- A persisted allocation can never change kind or owning shard. A conflicting request returns `DomainError::IdentityAllocationConflict`.
- SQLite integers never serialize as canonical identity.

Entity-kind-specific IDs are validated newtypes over `EntityId`; they provide compile-time boundaries for scope/API/application contracts while `EntityRef` remains the heterogeneous graph/relation carrier. Conversion to or from `EntityRef` validates the exact registered `EntityKind`; it is never a transmute or unchecked string parse. `CapabilityId` is the separate grammar-validated catalog identifier because its stable public identity is semantic rather than an entity UUID. Catalog/application crates re-export these IDs rather than defining a second identifier family.

## Ownership, entity, and payload contracts

```rust
pub enum ShardKind {
    Catalog,
    Activity,
    Project,
    GraphGeneration,
}

pub struct ShardRef {
    pub profile_id: ProfileId,
    pub shard_id: ShardId,
    pub kind: ShardKind,
    pub privacy_domain_id: PrivacyDomainId,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeclaredScope {
    Profile { profile_id: ProfileId },
    ZeroProject { profile_id: ProfileId },
    Project { project_id: ProjectId },
    CrossProject {
        profile_id: ProfileId,
        project_set_id: ProjectSetId,
        project_set_version_id: ProjectSetVersionId,
        membership_digest: ManifestDigest,
    },
    ImportUnresolved { profile_id: ProfileId, source_manifest_id: ManifestId },
}

// Exhaustive owner rule for every scope-sensitive kind:
// Profile | ZeroProject | CrossProject | ImportUnresolved -> Activity
// Project -> that project's canonical Project shard.
// ImportUnresolved is import/evidence-only: mutation is forbidden until a
// superseding resolved scope is recorded. No route, CWD, selected project, or
// current filter may fill or rewrite a declared scope.

pub struct EntityRef {
    pub id: EntityId,
    pub kind: EntityKind,
}

pub struct OwnedEntityRef {
    pub entity: EntityRef,
    pub owner: ShardRef,
}

pub struct EntityVersionV1 {
    pub entity: EntityRef,
    pub version_id: EntityVersionId,
    pub owner: ShardRef,
    pub schema: SchemaRef,
    pub valid_time: TimeInterval,
    pub observed_at: UtcMicros,
    pub attributes: PayloadRef,
    pub supersedes: Option<EntityVersionId>,
}

pub struct BlobDomainId {
    pub privacy_domain_id: PrivacyDomainId,
    pub key_epoch: u32,
    pub retention_class: RetentionClass,
}

pub struct PayloadRef {
    pub blob_domain: BlobDomainId,
    pub blob_id: BlobId,
    pub integrity_tag: BlobIntegrityTag,
    pub byte_len: u64,
    pub media_type: MediaTypeCode,
    pub schema: SchemaRef,
    pub sensitivity: DataSensitivity,
    pub sanitization_receipt: SanitizationReceiptId,
}
```

### One privacy and taint contract

[`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md) is the security authority. This crate publishes its exact cross-crate proof vocabulary; `tracedecay-capture` is the only runtime sanitizer implementation. Store, projector, query, policy, hook, catalog, application, and transport code may validate or narrow eligibility but may not rescan content or mint a sanitization proof.

```rust
pub enum DataSensitivity {
    CatalogSafe,
    Normal,
    Sensitive,
    SecretLike,
    SecretConfirmed,
    Reasoning,
    RedactedDerived,
    Unknown,
}

pub struct SanitizationReceiptV1 {
    pub receipt_id: SanitizationReceiptId,
    pub source_observation_id: ObservationId,
    pub policy_digest: PrivacyPolicyDigest,
    pub detector_set_digest: DetectorSetDigest,
    pub parser_digest: ParserDigest,
    pub input_domain: PrivacyDomainId,
    pub input_fingerprint: KeyedPayloadFingerprint,
    pub output_digest: SanitizedOutputDigest,
    pub findings_by_class: std::collections::BTreeMap<SecretClass, u64>,
    pub structured_fields_scanned: u64,
    pub raw_fallback_used: bool,
    pub decode_depth: u8,
    pub completeness: ScanCompleteness,
    pub occurred_at: UtcMicros,
}

pub struct Unclassified<T>(/* private */ T);
pub struct Classified<T>(/* private */ T, DataSensitivity, SanitizationReceiptId);
pub struct Sanitized<T>(/* private */ T, SanitizationReceiptId);
pub struct SanitizedPayload(Sanitized<Vec<u8>>);
pub struct CatalogSafeText(Sanitized<String>);
pub struct SearchEligibleText(Sanitized<String>);
pub struct PromptEligibleText(Sanitized<String>);
pub struct ExportEligibleText(Sanitized<String>);
pub struct LogSafeText(Sanitized<String>);
pub struct ScopeLocatorText(Sanitized<String>);
pub struct ProtectedSecretRef(/* opaque random quarantine reference */);
pub struct ProtectedQuarantineIngress(/* private move-only candidate plus detector decision */);
```

All tuple fields above are private. `Unclassified<T>` is transient capture/parser memory and implements neither `Serialize` nor any repository/transport trait. `Sanitized<T>` is constructible only from a complete `SanitizationReceiptV1` issued by the registered capture sanitizer. Sink wrappers require explicit checked conversion from `Sanitized<T>` under the current privacy policy and access context. The sanitizer alone can consume `Unclassified` content into move-only `ProtectedQuarantineIngress`; it cannot serialize, clone, display, log, index, or cross a general repository/transport port. `ProtectedSecretRef` has no `Display`, public `Serialize`, equality-across-domain, search, prompt, export, or ordinary blob conversion. `PayloadRef` always names sanitized bytes and binds their receipt; protected forensic content uses the separate quarantine port from Plan 18, never `PayloadRef`.

Architecture/compile-fail tests forbid raw `String`, `serde_json::Value`, `Vec<u8>`, `Bytes`, or slices at application-to-store, projector-to-index, policy-to-hint, and application-to-transport content ports. Static catalog metadata uses reviewed `CatalogText`, which is not a conversion from runtime content. Safe redaction markers expose class plus random receipt reference only; no original length, prefix/suffix, plaintext digest, or cross-domain fingerprint is public.

Replay fidelity is one domain vocabulary shared unchanged by capture, projectors, query, policy, hooks, application, API, and labs:

```rust
pub enum ReplayMode {
    ExactDeterministic,
    RecordedResult,
    CurrentBestEffort,
}

pub struct ReplayManifestRef {
    pub manifest_id: ManifestId,
    pub mode: ReplayMode,
    pub payload: PayloadRef,
    pub digest: ManifestDigest,
    pub created_at: UtcMicros,
}
```

`ExactDeterministic` means the executable artifact and every declared input/version/digest are available and verified. `RecordedResult` verifies and renders the stored result without executing. `CurrentBestEffort` runs current or substituted components, reports every substitution/omission, and can never be labeled historical truth.

```rust
pub struct ProtocolEpoch(pub u32);

pub struct RuntimeHandshakeV1 {
    pub protocol_epoch: ProtocolEpoch,
    pub schema_registry_digest: ManifestDigest,
    pub tool_catalog_digest: Option<ManifestDigest>,
    pub client_kind: RuntimeClientKind,
    pub client_version: ComponentVersion,
}

pub enum ProtocolMismatchRemediation {
    RestartClient,
    UpdateClient,
    ReinstallIntegration,
    UseCurrentCapability,
}
```

Handshake acceptance requires the current exact protocol epoch and compatible current digests. Mismatch returns a typed remediation and current catalog digest; it cannot carry or execute an old tool-name alias/fallback.

`EntityKind` includes every master-plan kind: profile/project/project-set/repository/remote/checkout/worktree/ref/commit/tree/pull-request/check/review/release; provider/host/model/installation/actor/agent/agent-presence/work-claim/session/thread/workflow/run/turn/message/content-part; tool definition/invocation/result/approval/goal/provider-native-task/provider-native-plan; initiative/plan/plan-version/work-item/work-item-version/task-dependency/acceptance-criterion/task-decision/task-assignment/task-lease/execution-attempt/executor-registration/workspace-binding/context-packet/handoff/task-artifact/task-outcome; code snapshot/file and symbol identity/occurrence/diagnostic/test/build; fact/version/knowledge entity/decision/contradiction/retrieval/feedback; policy bundle/evaluation/hint; automation job/run/artifact/skill/skill-package/proposal/doctor-finding/remediation; lifecycle lease/drain/checkpoint/service-state receipt; query/saved view/annotation/export/payload blob. Provider-native task/plan records remain observed entities or aliases until an authorized materialization command creates canonical work.

Coordination contracts are explicit:

```rust
pub enum WorkIntent { Read, Write, ReadWrite }
pub enum CoordinationScopeKind { Repository, Worktree, Ref, PullRequest, File, Symbol, Query }
pub enum WorktreeProximity { SameWorktree, ParallelWorktree, SameRepository, CrossRepository }
pub enum PresenceStatus { Active, Idle, HandedOff, Completed, Blocked, Expired }
pub enum WorkClaimStatus { Planned, Active, Acknowledged, HandedOff, Completed, Cancelled, Expired }
pub enum CoordinationOutcome {
    Eligible,
    Emitted,
    Suppressed,
    Acted,
    HandedOff,
    DuplicateAvoided,
    FalsePositive,
    Unresolved,
}
pub enum RedundancyMode {
    AccidentalOverlapRisk,
    DeliberateEnsemble,
    DiverseReview,
    SharedExecution,
    SequentialHandoff,
}

pub struct SafeCoordinationSummary(CatalogSafeText); // opaque disclosure-safe UTF-8 <=160 chars

pub enum RetrievalAnchorTargetV1 {
    Entity(EntityRef),
    Query(QueryId),
    SourcePosition { source: SourceInstanceId, position_digest: PrivacyDomainBoundLocatorDigest },
    Artifact { artifact: EntityRef, sanitized_output_digest: SanitizedOutputDigest },
}

pub enum SourceIdentityClass { ProfileActivity, ProjectEvidence, GraphGeneration, BlobArtifact, ExternalDelivery }
pub enum RetrievalViewV1 { SanitizedNative, Representative, EntityVersion, QueryResult, SourceObservation }
pub enum RetrievalExpansionMode { ExactTarget, AdjacentContext, RepresentedMembers, SourceLineage }
pub struct RetrievalExpansionRecipeV1 {
    pub capability_id: CapabilityId,
    pub expansion: RetrievalExpansionMode,
    pub bounded_arguments_digest: PrivacyDomainBoundLocatorDigest,
}
pub enum PayloadAccessState { Eligible, Redacted, Quarantined, RetentionExpired, Unavailable }
pub enum AnchorDurabilityClass { DurableEvidence, RetentionBound { expires_at: UtcMicros }, Archived }

pub struct RetrievalAnchorRecordV1 {
    pub anchor_id: RetrievalAnchorId,
    pub target: RetrievalAnchorTargetV1,
    pub target_kind: EntityKind,
    pub resolved_scope_id: ScopeResolutionId,
    pub privacy_domain_id: PrivacyDomainId,
    pub access_policy_digest: AccessPolicyDigest,
    pub source_identity_class: SourceIdentityClass,
    pub immutable_source_refs: Vec<EntityRef>,
    pub source_observations: Vec<ObservationId>,
    pub snapshot: VectorWatermark,
    pub schema_registry_digest: ManifestDigest,
    pub capability_catalog_digest: ManifestDigest,
    pub data_version_digest: DataVersionDigest,
    pub projection_version: ComponentVersion,
    pub view_algorithm_version: Option<ComponentVersion>,
    pub view: RetrievalViewV1,
    pub expansion_recipe: RetrievalExpansionRecipeV1,
    pub canonical_request_digest: PrivacyDomainBoundLocatorDigest,
    pub provenance: Vec<ProvenanceId>,
    pub payload_access: PayloadAccessState,
    pub retention_class: RetentionClass,
    pub created_at: UtcMicros,
    pub durability: AnchorDurabilityClass,
}

// Public responses/deep links expose only RetrievalAnchorId. Resolution loads
// this safe-metadata record under current authorization and returns exact,
// moved/adopted, redacted, retention-expired, unavailable, or denied state;
// it never redirects to a merely similar entity.

// SafeCoordinationSummary disclosure is independently authorized for the
// recipient/scope. Prompt injection performs a separate checked conversion to
// PromptEligibleText and records the policy/receipt; catalog-safe display
// eligibility alone is not prompt eligibility.

// The portable multi-anchor recipe is a domain contract owned here. Plan 09
// produces and consumes it; plans 11 and 13 cite this definition rather than
// defining their own. Recipes contain no literal prompt/query/path secret,
// cursor, response-handle token, or remote credential.
pub struct RetrievalRecipeV1 {
    pub recipe_id: RetrievalRecipeId,
    pub use_case: UseCaseId,
    pub anchors: Vec<RetrievalAnchorId>,
    pub protected_input: Option<ProtectedContentRef>,
    pub canonical_input_digest: PrivacyDomainBoundLocatorDigest,
    pub scope: ScopeSelectorV2,
    pub time: InvestigationTime,
    pub message_view: Option<MessageView>,
    pub schema_catalog_ranking: VersionSet,
    pub freshness: FreshnessRequirement,
}

pub struct UseCaseId(String); // grammar-validated `usecase.<domain>.<verb-noun>`; the use-case registry is owned by plan 08
pub struct ProtectedContentRef(/* opaque random protected-draft reference; no Display or public Serialize */);
pub struct InvestigationTime {
    pub window: Option<TimePredicate>,
    pub temporal: Option<TemporalClauseV1>,
}
pub struct VersionSet {
    pub schema_registry_digest: ManifestDigest,
    pub capability_catalog_digest: ManifestDigest,
    pub ranking: RankingProfileRef,
}
pub enum FreshnessRequirement {
    AsRecorded,
    BestEffort,
    RequireCurrent { max_age_seconds: u64 },
}

pub struct WorkClaimScopeV1 {
    pub repositories: Vec<EntityRef>,
    pub worktrees: Vec<EntityRef>,
    pub refs: Vec<EntityRef>,
    pub pull_requests: Vec<EntityRef>,
    pub files: Vec<EntityRef>,
    pub symbols: Vec<EntityRef>,
    pub query_scope: Option<QueryId>,
}

pub struct AgentPresenceV1 {
    pub presence: EntityRef,
    pub agent: EntityRef,
    pub session: EntityRef,
    pub parent_agent: Option<EntityRef>,
    pub goal: Option<EntityRef>,
    pub heartbeat_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub status: PresenceStatus,
    pub provenance_id: ProvenanceId,
}

pub struct WorkClaimV1 {
    pub claim: EntityRef,
    pub agent: EntityRef,
    pub session: EntityRef,
    pub parent_agent: Option<EntityRef>,
    pub goal: Option<EntityRef>,
    pub scope: WorkClaimScopeV1,
    pub intent: WorkIntent,
    pub summary: Option<SafeCoordinationSummary>,
    pub retrieval_anchors: Vec<RetrievalAnchorId>,
    pub redundancy: RedundancyMode,
    pub heartbeat_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub status: WorkClaimStatus,
    pub provenance_id: ProvenanceId,
}
```

Presence/claim events are immutable: started/declared, heartbeat, scope-changed, acknowledged, suppressed-as-planned, handed-off, completed/cancelled, and expired. TTL controls current visibility, not historical deletion. `SafeCoordinationSummary::try_from_classified` rejects control characters, values longer than 160 Unicode scalar values, and any value not already classified catalog-safe; it never truncates or redacts a raw prompt. Missing safe text remains `None`. Retrieval anchors contain canonical IDs plus digests and safe source positions, never prompt/query/path excerpts. Coordination is advisory by default; no domain contract grants cancellation, locking, reassignment, messaging, or mutation authority.

Message-origin and representative views are explicit domain contracts:

```rust
pub enum MessageOrigin {
    DirectUser,
    DelegatedAgentPrompt,
    ToolResultProtocol,
    ProviderProtocol,
    Unknown,
}

pub enum MessageView {
    NativeRows,
    RepresentativeRows,
    HumanBestEffort,
    DirectUser,
    DelegatedAgents,
    ToolResults,
    ProviderProtocol,
}

pub struct MessageOriginAssertion {
    pub message: EntityRef,
    pub origin: MessageOrigin,
    pub representative: Option<EntityRef>,
    pub evidence_class: EvidenceClass,
    pub classifier: ProducerRef,
    pub supporting_observations: Vec<ObservationId>,
}
```

`HumanBestEffort` is never represented as an observed fact unless the provider explicitly marks the author. Representative membership is versioned evidence, not a tombstone or content rewrite. Query responses always report native-row count, returned representative count, hidden-copy count, unknown-origin count, and classifier version.

Plan 23's `MessageOccurrenceV1`, `LogicalMessageClusterV1`, and `MessageCopyAssertionV1` are the one canonical copied-message vocabulary; their identifier newtypes live in this crate and plan 23 owns the product semantics. `MessageOriginAssertion` classifies exactly one native occurrence's origin, and representative membership is expressed as logical-cluster membership at a `representative_policy_version` — there is no second membership vocabulary. Plan 02 §11.4 owns the persisted table shapes for this family.

Catalog-safe fields use dedicated types:

```rust
pub struct CatalogEntityLocator {
    pub entity: EntityRef,
    pub owning_shard: ShardId,
    pub opaque_locator: NaturalKeyDigest,
}

pub enum CatalogValue {
    Count(u64),
    Timestamp(UtcMicros),
    Digest(ManifestDigest),
    Kind(EntityKind),
    Health(ShardHealth),
    Version(SchemaVersion),
}
```

There is no catalog string/literal variant. Display names, aliases, queries, annotations, payloads, and source locators remain in encrypted profile/project content storage.

`BlobIntegrityTag` is keyed inside `BlobDomainId`; it and `BlobId` cannot be compared across privacy/key/retention domains. A raw-byte checksum may exist only as a non-serializable transient inside the sanitizer invocation. Persisted source identity/provenance/spool/cursor fields use `KeyedSourceRecordFingerprint`, `SanitizedOutputDigest`, `PrivacyDomainBoundLocatorDigest`, or a non-content manifest digest; no unkeyed secret-content digest crosses the sanitizer.

## Observation, event, and provenance contracts

```rust
pub struct ObservationEnvelopeV1 {
    pub observation_id: ObservationId,
    pub source: SourceRecordRef,
    pub schema: SchemaRef,
    pub parser_version: ComponentVersion,
    pub occurred_at: Option<UtcMicros>,
    pub missing_time_reason: Option<MissingTimeReason>,
    pub ingested_at: UtcMicros,
    pub hints: ResolutionHints,
    pub sensitivity: DataSensitivity,
    pub sanitization_receipt: SanitizationReceiptId,
    pub payload: PayloadRef,
    pub idempotency_key: ObservationKey,
}

pub struct CanonicalEventV1 {
    pub event_id: EventId,
    pub kind: EventKind,
    pub schema: SchemaRef,
    pub owner: ShardRef,
    pub occurred_at: Option<UtcMicros>,
    pub ingested_at: UtcMicros,
    pub actor: Option<EntityRef>,
    pub session: Option<EntityRef>,
    pub run: Option<EntityRef>,
    pub snapshot: Option<EntityRef>,
    pub correlation_id: Option<EntityId>,
    pub causation_id: Option<EventId>,
    pub source_observations: Vec<ObservationId>,
    pub provenance_id: ProvenanceId,
    pub payload: Option<PayloadRef>,
    pub indexed_attrs: TypedAttrs,
    pub sensitivity: DataSensitivity,
    pub retention_class: RetentionClass,
    pub supersedes: Option<EventId>,
}

pub struct ProvenanceV1 {
    pub provenance_id: ProvenanceId,
    pub source_id: SourceInstanceId,
    pub source_locator_digest: PrivacyDomainBoundLocatorDigest,
    pub source_record_fingerprint: KeyedSourceRecordFingerprint,
    pub parser_version: ComponentVersion,
    pub resolver_version: Option<ComponentVersion>,
    pub ingested_at: UtcMicros,
}
```

Validation rules:

- `occurred_at = None` requires `missing_time_reason`; a present occurred time forbids that reason.
- Recomputing the deterministic observation ID from `idempotency_key` must match `observation_id`.
- `source_observations` is nonempty and sorted/deduplicated.
- `causation_id` is accepted only for registry event kinds that support direct causation and must differ from `event_id`; projectors also enforce graph acyclicity.
- Corrections create a new event with `supersedes`; no immutable event body is overwritten.
- Provider extension JSON is stored through `PayloadRef`, and the full attribute set stays lossless in the content-addressed payload blob. `indexed_attrs` carries only registry-declared `AttrKeyId` entries; the store materializes exactly those entries into its registered-attribute index tables (`event_attr_index`, plan 02 §11.3). There is no inline transport attribute shape — blob-complete payload plus registry-indexed attributes is the one shape.
- Canonical provider activity uses an activity owner. Project attribution is expressed through registered relations, never by mutating event ownership.

## Relation and registry contracts

```rust
pub struct RelationAssertionV1 {
    pub relation_id: RelationId,
    pub subject: EntityRef,
    pub predicate: PredicateId,
    pub object: EntityRef,
    pub scope: RelationScope,
    pub valid_time: TimeInterval,
    pub observed_time: TimeInterval,
    pub evidence_class: EvidenceClass,
    pub confidence: Confidence,
    pub confidence_reason: ConfidenceReasonCode,
    pub confidence_rationale: Option<LogSafeText>,
    pub supporting_observations: Vec<ObservationId>,
    pub supporting_events: Vec<EventId>,
    pub producer: ProducerRef,
    pub provenance_id: ProvenanceId,
    pub sensitivity: DataSensitivity,
    pub supersedes: Option<RelationId>,
    pub tombstone: bool,
}

pub enum ConfidenceReasonCode {
    DirectObservation,
    ProviderDeclaration,
    DeterministicDerivation,
    CorrelatedEvidence,
    HeuristicCandidate,
    HumanAdjudication,
}

pub enum RelationScope {
    SubjectOwner,
    ObjectOwner,
    Declared(ShardRef),
}

pub struct PredicateSpec {
    pub id: PredicateId,
    pub owner: BoundedContext,
    pub allowed_subjects: &'static [EntityKind],
    pub allowed_objects: &'static [EntityKind],
    pub inverse: Option<PredicateId>,
    pub cardinality: Cardinality,
    pub minimum_evidence: EvidenceClass,
    pub temporal_requirement: TemporalRequirement,
    pub default_sensitivity: DataSensitivity,
    pub default_retention: RetentionClass,
}

pub struct SchemaRegistryV1;
pub struct PredicateRegistryV1;

impl SchemaRegistryV1 {
    pub fn version() -> RegistryVersion;
    pub fn digest() -> ContentDigest;
    pub fn validate_observation(value: &ObservationEnvelopeV1) -> Result<(), DomainError>;
    pub fn validate_event(value: &CanonicalEventV1) -> Result<(), DomainError>;
}

impl PredicateRegistryV1 {
    pub fn version() -> RegistryVersion;
    pub fn digest() -> ContentDigest;
    pub fn get(id: PredicateId) -> Option<&'static PredicateSpec>;
    pub fn validate(value: &RelationAssertionV1) -> Result<(), DomainError>;
}
```

`EvidenceClass` orders authority as `Heuristic < Inferred < DerivedExact < UserDeclared < ProviderDeclared < Observed`; the registry compares minimum authority but never converts one class into another. `Confidence` is finite and within `[0.0, 1.0]`. Observed/provider/user declarations use confidence `1.0`; derived/inferred/heuristic assertions require a nonempty rationale and producer version.

`RelationScope` names the shard that owns the assertion row: the subject's owner shard, the object's owner shard, or an explicitly declared owner that must equal one endpoint's owner shard. The predicate registry fixes each predicate's scope — activity-to-project attribution predicates are `SubjectOwner`, which is why session-to-project assertions live in activity — and a cross-shard endpoint's non-owning shard holds at most a content-free locator row, never a second copy of the assertion.

The initial predicate set includes explicit activity attribution (`activity_related_to_project`, `activity_related_to_repository`, `activity_observed_in_worktree`, `activity_observed_on_ref`, `activity_used_snapshot`), agent/session/workflow relations, code lineage/change/impact relations, Git/delivery relations, knowledge provenance, policy evaluation/outcome, automation lineage, and blob ownership. Legal endpoints, inverse, cardinality, evidence, sensitivity, and retention are fixture-locked.

## Supporting vocabulary contracts

These names appear throughout this plan and its consumers; they are exact public contracts, not placeholders. Enum variant registries marked plan-owned grow only through a versioned registry revision in the owning plan.

```rust
pub struct SourceSystem(String);      // registry token naming one source family (codex, claude, cursor, git, hooks, lcm_v1, ...)
pub struct EntityNamespace(String);   // registry token naming one deterministic-key namespace
pub struct RegistryVersion(pub u32);
pub struct QuerySchemaVersion(pub u16);
pub struct SchemaRef { pub schema_id: u32, pub schema_version: u16 } // resolves only through SchemaRegistryV1
pub enum EventKind { /* closed registry enum generated from SchemaRegistryV1 event declarations; no free-form variant */ }
pub struct Confidence(f64);           // private; constructor requires a finite value in [0.0, 1.0]
pub struct ProducerRef { pub component: NativeKindCode, pub version: ComponentVersion }
pub struct AttrKeyId(pub u32);        // SchemaRegistryV1-issued indexed-attribute key
pub enum TypedAttrValue { I64(i64), U64(u64), Bool(bool), Time(UtcMicros), Token(NativeKindCode), Digest(ContentDigest), Id(EntityId) }
pub struct TypedAttrs(std::collections::BTreeMap<AttrKeyId, TypedAttrValue>);
pub struct ResolutionHints {          // advisory resolver routing only; never canonical evidence
    pub session: Option<EntityRef>,
    pub thread: Option<EntityRef>,
    pub actor: Option<EntityRef>,
    pub repository: Option<EntityRef>,
    pub provider_kind: Option<NativeKindCode>,
}
pub enum MissingTimeReason { SourceOmitted, SourceUnparseable, ClockDomainUnknown, ImportedWithoutTime }
pub enum SourcePosition {
    ByteOffset { start: u64, end: u64 },
    RowId(i64),
    Sequence(u64),
    ObjectKey(String), // bounded source-internal key that passed adapter normalization; never a filesystem path
}
pub struct SourceRecordRef {
    pub source_id: SourceInstanceId,
    pub artifact_digest: NaturalKeyDigest,
    pub rewrite_generation: u64,
    pub position: SourcePosition,
}
pub struct IndexVersionSet(pub std::collections::BTreeMap<NativeKindCode, ComponentVersion>); // one entry per registered index family (fts, vector, attr, graph)
pub struct ShardCursorPosition { pub watermark: ShardWatermark, pub resume: Vec<u8> } // resume bytes are the store's opaque StoreResumePosition
pub enum SortValue { I64(i64), U64(u64), Time(UtcMicros), F64Bits(u64), Digest(ContentDigest), Id(EntityId) } // floats travel as canonical bit patterns for cross-platform determinism
pub struct AggregateVersion(pub u64);
pub struct RankingProfileRef { pub id: NativeKindCode, pub version: ComponentVersion }
pub enum ShardHealth { Healthy, Degraded, Quarantined, Missing, Incompatible }
pub struct EvidenceRetentionWatermark {
    pub evaluated_at: UtcMicros,
    pub cutoffs: std::collections::BTreeMap<RetentionClass, UtcMicros>,
}
pub struct CatalogText(&'static str); // build-time reviewed static metadata; never constructed from runtime content

// Plan 18 owns the security semantics of these; the exact value contracts live here:
pub struct PrivacyPolicyDigest(pub [u8; 32]);
pub struct DetectorSetDigest(pub [u8; 32]);
pub struct ParserDigest(pub [u8; 32]);
pub struct KeyedPayloadFingerprint(pub [u8; 32]); // privacy-domain-keyed; never a raw content hash
pub struct SanitizedOutputDigest(pub [u8; 32]);
pub enum SecretClass { /* closed detector-class registry owned by plan 18 §8 */ }
pub enum ScanCompleteness { Complete, PartialBudget, PartialTimeout, FailedClosed }
```

`SourcePosition` is constructed only by plan 03's adapters — no adapter invents a second position vocabulary — and lowers into storage columns exactly as plan 02 §11.2 documents. `TypedAttrs`/`AttrKeyId` are the sole indexed-attribute carrier; `IndexVersionSet` values are produced by the store's read surface (plan 02 §9) so cursor claims can bind them.

## Ordering, concurrency-visible state, and watermarks

Domain contracts intentionally expose concurrency without prescribing threads or locks:

```rust
pub enum SourceOrdering {
    Ordered { initial_offset: u64 },
    Unordered,
}

pub enum SourceContinuity {
    Contiguous,
    Duplicate,
    Late,
    Gap { expected_offset: u64, observed_offset: u64 },
    RewriteConflict,
}

pub struct ShardWatermark {
    pub shard_id: ShardId,
    pub outbox_sequence: u64,
}

pub struct VectorWatermark {
    pub components: std::collections::BTreeMap<ShardId, u64>,
}

impl VectorWatermark {
    pub fn partial_cmp_components(&self, other: &Self) -> Option<std::cmp::Ordering>;
    pub fn dominates(&self, other: &Self) -> bool;
    pub fn merge_max(&self, other: &Self) -> Self;
}

pub struct FrozenSnapshot {
    pub captured_at: UtcMicros,
    pub watermark: VectorWatermark,
    pub registry_version: RegistryVersion,
    pub ranking_version: Option<ComponentVersion>,
}

pub enum IngressAck {
    Committed(AppendReceipt),
    DurablyQueued(SpoolReceipt),
}

pub enum AppendDisposition {
    Inserted,
    Duplicate,
    Late,
    Gap,
    Quarantined,
}

pub struct ObservationAppendDisposition {
    pub observation_id: ObservationId,
    pub disposition: AppendDisposition,
}

pub struct AppendReceipt {
    pub shard_id: ShardId,
    pub lease_epoch: u64,
    pub observations: Vec<ObservationAppendDisposition>,
    pub first_outbox_sequence: Option<u64>,
    pub last_outbox_sequence: Option<u64>,
    pub committed_at: UtcMicros,
    pub watermark: ShardWatermark,
}

pub struct SpoolReceipt {
    pub source_id: SourceInstanceId,
    pub spool_sequence: u64,
    pub frame_fingerprint: KeyedSourceRecordFingerprint,
    pub durable_at: UtcMicros,
}
```

Rules:

- Source order exists only within one `(source_id, rewrite_generation)` and is defined by `[offset, next_offset)`. Concurrent sources are not globally ordered.
- A shard outbox sequence orders committed shard transactions, not real-world causation. Cross-shard progress is a `VectorWatermark`.
- Stable display order is `(occurred_at or ingested_at, ingested_at, shard_id, outbox_sequence, entity_id)` and is labeled render order.
- Duplicate observation IDs with the same record/payload digest are successful no-ops. A matching source position with a different digest is a rewrite conflict and enters quarantine.
- Late observations are retained with both occurred and ingested time. Gaps remain visible until closed; a frozen snapshot never silently reorders after capture.
- Only `IngressAck::Committed` authorizes advancing the canonical V2 source cursor. `DurablyQueued` proves durability in the capture-owned spool — plan 03 owns the one spool, its frame format, and its drainer, and `SpoolReceipt` is the only spool receipt type (no crate defines a local variant) — but not journal visibility; replay remains idempotent.
- Cursor and export completeness always name the vector watermark, skipped/unavailable shards, gaps, late counts, and redactions.

## Time and retention semantics

`UtcMicros` is signed Unix microseconds. `TimeInterval` is `[start, end)`; an absent end is open. A zero-width interval is invalid. All query intervals and timeline selections use the same half-open rule.

`RetentionPolicyV1::local_default()` fixes these classes:

| Class | Content horizon | Index/export rule |
|---|---:|---|
| `NormalContent` | Indefinite until explicit policy/delete | Eligible after classification |
| `Reasoning` | 30 days unless pinned | Excluded from FTS, vectors, facts, shares, and exports by default |
| `SecretQuarantine` | 24 hours | Never indexed |
| `ResponseCache` | 7 days | Reconstructable cache only |
| `RawTelemetry` | 180 days | Nonsensitive aggregate rollups may persist |
| `AutomationIntermediate` | 90 days unless pinned by run/artifact policy | Excluded after deletion |
| `TombstoneSkeleton` | Indefinite without deleted content | Metadata/provenance only |

Eligibility is evaluated from required `ingested_at`, not unreliable occurred time. At evaluation time `T`, cutoff is `T - horizon`; content is eligible only when `ingested_at < cutoff`. Content exactly at cutoff remains until the next evaluation. Holds and pins override eligibility. Deletion preserves entity ID, kind, provenance digest, deletion reason/time, and relation tombstone without content. Query/export responses include an `EvidenceRetentionWatermark` and mark replay incomplete when required inputs crossed a retention horizon.

## Query and optimistic-command contracts

```rust
pub enum ScopeLocatorKindV2 {
    StableHandle,
    ProjectName,
    RepositoryNameOrRemote,
    LocalPath,
    WorktreePath,
    RefName,
    PullRequest,
}

pub struct ScopeLocatorV2 {
    pub kind: ScopeLocatorKindV2,
    pub value: ScopeLocatorText,
    pub repository_hint: Option<EntityRef>,
}

pub enum ScopeTargetV2 {
    Canonical(EntityRef),
    Locator(ScopeLocatorV2),
}

pub enum ScopeRootV2 {
    CurrentInvocation,
    AllAuthorized { profile_id: ProfileId },
    Profile { profile_id: ProfileId },
    ProjectSet { target: ScopeTargetV2 },
    Collection { target: ScopeTargetV2 },
    Repository { target: ScopeTargetV2 },
    Project { target: ScopeTargetV2 },
    Checkout { target: ScopeTargetV2 },
    Worktree { target: ScopeTargetV2 },
    Ref { target: ScopeTargetV2 },
    Commit { target: ScopeTargetV2 },
    CodeSnapshot { target: ScopeTargetV2 },
    GraphGeneration { generation_id: GraphGenerationId },
    PullRequest { target: ScopeTargetV2 },
    Session { target: ScopeTargetV2 },
    Thread { target: ScopeTargetV2 },
    Turn { target: ScopeTargetV2 },
    Agent { target: ScopeTargetV2 },
    Goal { target: ScopeTargetV2 },
    Workflow { target: ScopeTargetV2 },
    AutomationRun { target: ScopeTargetV2 },
    Initiative { target: ScopeTargetV2 },
    Plan { target: ScopeTargetV2 },
    WorkItem { target: ScopeTargetV2 },
    ExecutionAttempt { target: ScopeTargetV2 },
    Executor { target: ScopeTargetV2 },
    SavedView { target: ScopeTargetV2 },
    GraphNeighborhood { seed: ScopeTargetV2, depth: u8 },
}

pub enum ScopeAmbiguityPolicyV2 { Error, ReturnCandidates }
pub enum ScopeCoveragePolicyV2 { RequireComplete, AllowPartial }
pub enum ScopeStalePolicyV2 { Reject, Report }
pub enum ActivityAttributionModeV2 { AnyEvidence, OccurredDuring, Overlap, PrimaryOnly }
pub enum ScopeTraversalV2 { Exact, Related { max_depth: u8 } }

pub struct ScopeFreshnessPolicyV2 {
    pub max_age_seconds: Option<u64>,
    pub on_stale: ScopeStalePolicyV2,
}

pub struct ScopeLimitsV2 {
    pub max_projects: u16,
    pub max_shards: u16,
    pub max_graph_nodes: u32,
}

pub struct ScopeSelectorV2 {
    pub version: u16,
    pub roots: Vec<ScopeRootV2>,
    pub exclude: Vec<ScopeRootV2>,
    pub time: Option<TimePredicate>,
    pub activity_attribution: ActivityAttributionModeV2,
    pub coverage: ScopeCoveragePolicyV2,
    pub freshness: ScopeFreshnessPolicyV2,
    pub traversal: ScopeTraversalV2,
    pub ambiguity: ScopeAmbiguityPolicyV2,
    pub limits: ScopeLimitsV2,
}

pub struct ScopeResolutionCandidateV2 {
    pub entity: EntityRef,
    pub owning_shard: ShardId,
    pub repository: Option<EntityRef>,
    pub checkout: Option<EntityRef>,
    pub worktree: Option<EntityRef>,
    pub ref_entity: Option<EntityRef>,
    pub snapshot: Option<EntityRef>,
    pub graph_generation: Option<GraphGenerationId>,
    pub evidence: EvidenceClass,
    pub status: ScopeCandidateStatus,
    pub registry_watermark: ShardWatermark,
    pub index_watermark: Option<ShardWatermark>,
}

pub struct ScopeResolutionV2 {
    pub resolution_id: ScopeResolutionId,
    pub selector_digest: ScopeSelectorDigest,
    pub canonical_selector: ScopeSelectorV2,
    pub selected: Vec<ScopeResolutionCandidateV2>,
    pub ambiguous: Vec<ScopeResolutionCandidateV2>,
    pub stale: Vec<ScopeResolutionCandidateV2>,
    pub unavailable: Vec<ScopeResolutionCandidateV2>,
    pub quarantined: Vec<ScopeResolutionCandidateV2>,
    pub missing: Vec<ScopeRootV2>,
    pub defaulted_current: bool,
    pub catalog_generation: ManifestDigest,
    pub watermark: VectorWatermark,
}

pub enum TemporalClauseV1 {
    Current,
    AsOf { valid_time: UtcMicros, knowledge_time: UtcMicros },
    Evolution,
    Forensic,
}

pub struct TraceQueryV1 {
    pub query_id: QueryId,
    pub scope: ScopeSelectorV2,
    pub entity_kinds: Vec<EntityKind>,
    pub message_view: Option<MessageView>,
    pub time: Option<TimePredicate>,
    pub temporal: Option<TemporalClauseV1>,
    pub attributes: Vec<AttributePredicate>,
    pub text: Option<TextPredicate>,
    pub semantic: Option<SemanticPredicate>,
    pub traversal: Option<TraversalPredicate>,
    pub provenance: Option<ProvenancePredicate>,
    pub sensitivity: SensitivityFilter,
    pub facets: Vec<FacetRequest>,
    pub aggregates: Vec<AggregateRequest>,
    pub projection: FieldProjection,
    pub sort: Vec<SortKey>,
    pub page_size: PageSize,
    pub snapshot: SnapshotMode,
    pub explain: ExplainMode,
    pub budget: QueryBudget,
}

pub struct CursorClaimsV1 {
    pub version: u16,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub query_digest: PrivacyDomainBoundLocatorDigest,
    pub access_digest: AccessPolicyDigest,
    pub scope_digest: ScopeSelectorDigest,
    pub catalog_generation: ManifestDigest,
    pub temporal: Option<TemporalClauseV1>,
    pub intent_profile_version: Option<ComponentVersion>,
    pub schema_version: QuerySchemaVersion,
    pub ranking: RankingProfileRef,
    pub index_versions: std::collections::BTreeMap<ShardId, IndexVersionSet>,
    pub snapshot: FrozenSnapshot,
    pub per_shard_positions: std::collections::BTreeMap<ShardId, ShardCursorPosition>,
    pub shard_dispositions: std::collections::BTreeMap<ShardId, ShardDispositionV1>,
    pub sort_cutoff: Vec<SortValue>,
    pub last_entity_id: Option<EntityId>,
    pub emitted_ids_digest: ManifestDigest,
}

pub enum ShardDispositionV1 {
    Searched,
    Skipped,
    Stale,
    Unavailable,
    Incompatible,
    Locked,
    Redacted,
    Truncated,
}

pub struct CoverageReportV1 {
    pub searched: Vec<ShardId>,
    pub skipped: Vec<ShardId>,
    pub stale: Vec<ShardId>,
    pub unavailable: Vec<ShardId>,
    pub incompatible: Vec<ShardId>,
    pub locked: Vec<ShardId>,
    pub redacted: Vec<ShardId>,
    pub truncated: Vec<ShardId>,
    pub freshness: std::collections::BTreeMap<ShardId, ShardWatermark>,
    pub retention_watermark: Option<EvidenceRetentionWatermark>,
    pub unknown_coverage: bool,
}

pub struct CommandEnvelopeV1<C> {
    pub command_id: CommandId,
    pub idempotency_key: NaturalKeyDigest,
    pub scope: ScopeSelectorV2,
    pub expected_version: AggregateVersion,
    pub issued_at: UtcMicros,
    pub payload: C,
}
```

`ScopeSelectorV2` is the only public scope selector across query, commands, policy, catalog, capture, hooks/application, labs, exports, saved views, and coordination. `roots` must be nonempty; `exclude` can only subtract from resolved roots. `CurrentInvocation` and `AllAuthorized` are explicit roots, never meanings assigned to an empty vector. Human locators are typed, sanitized inputs inside the same selector and resolve to a canonical-selector echo in `ScopeResolutionV2`; no transport invents `project_key`, `project_path`, or stringly `all` semantics. Multi-repository/project/checkout/worktree/ref/snapshot/graph-generation selection is first-class. Resolution never falls back to CWD, `sessions.project_key`, the first Claude CWD, active base checkout, current branch graph, ignored-dependency hint scope, or a stale registry row. Ambiguity is an error or returned candidate set according to the selector, never “pick first.” Each selected code candidate is the explicit repository/checkout/worktree/ref/snapshot/generation tuple actually opened; refs may share a generation, and no ref name owns a database. Core resolution reports selected/candidate/missing/stale/unavailable/quarantined stores plus registry/index/ref watermarks and whether current invocation was deliberately defaulted. Application/query responses join separately authorized cross-project session/activity relation evidence without mutating or narrowing the selector.

Query validation rejects page size above 1,000, unbounded traversal, traversal depth above 5, missing total/operator budgets, text/semantic predicates against secret or reasoning content without an explicit authorized filter, and unregistered attributes/predicates. Cursor claims expire after 24 hours by default. Registry/ranking changes, retention crossing the snapshot, or incompatible shard replacement yield a typed restart reason. Commands require idempotency and compare-and-swap aggregate version; a conflict returns current version without applying the command.

`TemporalClauseV1` is the only temporal answer-mode carrier: an absent clause means `Current`, and `AsOf` requires both the valid-time and knowledge-time cutoffs (plan 05 §11.4). Plan 05 plans and executes the clause; plan 23's session/LCM retrieval rides it, and plan 05 specifies the registered-attribute mapping for plan 23's filters — no parallel temporal AST or second answer-mode enum exists. `CursorClaimsV1` binds the resolved scope digest, catalog generation, temporal clause, intent-profile version, and partial-shard dispositions exactly as issued; plans 16/17/21/23 cite these fields rather than restating binding lists. Its digest fields are authoritative types: every adapter (including plan 05's private `CursorV1` encoding) must reuse `PrivacyDomainBoundLocatorDigest`, `AccessPolicyDigest`, and `ManifestDigest` unchanged — an unkeyed `ContentDigest` of query or access material is forbidden by the keyed-digest rule above. `CoverageReportV1` is the one shared coverage vocabulary for query/export/replay responses: plan 05 produces it, plans 07/09/10/13/17/20/22 consume it under this exact name, and `unknown_coverage: true` is mandatory whenever any shard's disposition cannot be proven — coverage never silently reads as complete.

## Cross-crate consumes/produces contracts

| Consumer | Consumes from domain | Produces for other crates |
|---|---|---|
| `tracedecay-store` | IDs, ownership, allocations, observations, events, relations, registries, retention, watermarks, command envelopes | Persisted allocations, append receipts, shard/vector watermarks, migration/import receipts |
| `tracedecay-capture` | source keys/positions, unclassified/classified/sanitized wrappers, receipt and observation contracts | Sanitized observations, receipts, deterministic IDs, and optional protected-quarantine references through a separate port |
| `tracedecay-projectors` | events, relations, registry, watermarks | Registry-valid entity versions and projections |
| `tracedecay-query` | `TraceQueryV1`, cursor claims, scopes, predicates, evidence, watermarks | Signed cursor and query response types owned outside this crate |
| `tracedecay-policy` | immutable input refs, policy entities, evidence/retention | Versioned evaluation events/relations |
| `tracedecay-application` | commands, queries, entity/evidence contracts | Use-case results rendered by adapters |

No consumer may duplicate enum spellings or legal predicate matrices. Rust/OpenAPI/TypeScript schemas derive from the registry digest and fail CI on drift.

## Implementation sequence

### Task 1: Create the pure crate boundary

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/tracedecay-domain/Cargo.toml`
- Create: `crates/tracedecay-domain/src/lib.rs`
- Create: `crates/tracedecay-domain/src/error.rs`
- Create: `crates/tracedecay-domain/tests/schema_contract.rs`

- [ ] **Step 1: Add a failing architecture test**

Add `domain_manifest_has_no_io_or_transport_dependencies`, which reads `crates/tracedecay-domain/Cargo.toml` and rejects `rusqlite`, `libsql`, `tokio`, `axum`, `tracedecay`, dashboard, MCP, and path dependencies.

- [ ] **Step 2: Run the test and verify the missing crate failure**

Run: `cargo test -p tracedecay-domain --test schema_contract domain_manifest_has_no_io_or_transport_dependencies -- --exact`

Expected: FAIL because package `tracedecay-domain` does not exist.

- [ ] **Step 3: Add the workspace member and crate dependencies**

Use workspace dependencies for `serde`, `serde_json`, `schemars`, `uuid`, `sha2`, and `thiserror`; add `proptest` and `jsonschema` as dev dependencies. `lib.rs` publicly declares every module in the proposed tree and re-exports only the public value contracts.

- [ ] **Step 4: Run the architecture test**

Run: `cargo test -p tracedecay-domain --test schema_contract domain_manifest_has_no_io_or_transport_dependencies -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tracedecay-domain
git commit -m "feat(domain): establish v2 contract crate"
```

### Task 2: Implement deterministic IDs and persisted-allocation requests

**Files:**
- Create: `crates/tracedecay-domain/src/id.rs`
- Create: `crates/tracedecay-domain/src/source.rs`
- Create: `crates/tracedecay-domain/src/entity.rs`
- Create: `crates/tracedecay-domain/tests/id_contract.rs`

- [ ] **Step 1: Write failing golden and property tests**

Cover identical source/observation/entity keys across 10,000 randomized inputs, field-boundary collision resistance, order sensitivity, fixed golden UUIDs, `offset < next_offset`, UUIDv7 allocation-request serialization, and rejection of entity kind/owner changes.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test id_contract`

Expected: FAIL with unresolved `derive_source_instance_id`, `derive_observation_id`, `derive_exact_entity_id`, and `AllocationRequest`.

- [ ] **Step 3: Implement the exact contracts and canonical byte encoder**

Add the public types and functions specified above. Fix namespace UUID constants in source, document the byte encoding, derive serde/schema traits, and return typed validation errors rather than panics.

- [ ] **Step 4: Verify determinism**

Run: `cargo test -p tracedecay-domain --test id_contract`

Expected: PASS with stable golden UUIDs on Linux, macOS, and Windows.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/src/id.rs crates/tracedecay-domain/src/source.rs crates/tracedecay-domain/src/entity.rs crates/tracedecay-domain/tests/id_contract.rs
git commit -m "feat(domain): define stable v2 identity"
```

### Task 3: Lock ownership, privacy, time, and retention

**Files:**
- Create: `crates/tracedecay-domain/src/ownership.rs`
- Create: `crates/tracedecay-domain/src/time.rs`
- Create: `crates/tracedecay-domain/src/privacy.rs`
- Create: `crates/tracedecay-domain/src/retention.rs`
- Create: `crates/tracedecay-domain/src/replay.rs`
- Create: `crates/tracedecay-domain/src/protocol.rs`
- Create: `crates/tracedecay-domain/src/payload.rs`
- Create: `crates/tracedecay-domain/tests/ownership_contract.rs`
- Create: `crates/tracedecay-domain/tests/retention_contract.rs`

- [ ] **Step 1: Write failing boundary tests**

Assert activity ownership for canonical messages; project ownership for Git/code; activity ownership for profile-scoped facts/skills/policy/automation; project ownership for project-scoped equivalents; rejection of missing/ambiguous declared scope; catalog rejection of literal strings; blob-domain inequality across privacy/key/retention domains; half-open time behavior; exact-cutoff retention; hold precedence; and the seven content-horizon defaults.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test ownership_contract --test retention_contract`

Expected: FAIL with unresolved ownership and retention types.

- [ ] **Step 3: Implement the ownership and retention matrices**

Implement `ShardKind`, `ShardRef`, `DeclaredScope`, `BlobDomainId`, `CatalogValue`, `UtcMicros`, `TimeInterval`, the Plan 18 `DataSensitivity`/receipt/taint/sink-eligibility types, `RetentionClass`, `RetentionPolicyV1`, `EvidenceRetentionWatermark`, `ReplayMode`, `RuntimeHandshakeV1`, `PayloadRef`, and reasoning format/visibility. Put kind-plus-declared-scope ownership in one exhaustive match so a new kind or scope class causes a compile error.

- [ ] **Step 4: Verify pass and schema serialization**

Run: `cargo test -p tracedecay-domain --test ownership_contract --test retention_contract`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/src/ownership.rs crates/tracedecay-domain/src/time.rs crates/tracedecay-domain/src/privacy.rs crates/tracedecay-domain/src/retention.rs crates/tracedecay-domain/src/replay.rs crates/tracedecay-domain/src/protocol.rs crates/tracedecay-domain/src/payload.rs crates/tracedecay-domain/tests/ownership_contract.rs crates/tracedecay-domain/tests/retention_contract.rs
git commit -m "feat(domain): lock ownership and retention semantics"
```

### Task 4: Define immutable observations, events, and provenance

**Files:**
- Create: `crates/tracedecay-domain/src/provenance.rs`
- Create: `crates/tracedecay-domain/src/observation.rs`
- Create: `crates/tracedecay-domain/src/event.rs`
- Create: `crates/tracedecay-domain/src/message.rs`
- Create: `crates/tracedecay-domain/src/coordination.rs`
- Create: `crates/tracedecay-domain/src/policy/{mod,bundle,evaluation,outcome}.rs`
- Create: `crates/tracedecay-domain/src/hooks/{mod,request,receipt}.rs`
- Create: `crates/tracedecay-domain/tests/observation_contract.rs`
- Create: `crates/tracedecay-domain/tests/message_origin_contract.rs`
- Create: `crates/tracedecay-domain/tests/coordination_contract.rs`
- Create: `crates/tracedecay-domain/tests/policy_hook_contract.rs`
- Create: `crates/tracedecay-domain/tests/fixtures/observation-envelope-v1.json`

- [ ] **Step 1: Write failing validation and round-trip tests**

Cover deterministic ID recomputation, required missing-time reason, nonempty sorted evidence, activity owner for messages, project attribution exclusion from canonical activity rows, opaque unknown provider payload, correction by supersession, forward-version rejection, fixture round-trip digest, PR #410 origin categories, unknown/human-best-effort evidence rules, representative membership without deletion, native/representative count invariants, policy/evaluation/outcome references, hook request/durability/receipt round trips, #411 installation-owner/remediation agreement, #412 lease/drain/checkpoint/service-state distinctions, safe coordination summaries at 160 and 161 Unicode scalar values, rejection rather than truncation of unsafe text, optional-summary round trip, literal-free retrieval anchors, TTL expiry without deletion, every redundancy mode, and multi-worktree/file/symbol/query claim scopes.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test observation_contract --test message_origin_contract --test coordination_contract --test policy_hook_contract`

Expected: FAIL with unresolved observation/event/provenance/policy/hook contracts.

- [ ] **Step 3: Implement immutable contracts and validators**

Implement every field and invariant in the public contracts above. Validation returns a path-aware `DomainError` and never drops opaque payload data.

- [ ] **Step 4: Verify pass**

Run: `cargo test -p tracedecay-domain --test observation_contract --test message_origin_contract --test coordination_contract --test policy_hook_contract`

Expected: PASS and the fixture digest matches `schema-digests.json` once Task 8 writes the consolidated manifest.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/src/provenance.rs crates/tracedecay-domain/src/observation.rs crates/tracedecay-domain/src/event.rs crates/tracedecay-domain/src/message.rs crates/tracedecay-domain/src/coordination.rs crates/tracedecay-domain/src/policy crates/tracedecay-domain/src/hooks crates/tracedecay-domain/tests/observation_contract.rs crates/tracedecay-domain/tests/message_origin_contract.rs crates/tracedecay-domain/tests/coordination_contract.rs crates/tracedecay-domain/tests/policy_hook_contract.rs crates/tracedecay-domain/tests/fixtures/observation-envelope-v1.json
git commit -m "feat(domain): define immutable evidence envelopes"
```

### Task 5: Implement schema and predicate registries

**Files:**
- Create: `crates/tracedecay-domain/src/relation.rs`
- Create: `crates/tracedecay-domain/src/registry.rs`
- Create: `crates/tracedecay-domain/tests/relation_registry_contract.rs`
- Create: `crates/tracedecay-domain/tests/fixtures/relation-assertion-v1.json`

- [ ] **Step 1: Write failing registry tests**

Assert legal endpoint matrices, inverse symmetry, cardinality, minimum evidence, confidence/rationale rules, bitemporal interval validation, activity-to-project attribution predicates, registry digest stability, unregistered attribute rejection, and causal-word prohibition metadata for inferred/heuristic predicates.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test relation_registry_contract`

Expected: FAIL with unresolved registry types.

- [ ] **Step 3: Implement one exhaustive registry**

Define event/entity/predicate/attribute specifications as static typed data, expose the exact lookup and validation methods above, derive inverse indexes from the same declarations, and hash canonical registry JSON for `digest()`.

- [ ] **Step 4: Verify pass**

Run: `cargo test -p tracedecay-domain --test relation_registry_contract`

Expected: PASS with no duplicate IDs, asymmetric inverses, illegal endpoint pairs, or unversioned promoted attributes.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/src/relation.rs crates/tracedecay-domain/src/registry.rs crates/tracedecay-domain/tests/relation_registry_contract.rs crates/tracedecay-domain/tests/fixtures/relation-assertion-v1.json
git commit -m "feat(domain): register typed evidence relations"
```

### Task 6: Define source continuity and vector watermarks

**Files:**
- Create: `crates/tracedecay-domain/src/ordering.rs`
- Create: `crates/tracedecay-domain/src/watermark.rs`
- Create: `crates/tracedecay-domain/tests/ordering_watermark_contract.rs`

- [ ] **Step 1: Write failing concurrency-model tests**

Cover contiguous, duplicate, late, gap, rewrite-conflict, unordered-source, monotonic shard watermark, componentwise vector comparison, incomparable vectors, merge, missing-shard coverage, and stable render-order tie breaking.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test ordering_watermark_contract`

Expected: FAIL with unresolved continuity and watermark types.

- [ ] **Step 3: Implement partial-order contracts**

Implement `SourceOrdering`, `SourceContinuity`, `ShardWatermark`, `VectorWatermark`, `FrozenSnapshot`, `IngressAck`, `AppendReceipt`, and `SpoolReceipt`. Do not implement a scalar cross-shard sequence or `Ord` for `VectorWatermark`; expose `partial_cmp_components`.

- [ ] **Step 4: Verify pass**

Run: `cargo test -p tracedecay-domain --test ordering_watermark_contract`

Expected: PASS and property tests never report a false total order for incomparable vectors.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/src/ordering.rs crates/tracedecay-domain/src/watermark.rs crates/tracedecay-domain/tests/ordering_watermark_contract.rs
git commit -m "feat(domain): model concurrent source progress"
```

### Task 7: Implement bounded query, cursor, and command contracts

**Files:**
- Create: `crates/tracedecay-domain/src/command.rs`
- Create: `crates/tracedecay-domain/src/query/mod.rs`
- Create: `crates/tracedecay-domain/src/query/scope.rs`
- Create: `crates/tracedecay-domain/src/query/predicate.rs`
- Create: `crates/tracedecay-domain/src/query/text.rs`
- Create: `crates/tracedecay-domain/src/query/semantic.rs`
- Create: `crates/tracedecay-domain/src/query/relation.rs`
- Create: `crates/tracedecay-domain/src/query/time.rs`
- Create: `crates/tracedecay-domain/src/query/traversal.rs`
- Create: `crates/tracedecay-domain/src/query/aggregate.rs`
- Create: `crates/tracedecay-domain/src/query/sort.rs`
- Create: `crates/tracedecay-domain/src/query/cursor.rs`
- Create: `crates/tracedecay-domain/tests/query_contract.rs`
- Create: `crates/tracedecay-domain/tests/fixtures/trace-query-v1.json`

- [ ] **Step 1: Write failing query/command tests**

Cover every master-plan scope; multi-repo/project/checkout/worktree/ref/snapshot/graph-generation selectors; explicit `AllAuthorized`; empty-selector rejection; candidate-versus-error ambiguity; exact repository/checkout/worktree/ref/snapshot/generation tuple preservation; no CWD/current-project/first-row fallback; occurred/ingested/valid/as-of time; temporal clause modes with both `AsOf` cutoffs required; registry predicates; lexical/semantic filters; evidence traversal; facets/aggregates; page-size/depth/budget bounds; cursor expiry/invalidation; cursor claims binding scope digest, catalog generation, temporal clause, intent-profile version, and shard dispositions; sensitivity; idempotency; and optimistic conflicts.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test query_contract`

Expected: FAIL with unresolved query and command types.

- [ ] **Step 3: Implement the AST and validators**

Implement the exact `TraceQueryV1`, `CursorClaimsV1`, and `CommandEnvelopeV1` contracts. Keep signing, SQL compilation, ranking, execution, and authorization outside the crate.

- [ ] **Step 4: Verify pass**

Run: `cargo test -p tracedecay-domain --test query_contract`

Expected: PASS with deterministic canonical query digest and fixture round trip.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/src/command.rs crates/tracedecay-domain/src/query crates/tracedecay-domain/tests/query_contract.rs crates/tracedecay-domain/tests/fixtures/trace-query-v1.json
git commit -m "feat(domain): define bounded query contracts"
```

### Task 8: Freeze schemas and compatibility gates

**Files:**
- Modify: `crates/tracedecay-domain/tests/schema_contract.rs`
- Create: `crates/tracedecay-domain/tests/fixtures/schema-digests.json`
- Modify: `tests/fixtures/v2/manifest.json`
- Modify: `tests/v2_corpus_suite/domain_contract.rs`

- [ ] **Step 1: Add failing whole-registry schema tests**

Generate JSON Schema for all public versioned contracts, validate every V2 fixture, assert stable registry/schema digests, reject forward versions, verify exact protocol-handshake mismatch/remediation with no old-name fallback, and verify TypeScript/OpenAPI generation sees identical enum/predicate spellings.

- [ ] **Step 2: Verify failure**

Run: `cargo test -p tracedecay-domain --test schema_contract && cargo test --test v2_corpus_suite domain_contract`

Expected: FAIL until the digest manifest and V2 corpus registration exist.

- [ ] **Step 3: Commit schema digests and V1 import mapping**

Record every schema/registry digest and map V1 provider/session/LCM/graph/fact/automation kinds, PR #405 identity-adoption evidence, PR #407 Hermes session/fact migration receipts, and PR #410 message-origin/representative semantics to V2 kinds. Unknown V1 kinds map to opaque quarantined observations, never a guessed semantic kind.

- [ ] **Step 4: Run the complete crate gate**

Run: `cargo test -p tracedecay-domain && cargo clippy -p tracedecay-domain --all-targets -- -D warnings && cargo doc -p tracedecay-domain --no-deps`

Expected: PASS with no warnings, schema drift, invalid fixtures, or undocumented public items.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-domain/tests tests/fixtures/v2/manifest.json tests/v2_corpus_suite/domain_contract.rs
git commit -m "test(domain): freeze v2 schema contracts"
```

## Compatibility and cutover

- Compatibility in this plan means on-disk evidence/schema import, shadow comparison, and bounded rollback only. It does not promise live compatibility for stale MCP, daemon, plugin, hook, CLI, HTTP, or dashboard clients or retired tool names.
- Runtime boundaries exchange an exact `ProtocolEpoch` plus schema/catalog digests. A mismatch fails closed with `client_restart_required`, `client_update_required`, or the current capability ID/name; it never retries through an old name, guesses a schema, or translates a stale request.
- Hints, generated help, schemas, and catalog snapshots expose current capabilities only. Historical aliases remain import/provenance evidence and replay metadata, never active runtime bindings.
- The root crate converts V1 records at adapter boundaries; V1 modules never depend on `tracedecay-domain` storage implementations.
- PR #405 repository markers, legacy candidate inventories, and retirement receipts are imported before a repository entity is allocated. Healthy/pristine adoption remains evidence; nonempty conflicts remain separate entities and block cutover.
- PR #407 migration markers and source fingerprints are imported idempotently. Hermes host transcript/config roots remain source locators; no Hermes-owned V2 profile or canonical data shard is created.
- V1 strings remain aliases with namespace, validity, resolver version, confidence, and provenance. They are not serialized in `EntityRef`.
- V2 schema versions are accepted only when exact or explicitly listed compatible. A newer observation remains quarantined with its original payload.
- Domain contract changes after PR 4 require a new versioned type/registry entry and compatibility fixture; mutating V1 wire meaning is prohibited.

## Privacy, recovery, and performance obligations

- Privacy: Plan 18 compile-fail and schema tests prove unclassified/classified content cannot serialize into a sink, every `PayloadRef` binds one complete sanitization receipt, catalog-safe types have no arbitrary runtime text, and secret/reasoning defaults plus blob-domain separation are serialized in the registry.
- Recovery: deterministic IDs reproduce exact-source identity; allocation requests make every non-deterministic UUID recoverable only through backed-up allocation ledgers. The domain crate never invents a replacement for a missing ledger entry during restore.
- Performance: `EntityRef`, IDs, timestamps, evidence classes, and watermarks avoid heap allocation; validation is linear in evidence/attribute count; registry lookup is constant time; canonical encoding has a 1 MiB input cap per domain object.
- Concurrency: every state-bearing response includes shard/vector progress; APIs cannot imply globally serial execution.

## Definition of done

- All public contracts and signatures in this plan exist with the same names.
- All IDs have deterministic golden tests or persisted-allocation semantics; no path-derived public identity remains.
- Activity/project/catalog/graph/blob ownership is exhaustive for every entity/event kind.
- Registry validation rejects illegal endpoints, attributes, evidence, confidence, time, sensitivity, and retention combinations.
- Exact retention cutoffs and holds pass property tests.
- Query/cursor/command schemas are bounded, versioned, and round-trip stable.
- `ScopeSelectorV2` preserves every explicit repository/project/checkout/worktree/ref/snapshot/generation and returns typed ambiguity/stale/quarantine coverage; no current-project/CWD/first-match fallback exists.
- The exact Plan 18 `Unclassified -> Classified -> Sanitized -> sink-eligible` contract is generated once from this crate; no consumer owns a second redactor, receipt, taint enum, or public secret marker.
- Presence/work-claim contracts are privacy-safe, TTL-history preserving, redundancy-aware, and advisory-only; unsafe summaries and literal-bearing retrieval anchors cannot be constructed.
- `cargo test -p tracedecay-domain`, clippy, and docs pass.
- Dependency lint proves the crate has no I/O/runtime/transport dependency.
- V1 plus #405/#407/#410/#411/#412 import/semantic mappings have no silent omission; #413 contributes only the actual release/protocol version unless its merged diff changes more.

## Risks and rollback

| Risk | Control | Rollback |
|---|---|---|
| A deterministic-key normalization changes | Fixed canonical encoder, namespace/version byte, golden vectors | Keep the previous derivation version readable; emit a new namespace version and alias relation. |
| UUID allocation ledger is lost | Allocation is durable canonical data and part of every backup manifest | Stop restore with `MissingIdentityLedger`; retain V1/V2 stores read-only rather than minting replacements. |
| Activity is forced into one project | Exhaustive ownership tests and attribution predicates | Disable the projector that wrote invalid ownership, replay from observations with corrected registry. |
| Registry drift breaks consumers | Digest fixtures and generated-client CI | Revert the registry commit; stored old-version rows remain readable. |
| Retention boundary deletes early | Required ingested anchor, strict `< cutoff`, hold tests | Disable retention worker; restore protected blobs during the 24-hour recovery grace and replay projections. |
| A cursor presents false completeness | Vector watermark and coverage are mandatory response fields | Invalidate affected cursors with a typed restart reason. |

Rollback for this crate is code-only until a store migration consumes a new registry version. After persisted use, rollback means restoring the previous crate plus its compatible registry implementation; stored immutable envelopes remain intact and newer rows are quarantined rather than rewritten.
