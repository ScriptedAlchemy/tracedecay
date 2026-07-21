# V2 projection boundary

## Status / Role

PR5 pinned the first production observation-to-view contract. Projection now
participates in each active vertical PR that introduces or replaces a product view. It
is not a standalone framework, registry, or generated-inventory project. See
[the plan index](00-plan-set-index.md) for the owning slices and
[the V2 overview](README.md) for global rules. Production projection paths emit
lag, throughput, resource, and no-op measurements directly to the end-to-end
performance journey.

## Outcome

Immutable sanitized observations deterministically produce existing product
views. Incremental replay and a rebuild at the same committed frontier produce
the same rows, order, provenance, coverage, and checkpoint.

## Owns

- Pure observation-to-view derivation and stable projector versioning.
- Idempotent output keys, provenance links, coverage, and source watermarks.
- Projector checkpoint semantics and dead-letter disposition required by the
  product view introduced in the same PR.
- Rebuild validation and atomic publication when a view uses generations.
- PR9 current-diagnostic derivation from sanitized, identity-matched clean
  generation evidence.
- Doctor/operations read models introduced by the PR14 product slice.

## Does not own

- Provider discovery, parsing, sanitization, source offsets, or hook ingestion.
- Database connections, transactions, writer leases, or publication mechanics;
  the daemon store adapter implements those contracts.
- Query parsing/ranking, policy execution, application commands, transport,
  rendering, repair execution, scheduling, or task/workflow execution.
- Dirty LSP overlay diagnostics or per-client document state; those remain
  ephemeral daemon session state under
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- A complete projector registry, dependency planner, compatibility metamodel,
  speculative view family, or copied canonical transcript store.

## Required behavior

- PR5 pins one captured observation family and proves its deterministic mapping
  to the existing searchable product row without changing capture truth.
- A projector consumes only sanitized observations and receipt-validated fields;
  it cannot scan or redact content or mint sanitization eligibility.
- Effects and checkpoint commit atomically through the daemon store adapter.
  Failure, cancellation, stale authority, gap, or blocking dead letter leaves
  the checkpoint at the prior committed input.
- Duplicate delivery is a no-op. Late and corrected evidence produces explicit
  provenance or supersession rather than an in-place historical rewrite.
- PR9 projects only sanitized diagnostics whose repository, snapshot,
  generation, file identity, and content digest match the clean code view.
  Dirty-overlay diagnostics bypass durable projection and become eligible only
  after saved content enters the normal capture and generation pipeline with
  the same digest.
- Current and rebuilt diagnostic views honor clearing and supersession evidence;
  they never revive or publish stale, historical, or cross-snapshot findings as
  current.
- Incremental and rebuild execution at the same frontier are byte-stable for
  rows whose representation is ordered; generated views publish only after
  validation and keep the prior validated generation on failure.
- Provider expansion PRs add only the mapping needed for that provider and prove
  parity with its PR5 contract before exposing the view.
- Canonical transcript bodies remain profile-wide. Project views contain scoped
  rows or locators, never copied message authority.
- Project facts and sessions are project-wide. Code projections require the
  exact repository, checkout, worktree, ref, snapshot, and generation and never
  fall back to an active branch.
- PR14 Doctor/operations projections expose real health, lag, corruption,
  recovery, and repair receipts; they do not manufacture findings from source
  code or documentation metadata.

## External-source projection contract

The first external-source view adds
`crates/tracedecay-projectors/src/{lib,projector,frontier,external,lineage,error}.rs`,
`crates/tracedecay-projectors/tests/{frontier_contract,source_state,replay_convergence}.rs`,
`crates/tracedecay-store/src/projection/{frontier,commit}.rs`, and
`crates/tracedecay-store/tests/projection_contract.rs`, plus
`src/global_db/observation_projection/{frontier,commit}.rs`. It consumes
`SourceId`, `NativeObjectId`, `SourceRevisionId`, `SourceFrontierSetV1`, and the
owner binding from Plan 01.

```rust
pub trait Projector {
    type State;
    type Effect;

    fn descriptor(&self) -> ProjectorDescriptorV1;
    fn transition(
        &self,
        prior: Option<&Self::State>,
        input: &ProjectionInputV1<'_>,
    ) -> Result<ProjectionTransitionV1<Self::State, Self::Effect>, ProjectionErrorV1>;
}

pub struct ProjectionInputV1<'a> {
    pub event: ProjectionEventV1<'a>,
    pub binding: &'a SourceBindingSnapshotV1,
    pub partition_id: &'a SourcePartitionId,
    pub expected_frontier: &'a PartitionProjectionFrontierV1,
    pub source_frontier: &'a SourceFrontierSetV1,
}

pub enum ProjectionEventV1<'a> {
    Observation(&'a DurableObservationV1),
    PartitionSnapshotCompleted(&'a PartitionSnapshotCompletedV1),
}

pub struct PartitionSnapshotCompletedV1 {
    pub partition_id: SourcePartitionId,
    pub snapshot_id: SourceSnapshotIdV1,
    pub coverage: SourceFrontierCoverageV1,
    pub observed_native_objects_digest: Digest,
    pub observed_count: u64,
}
```

The transition is pure and returns next state, concrete view effects, explicit
lineage assertions, next partition frontier, and a
`Applied | DuplicateNoop | Blocked` disposition. Store integration uses only
`SourceProjectionStore::commit_projection`; that operation atomically verifies
projector/version, exact Project/Profile owner, source partition, expected
frontier digest, source definition revision, binding revision, and binding
digest; insert-or-verifies effects;
appends lineage; updates current pointers or tombstones; updates the partition
frontier; recomputes the sorted aggregate digest; and persists the idempotent
receipt. A failure rolls all of it back.
`PartitionSnapshotCompletedV1` is payload-free finalization evidence after all
staged observations for that snapshot. A complete event may compare the staged
object set with the prior published set and derive absence tombstones; partial
or unknown completion publishes no absence tombstones. Duplicate completion is
a no-op, including for an authoritative empty root.

`PartitionProjectionFrontierV1` records projector/version, binding revision and
digest, partition, source partition cursor, sanitized observation sequence,
`ExternalContentStatusV1`, coverage, continuation digest, last complete
snapshot, input digest, and output digest.
`AggregateProjectionFrontierV1` records the projector ID/version, a sorted map
from `(SourceBindingId, SourcePartitionId)` to partition-frontier digest, and
one aggregate digest. It never collapses incomparable partitions to a scalar
maximum and never treats the aggregate digest as an external cursor.

The projector-owned content-status state machine is:

- `Live`: committed sanitized evidence from a canonical provider read was
  observed; it is authoritative only as local evidence at its receipt/frontier.
- `AuthoritativeDeleted`: only an explicit deletion or declared absence in a
  complete authoritative snapshot; append a tombstone and retain history.
- `Partial`: commit admitted evidence and an explicit gap/continuation, but do
  not advance the last-complete snapshot.
- `TemporarilyUnavailable`: retain prior projection and complete frontier while
  recording unavailable coverage.

`PolicyExcluded` and `Unauthorized` are fresh Plan 06 access results composed
by Plan 09 with the content projection; they are never persisted as source
truth or used to advance a frontier. Policy exclusion blocks use/disclosure by
this projection but does not itself execute retention. Receipt-bearing local
retention expiry is a separate owning path and never produces
`AuthoritativeDeleted`. `Unauthorized`, `PolicyExcluded`, `Partial`, and
`TemporarilyUnavailable` never emit provider tombstones. Corrections append a
new occurrence plus `Correction`/`Successor` lineage; explicit or complete-
snapshot-derived deletion appends `Tombstone`. Cycles, cross-owner lineage,
unknown predecessor substitution, or one native revision with conflicting
content blocks that partition without blocking independent partitions.
Reappearance after deletion is a new revision and explicit lineage transition,
not revival of superseded evidence.

Projectors consume only committed immutable sanitized observations. Those rows
and projections are local evidence of what TraceDecay observed at a receipt and
frontier; the external provider remains authoritative for its current state.
Projectors never fetch, parse, sanitize, authorize, schedule, infer deletion
from incomplete absence, or mutate the provider. Capture and projection use
separate atomic local commits; no distributed transaction or exactly-once
delivery is claimed.

## Migration, fixtures, and TDD

The additive migration creates `projection_partition_frontiers_v1`,
`projection_frontier_heads_v1`, `projection_lineage_v1`, and
`projection_commit_receipts_v1`. It freezes the old scalar checkpoint, replays
immutable observations into a staged generation with real source partitions,
validates rows/ordering/anchors/lineage/coverage/digests, catches up the bounded
suffix, and atomically publishes the new aggregate frontier. Failed validation
leaves the old generation active; old writes stop at cutover and retirement is
performed only by the owning view PR's Plan 09 typed rebuild, validate, publish,
rollback, and later retire commands, each with an idempotent generation
receipt.

Canonical fixtures under `crates/tracedecay-projectors/tests/fixtures/source/`
are `live_then_corrected.jsonl`, `live_then_deleted.jsonl`,
`partial_then_complete.jsonl`,
`temporarily_unavailable_then_live.jsonl`, and
`duplicate_reordered_partitions.jsonl`, each with a golden result. Provider
acceptance additionally replays the exact checked-in Plan 27 bytes and hashes
under `tests/fixtures/source_connectors/<source>/` through the real Plan 03
sanitizer; provider-shaped synthetic fixtures are insufficient. Plan 06/09
integration fixtures separately cover `PolicyExcluded` and `Unauthorized`
overlays without rewriting projector state.

TDD order:

1. Fail canonical frontier encoding and partition-order-independent digest
   tests.
2. Fail the four content-status transitions plus the cross-layer six-result
   composition and non-deletion tests.
3. Fail correction/tombstone lineage, cycle, and cross-owner tests.
4. Fail duplicate/reorder/permutation convergence plus empty-complete,
   empty-partial, and duplicate-snapshot-completion tests.
5. Fail `GlobalDb` CAS, atomic commit, and every effect/lineage/frontier/receipt
   kill point.
6. Fail staged rebuild equality and failed-publication preservation.
7. Fail native-fixture parity, stale-binding CAS, Project/Profile
   non-disclosure, policy-overlay independence, and restart.

Run:

```bash
cargo test -p tracedecay-projectors --test frontier_contract
cargo test -p tracedecay-projectors --test source_state
cargo test -p tracedecay-projectors --test replay_convergence
cargo test -p tracedecay-store --test projection_contract
cargo test --test architecture_boundaries projector
cargo check -p tracedecay-projectors --all-features
cargo clippy -p tracedecay-projectors --all-targets --all-features -- -D warnings
cargo test --all-features
```

Plan [09](09-application-crate.md) orchestrates authorized projection/rebuild
operations, Plan [13](13-research-provenance-and-context-anchors.md) owns
anchors, Plan [16](16-cross-project-repository-worktree-scope.md) owns scope,
Plan [20](20-configuration-control-plane.md) owns desired state, Plan
[23](23-session-lcm-temporal-retrieval-and-evaluation.md) owns temporal query
meaning, and Plan [27](27-cross-host-agent-plugin-bundles.md) owns connector
lifecycle/UI integration. Projection duplicates none of them and creates no
generic or monolithic embeddings table; representation families use immutable
typed generations and their own checkpoints.

## Acceptance

- PR5: a direct contract test maps the real provider observation to the expected
  existing row with stable identity, provenance, scope, and sanitized content.
- Each provider PR proves duplicate and reordered delivery converge on the same
  output and checkpoint.
- Each view PR proves an injected output failure rolls back effects and
  checkpoint together, then succeeds on replay.
- Each view PR using generations proves rebuild equals incremental at a frozen
  frontier and failed validation leaves the prior generation active.
- PR9 diagnostic tests prove dirty overlays create no durable projection,
  mismatched identities cannot enter current views, and rebuild preserves
  clears and supersession without reviving stale findings.
- Scope tests prove user/project ownership and reject base-checkout fallback for
  branch/worktree code graphs.
- PR14 tests prove Doctor diagnosis remains read-only and repair views reflect
  only authoritative, receipt-bearing operations.
- Host-surface parity and restart tests must pass before any superseded V1
  projection path is removed.
- Incremental and rebuild output is byte-identical at the same aggregate
  frontier; every duplicate/reordered partition permutation converges.
- Output, lineage, partition frontier, aggregate digest, and receipt commit
  atomically; exact replay performs no durable write.
- Partial, unavailable, unauthorized, and policy-excluded states never emit
  provider tombstones or masquerade as authoritative deletion or a complete
  empty result; retention remains a separate receipt-bearing path.
- Architecture tests reject provider, scheduler, policy executor, lifecycle,
  UI, transport, database-connection, and monolithic-embedding dependencies.
