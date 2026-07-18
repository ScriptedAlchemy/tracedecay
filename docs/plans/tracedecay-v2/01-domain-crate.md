# V2 domain boundary

## Status / Role

PR5 observation contracts are complete. `tracedecay-domain` is the pure value-and-validation
boundary used by vertical product PRs. It is not a standalone framework roadmap.
Delivery order and global rules live in [the plan index](00-plan-set-index.md)
and [the V2 overview](README.md).

## Outcome

Product slices exchange stable Rust values without leaking database rows,
provider payloads, transport shapes, paths, or runtime handles across ownership
boundaries. A public contract is added only in the same PR as its first product
consumer.

## Owns

- Versioned value types, identifiers, validation, and deterministic encoding.
- Pure research/evidence contracts already landed in PR4.
- Shipped observation, source-position, sanitization-receipt, sensitivity, and
  retention values required by capture and persistence.
- Scope values that distinguish profile-wide user data, project-wide facts and
  sessions, and branch/worktree-scoped code graphs.
- Immutable provenance, coverage, ordering, and watermark values introduced by
  the vertical slice that consumes them.

## Does not own

- Filesystem, database, network, clock, async runtime, locks, queues, or secrets.
- Provider parsing, redaction execution, persistence, projection, querying,
  ranking, policy execution, transport, rendering, or host integration.
- Documentation enforcement, delivery orchestration, source-derived metadata,
  or duplicate transport-local models.
- Speculative schemas, registries, or type families without a shipping consumer.

## Required behavior

- PR4 keeps the crate free of I/O and root-crate dependencies.
- PR5 derives observation identity from stable source evidence, never a row ID,
  absolute path, ambient CWD, or provider display label.
- PR5 permits durable content only after classification and sanitization; every
  durable payload is bound to a receipt covering its digest and disposition.
- PR5 values preserve malformed, partial, duplicate, late, redacted, rejected,
  and unavailable evidence as explicit typed outcomes.
- PR5 source positions and cursors are provider-safe opaque values; numeric and
  content-hash cursors cannot be compared under the wrong ordering rule.
- Each later vertical PR adds the smallest contract it consumes and proves the
  old version remains readable or supplies an explicit migration.
- Provider-exposed reasoning may be represented with visibility and retention;
  hidden reasoning is never inferred or reconstructed.

## Generic external-source contracts

The first external-source vertical slice adds the following files; it does not
land an unused connector framework:

- `crates/tracedecay-domain/src/source/mod.rs`
- `crates/tracedecay-domain/src/source/identity.rs`
- `crates/tracedecay-domain/src/source/definition.rs`
- `crates/tracedecay-domain/src/source/binding.rs`
- `crates/tracedecay-domain/src/source/frontier.rs`
- `crates/tracedecay-domain/src/source/lineage.rs`
- `crates/tracedecay-domain/tests/source_contract.rs`
- `crates/tracedecay-domain/tests/fixtures/source/*.json`

`identity.rs` defines opaque, canonical-encoding newtypes `SourceId`,
`SourceBindingId`, `SourcePartitionId`, `SourcePartitionCursorV1`,
`SourceSnapshotIdV1`, `NativeObjectId`, and `SourceRevisionId`.
`NativeObjectId` is derived inside one immutable `SourceBindingId` and is never
compared across bindings; it is a privacy-domain-bound digest, never a raw URL,
path, repository name, provider key, or credential. `SourceRevisionId`
identifies one external object revision; it is not a partition cursor,
definition, configuration, projection-generation, or policy revision and
deliberately has no total-order implementation.

`definition.rs` defines these exact version-one contracts:

```rust
pub struct SourceDefinitionV1 {
    pub source_id: SourceId,
    pub definition_revision: u64,
    pub definition_digest: Digest,
    pub connector: ConnectorContractV1,
}

pub struct ConnectorContractV1 {
    pub connector_id: SourceConnectorId,
    pub source_connector_contract_digest: Digest,
    pub mode: SourceCaptureModeV1,
    pub strategy: SourceRefetchStrategyV1,
    pub deletion_semantics: SourceDeletionSemanticsV1,
    pub partitioning: SourcePartitioningV1,
}

pub enum SourceCaptureModeV1 { Event, Poll, Hybrid }
pub enum SourceRefetchStrategyV1 {
    WholeRoot,
    IncrementalRevision,
    IncrementalWithWholeRootFallback,
}
pub enum SourceDeletionSemanticsV1 {
    ExplicitOnly,
    CompleteSnapshotAbsence,
}
pub enum SourcePartitioningV1 {
    Single,
    ConnectorDeclared { max_partitions: u32 },
}
```

`ConnectorContractV1` is the canonical capture/storage classification derived
by one validated conversion from Plan 27's richer
`SourceConnectorContractV1`; its digest pins that acquisition contract.
`Event`, `Poll`, and `Hybrid` map respectively from event-only,
poll-only, and event-plus-repair-poll acquisition modes, while the refetch
strategy maps from Plan 27 consistency semantics. It does not redefine Plan
27 envelopes, refresh requests, cursors, scheduling, or host registration.
Validation requires a nonzero definition revision, a digest matching the
canonical bytes excluding the digest field, a nonzero declared partition
limit, `CompleteSnapshotAbsence` only with `WholeRoot`, and
`IncrementalWithWholeRootFallback` only when the pinned Plan 27 contract
supports both incremental polling and whole-root reconciliation. Event-only,
poll-only, and hybrid classifications must match the pinned acquisition modes;
conversion mismatch is a typed domain error, not a best-effort downgrade.

A definition describes provider-neutral behavior and contains no owner,
endpoint, executable, credential, mutable path, scheduler, lifecycle, or UI
state. `binding.rs` separately defines these exact contracts:

```rust
pub enum SourceOwnerV1 {
    Profile(UserProfileId),
    Project(ProjectId),
}

pub struct ProjectSourceBindingV1 {
    pub binding_id: SourceBindingId,
    pub project_id: ProjectId,
    pub source_id: SourceId,
    pub definition_revision: u64,
    pub binding_revision: u64,
    pub binding_digest: Digest,
    pub native_root_id: NativeObjectId,
    pub privacy_domain_id: PrivacyDomainId,
}

pub struct ProfileSourceBindingV1 {
    pub binding_id: SourceBindingId,
    pub user_profile_id: UserProfileId,
    pub source_id: SourceId,
    pub definition_revision: u64,
    pub binding_revision: u64,
    pub binding_digest: Digest,
    pub native_root_id: NativeObjectId,
    pub privacy_domain_id: PrivacyDomainId,
}

pub enum SourceBindingSnapshotV1 {
    Project(ProjectSourceBindingV1),
    Profile(ProfileSourceBindingV1),
}
```

Owner, source, native root, and privacy domain are immutable across binding
revisions. Project and Profile bindings carry exact typed `ProjectId` and
`UserProfileId` authorities. CWD, checkout paths, display labels, collection
membership, and native identifiers never create or widen authority.
`SourceBindingId` is deterministically derived from source, exact typed owner,
privacy domain, and the privacy-bound canonical root locator digest before any
object ID is admitted. `NativeObjectId` is then derived in that binding domain,
so identical provider keys in two projects, profiles, or privacy domains cannot
collapse.

`frontier.rs` defines:

```rust
pub struct SourceFrontierSetV1 {
    pub binding_id: SourceBindingId,
    pub definition_revision: u64,
    pub binding_revision: u64,
    pub binding_digest: Digest,
    pub partitions: BTreeMap<SourcePartitionId, SourcePartitionFrontierV1>,
    pub coverage: SourceFrontierCoverageV1,
    pub aggregate_digest: Digest,
}

pub struct SourcePartitionFrontierV1 {
    pub cursor: Option<SourcePartitionCursorV1>,
    pub snapshot_id: Option<SourceSnapshotIdV1>,
    pub continuation_digest: Option<Digest>,
    pub coverage: SourceFrontierCoverageV1,
}

pub enum SourceFrontierCoverageV1 { Complete, Partial, Unknown }
pub enum ExternalContentStatusV1 {
    Live,
    AuthoritativeDeleted,
    Partial,
    TemporarilyUnavailable,
}
pub enum ExternalAccessStatusV1 { PolicyExcluded, Unauthorized }
pub enum ExternalSourceResultStatusV1 {
    Live,
    AuthoritativeDeleted,
    PolicyExcluded,
    Unauthorized,
    Partial,
    TemporarilyUnavailable,
}
```

The aggregate digest is the domain-separated digest of canonical,
length-prefixed partition IDs and partition-frontier encodings sorted by
partition ID, including each partition's coverage. Aggregate coverage is
derived from all partition states and is `Complete` only when every active
partition is complete. A coverage-only transition changes both partition and
aggregate digests. The digest is snapshot identity, not a scalar cursor or
cross-partition ordering claim. `Partial` and `Unknown` cannot prove deletion
or a clean empty result. `lineage.rs` defines
`SourceLineageKindV1::{Successor, Correction,
Tombstone}`, `SourceObjectRevisionRefV1 { binding_id, source_id,
native_object_id, revision_id }`, and `SourceLineageEdgeV1 { predecessor,
successor, kind }`; edges cannot cross owner, source, binding, privacy domain,
or native object, and replay creates neither a duplicate revision nor a
duplicate edge.

Immutable sanitized observations, receipts, anchors, and projections remain
local evidence of what TraceDecay observed at a committed frontier. They never
replace the external system as authority for its current content. Content state
and access state are separate axes: capture/projection may persist only
`Live`, `AuthoritativeDeleted`, `Partial`, or `TemporarilyUnavailable`;
current policy/application evaluation may return `PolicyExcluded` or
`Unauthorized` without rewriting a source frontier. Only explicit provider
deletion or absence in a complete snapshot with declared absence semantics
yields `AuthoritativeDeleted`; exclusion, access loss, partial coverage, and
temporary failure never do.
Plan 09 deterministically composes the two axes into
`ExternalSourceResultStatusV1`: `PolicyExcluded` and `Unauthorized` take
non-disclosing access precedence; otherwise the exact content status passes
through. The underlying decision retains both axes for audit and replay.

## Delivery, migration, and TDD

Dependency order is fixed: identities and canonical encoding; validated Plan 27
connector conversion and definition validation; exact owner bindings;
partition/aggregate frontiers; lineage; then Plan 02 storage, Plan 03 capture,
and Plan 04 projection consumers. Existing Plan 13 anchors participate in the
first retained-evidence transaction. Plan 09 owns authorized bind/refresh use
cases, Plan 16 owns scope resolution, Plan 20 is the sole source-binding
configuration mutation authority, Plan 23 owns temporal interpretation, and
Plan 27 owns acquisition contracts, scheduling, host packaging, and lifecycle.
Consumers import these canonical source/storage identities while Plan 27's
existing connector envelope and refresh types retain their acquisition role.

The additive migration seeds the first shipped source definition and maps
existing profile/project observations to bindings without changing observation
or anchor identity. It hashes legacy native identity in the existing privacy
domain and writes `Unknown` coverage whenever an exact predecessor frontier
cannot be proven; rerun returns the same migration receipt.

TDD order:

1. Add failing canonical-JSON and unknown-field tests for every V1 value.
2. Add digest tamper, raw-identifier, scope-ambiguity, and invalid-capability
   failures.
3. Add binding stability and project/profile non-collapse tests.
4. Add object-revision/partition-cursor separation, partition-order-independent
   aggregate-digest, binding-revision CAS, and partial-coverage tests.
5. Add correction/tombstone lineage, cycle, and replay tests.
6. Replay checked-in native provider fixtures through the consuming capture
   path; hand-authored lookalike protocol fields are not acceptance evidence.

Run:

```bash
cargo test -p tracedecay-domain --test source_contract
cargo test -p tracedecay-domain --test observation_contract
cargo check -p tracedecay-domain --all-features
cargo test --test architecture_boundaries domain
```

## Acceptance

- PR4: an architecture test proves `tracedecay-domain` has no I/O, database,
  transport, provider, or root dependency.
- PR5: golden tests prove stable observation identity and canonical encoding.
- PR5: negative tests reject unclassified durable payloads, receipt/digest
  mismatch, invalid source position, and scope ambiguity.
- PR5: serde round trips preserve unknown provider evidence without making it an
  indexed or executable field.
- Every PR changing a public value includes its consuming test in that same PR;
  unused public vocabulary fails review.
- Source fixtures prove byte-stable encoding and aggregate digests, definition
  and binding separation, typed `ProjectId` and `UserProfileId` authority, no
  raw native identifier or secret, partial-frontier non-deletion, and acyclic
  correction/tombstone lineage.
- Architecture tests prove these contracts add no I/O, settings, credential,
  lifecycle, transport, UI, or provider dependency.
