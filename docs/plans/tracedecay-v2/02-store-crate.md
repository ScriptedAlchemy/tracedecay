# V2 store boundary

## Status / Role

PR5 production observation persistence is complete. `tracedecay-store` owns persistence
contracts and DTOs; the daemon-owned `GlobalDb` adapter owns live connections
and transactions. This boundary participates in vertical PRs and does not grow
into a second database implementation. See [the plan index](00-plan-set-index.md)
and [global ownership rules](README.md). Production store paths emit the
database and write-amplification measurements consumed by the end-to-end
performance journey; this plan does not create a separate benchmark milestone.

## Outcome

All TraceDecay clients resolve one authoritative database path per mutable
shard: clients call the owning daemon, and that daemon reads and writes through
its already-open fenced authority. Committed data, receipts, and progress
cannot diverge after crashes or retries.

## Owns

- Store-facing records, batches, errors, and persistence traits.
- The transcript contract landed in PR4, including explicit physical transcript
  identity and separate opaque cursor identity.
- Shipped atomic append contract for sanitized observations, receipts, and offsets.
- Atomic projection-effect and checkpoint contracts added with each consuming
  view slice.
- PR9 canonical clean-generation diagnostic records and snapshots, including
  clearing and supersession evidence.
- Contract-level idempotency, compare-and-set, read-only, and recovery outcomes.

## Does not own

- Opening databases, selecting paths, holding production connections, or
  creating fallback writers; those remain in the daemon `GlobalDb` adapter.
- Parsing, sanitization, projection semantics, query planning, policy, HTTP,
  MCP, CLI, dashboard, hooks, or host workflows.
- A client-side, hook-side, source-adjacent, in-memory, recovery, or remote
  database authority.
- Unsaved LSP overlays, per-client document versions, or an analyzer-local or
  client cache database.
- Delivery metadata, speculative schemas, or a separate database per branch.
  Only code-graph indexes are branch/worktree scoped.

## Required behavior

- PR4 routes CLI, MCP, dashboard, hooks, analytics, LCM, and ingestion through
  the daemon authority; daemon unavailability fails closed.
- PR4 commits a transcript batch and its offset atomically. A failed write leaves
  both unchanged and the same writer remains usable after rollback.
- PR4 full-batch cursor compare-and-set is strict; compatible offset-only advance
  is idempotent and cannot create transcript rows.
- PR4 read-only audit paths do not create a missing database or become writers.
- PR5 commits the sanitized observation, sanitization receipt, and source offset
  in one authoritative transaction; acknowledgement follows commit.
- PR5 duplicate identity plus matching digest is a no-op. A conflicting digest
  fails without advancing progress or overwriting evidence.
- PR9 persists only canonical, sanitized diagnostics bound to a clean code
  generation, with clearing and supersession evidence, through daemon-owned
  store adapters. Unsaved overlays and client document versions remain
  ephemeral daemon session state and never become durable authority; see
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Projection slices commit all effects and their checkpoint together. A failed,
  partial, stale-owner, or dead-letter batch cannot advance the checkpoint.
- Project facts and sessions remain project-wide; user sessions remain
  profile-wide; code graphs retain exact repository/worktree/ref scope.
- Real Doctor, backup, integrity, and recovery operations use the same daemon
  authority and return typed findings/receipts. They never heal by opening an
  alternate writer.

## External-source persistence slice

The first consuming vertical slice adds
`crates/tracedecay-store/src/source/{mod,records,traits}.rs`,
`crates/tracedecay-store/tests/source_contract.rs`,
`src/global_db/source/{mod,schema,operations,migration}.rs`, and
`tests/source_store_contract.rs`. `tracedecay-store` defines DTOs and the
following narrow traits; only the daemon `GlobalDb` adapter implements them in
production:

```rust
pub trait SourceCatalogStore {
    fn load_definition(&self, source_id: &SourceId, revision: u64)
        -> StoreResult<Option<SourceDefinitionV1>>;
    fn publish_definition(
        &mut self,
        expected_prior: Option<(u64, Digest)>,
        value: SourceDefinitionV1,
    ) -> StoreResult<SourceDefinitionCommitReceiptV1>;
    fn load_project_binding(&self, binding_id: &SourceBindingId, revision: u64)
        -> StoreResult<Option<ProjectSourceBindingV1>>;
    fn publish_project_binding(
        &mut self,
        expected_prior: Option<(u64, Digest)>,
        configuration_receipt: ProtectedConfigurationApplyReceiptV1,
        value: ProjectSourceBindingV1,
    ) -> StoreResult<SourceBindingCommitReceiptV1>;
    fn load_profile_binding(&self, binding_id: &SourceBindingId, revision: u64)
        -> StoreResult<Option<ProfileSourceBindingV1>>;
    fn publish_profile_binding(
        &mut self,
        expected_prior: Option<(u64, Digest)>,
        configuration_receipt: ProtectedConfigurationApplyReceiptV1,
        value: ProfileSourceBindingV1,
    ) -> StoreResult<SourceBindingCommitReceiptV1>;
}

pub trait SourceIngestStore {
    fn load_frontier(&self, binding: &SourceBindingId)
        -> StoreResult<SourceFrontierSetV1>;
    fn commit_source_batch(&mut self, batch: SourceCommitBatchV1)
        -> StoreResult<SourceCommitReceiptV1>;
}

pub trait SourceProjectionStore<E> {
    fn commit_projection(&mut self, batch: ProjectionCommitBatchV1<E>)
        -> StoreResult<ProjectionCommitReceiptV1>;
}
```

`SourceCommitBatchV1` carries the pinned definition and binding revisions, an
idempotency key and request digest, the expected aggregate frontier digest,
immutable sanitized observations and sanitization receipts, retrieval anchors,
correction/tombstone lineage, and changed partition frontiers.
`ProjectionCommitBatchV1<E>` carries the projector/version, owner, source
partition, expected projection-frontier digest, concrete view effects, lineage,
and next partition frontier. `E` remains view-specific; no universal projection
effect record or table is introduced.

Plan 20 remains the sole source-binding mutation authority.
`publish_{project,profile}_binding` is the internal publication step of its
protected dry-run/apply transaction, invoked through Plan 09 with the matching
configuration receipt; capture, projection, connector, and host adapters
cannot call it directly. Definition publication is likewise restricted to the
Plan 09 `PublishSourceDefinitionV1` application operation with revision/digest
CAS and an idempotent `SourceDefinitionCommitReceiptV1`.

Definitions and bindings use separate revision histories. The SQLite migration
creates exactly these table families and schema-contract checks:

- `source_definitions_v1` and `source_definition_revisions_v1`, keyed by
  `SourceId` and definition revision/digest, with no owner or credential fields;
- `source_bindings_v1` and `source_binding_revisions_v1`, keyed by
  `SourceBindingId`, exact owner kind, typed owner ID, definition revision,
  privacy domain, and state. Rows contain `owner_kind`, nullable
  `owner_project_id`, and nullable `owner_user_profile_id`; a check requires
  exactly the matching typed ID and rejects the other;
- `source_partition_frontiers_v1` and `source_partition_frontier_heads_v1`,
  keyed by `(SourceBindingId, SourcePartitionId, frontier_version)`;
- `source_aggregate_frontiers_v1` and `source_aggregate_frontier_heads_v1`,
  containing the canonical partition count and aggregate digest;
- `source_occurrences_v1` and `source_lineage_v1`, containing immutable
  observation/anchor references and `successor | correction | tombstone`
  lineage, never provider payload mirrors;
- `source_commit_receipts_v1`, keyed by
  `(SourceBindingId, idempotency_key)` with request and committed-frontier
  digests; and
- view-owned projection frontier/head and commit-receipt tables keyed by
  `(projector_id, projector_version, SourceBindingId, SourcePartitionId)`.

`commit_source_batch` runs one `BEGIN IMMEDIATE` transaction: verify an existing
idempotency receipt; compare-and-set exact `(definition_revision,
definition_digest)`, `(binding_revision, binding_digest)`, and expected
aggregate frontier digest; insert-or-verify observations, receipts, anchors,
occurrences, and lineage; append changed partition frontiers including
coverage; recompute the domain-separated aggregate digest from all sorted
partition heads; write the aggregate head and receipt; commit; then
acknowledge. A matching duplicate is `DuplicateNoop`. Reuse of an identity,
revision, or idempotency key with a different digest is `DigestConflict`.
Stale authority, cancellation, a blocked partition, or any write failure rolls
back content, lineage, every frontier, and the receipt.

`commit_projection` similarly compare-and-sets the prior projection frontier,
the same definition/binding revision-and-digest tuples, and the source aggregate
digest; applies concrete view effects, appends lineage, updates partition and
aggregate frontiers, and persists its receipt in one transaction. This is local atomicity,
not an exactly-once transport claim and not a distributed transaction with an
external provider. Delivery is at least once.

Corrections and tombstones append evidence and lineage; they never overwrite
historical observations. Coalescing requires the same stable native identity
and revision plus the same sanitized digest. Similar text, title, timestamp,
path, or embedding never merges evidence. The external source remains
authoritative for current external state; local immutable sanitized observations
remain authoritative only for what TraceDecay observed. No `embeddings`,
`source_embeddings`, or other monolithic embeddings table is permitted:
representation families own immutable typed generations and checkpoints.

## Migration and TDD

The migration is additive: create tables and invariants; publish definitions;
create or backfill only provable bindings; seed each binding's empty or proven
partition and aggregate frontier; mark ambiguous scope or cursor history
blocked/unknown rather than guessing; record one idempotent migration receipt;
then enable the first source writer. Definitions must land before bindings,
bindings before frontiers, source commit before projection commit, and
projection cutover before old-state retirement.

Plan 27's checked-in native acquisition bytes under
`tests/fixtures/source_connectors/<source>/` are the single fixture authority.
`tests/fixtures/source_connectors/manifest.json` records provider, native
version, fixture path, and SHA-256. The existing `github_review/` files
`event-then-poll.jsonl`, `incremental-overlap.jsonl`,
`whole-root-consistent-scan-{1,2}.jsonl`,
`whole-root-drift-scan-2.jsonl`, `rate-limited.json`, and
`schema-drift.json` remain canonical. The source slice adds native
`explicit-delete.jsonl`, `corrected-version.jsonl`,
`partial-pagination.jsonl`, and `malformed-record.jsonl` in that same
directory and manifest. Store goldens reference those exact paths and hashes
after sanitization rather than copying or inventing provider-shaped JSON.

TDD order:

1. Fail schema-contract tests for every table, key, state check, trigger, and
   the absence of a generic embeddings table.
2. Add definition/binding revision CAS, typed project/Profile isolation, and
   stale Plan-20 publication tests.
3. Add duplicate, conflicting-digest, stale-binding/frontier,
   object-revision/partition-cursor separation, reorder, and retry tests.
4. Add correction/tombstone lineage and unknown-predecessor tests.
5. Inject failures before every row, head, receipt, commit, and acknowledgement.
6. Prove projection effect/frontier rollback and rebuild equality.
7. Run native fixtures through capture, store, and projection adapters.

Run:

```bash
cargo test -p tracedecay-store --test source_contract
cargo test --test source_store_contract
cargo test --test architecture_boundaries store
cargo check --all-features
cargo test --all-features
```

Plans [09](09-application-crate.md), [13](13-research-provenance-and-context-anchors.md),
[16](16-cross-project-repository-worktree-scope.md),
[20](20-configuration-control-plane.md),
[23](23-session-lcm-temporal-retrieval-and-evaluation.md), and
[27](27-cross-host-agent-plugin-bundles.md) respectively own orchestration,
anchors, scope resolution, configuration/secrets, temporal interpretation, and
host lifecycle. Store persists their typed references but duplicates none of
those authorities.

## Acceptance

- PR4: `transcript_batch_survives_restart_and_replay_is_idempotent` passes.
- PR4: `late_cursor_failure_rolls_back_every_transcript_write_then_retries` and
  `invalid_batch_mutates_no_transcript_state` pass.
- PR4: concurrent full and offset-only batch tests prove convergence without
  split brain or partial writes.
- PR4: daemon-only writer, read-only no-create, and post-rollback writer-reuse
  regressions pass.
- PR5: kill-point tests around observation, receipt, offset, commit, and
  acknowledgement prove complete commit or safe retry.
- PR9 diagnostic persistence tests reject dirty overlays, mismatched content
  digests, and client-local authority while preserving explicit clears and
  supersession across restart.
- Each projection PR proves atomic effect/checkpoint rollback and deterministic
  restart before its view becomes queryable.
- Doctor tests prove diagnosis is read-only and every applied repair is
  authority-fenced, idempotent, and receipt-bearing.
- External-source kill-point and restart tests prove observation, receipt,
  lineage, partition frontier, aggregate digest, and projection effects commit
  completely or not at all under at-least-once replay.
- Schema tests prove definition/binding separation, exact Project/Profile owner
  isolation, no alternate writer or raw-source mirror, and no generic or
  monolithic embeddings table.
