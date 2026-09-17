# V2 projection boundary

## Status / Role

Sanitized capture pinned the first production observation-to-view contract. Projection now
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
- Current-diagnostic derivation from sanitized, identity-matched clean
  generation evidence.
- Doctor/operations read models introduced by the dashboard product slice.

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

- Sanitized capture pins one captured observation family and proves its deterministic mapping
  to the existing searchable product row without changing capture truth.
- A projector consumes only sanitized observations and receipt-validated fields;
  it cannot scan or redact content or mint sanitization eligibility.
- Effects and checkpoint commit atomically through the daemon store adapter.
  Failure, cancellation, stale authority, gap, or blocking dead letter leaves
  the checkpoint at the prior committed input.
- Duplicate delivery is a no-op. Late and corrected evidence produces explicit
  provenance or supersession rather than an in-place historical rewrite.
- Diagnostic projection admits only sanitized diagnostics whose repository, snapshot,
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
  parity with the pinned capture contract before exposing the view.
- Canonical transcript bodies remain profile-wide. Project views contain scoped
  rows or locators, never copied message authority.
- Project facts and sessions are project-wide. Code projections require the
  exact repository, checkout, worktree, ref, snapshot, and generation and never
  fall back to an active branch.
- Doctor/operations projections expose real health, lag, corruption,
  recovery, and repair receipts; they do not manufacture findings from source
  code or documentation metadata.

## External-source projection behavior

The canonical projection owner consumes Plan 01 source/native-object/revision,
frontier, and binding semantics and Plan 02's atomic projection persistence. It
does not prescribe a module tree, trait declaration, table family, fixture
filename, or generated inventory.

- Projection is a pure transition from prior view state plus a committed
  sanitized observation or payload-free partition-snapshot completion event.
  Inputs pin exact binding, partition, expected projection frontier, and source
  frontier. Output contains next state, concrete view effects, explicit
  lineage, next frontier, and applied/duplicate-no-op/blocked disposition.
- The daemon store adapter atomically verifies projector/version, exact
  project/profile owner, source partition, expected projection frontier, source
  definition revision, binding revision/digest, and source frontier;
  insert-or-verifies concrete effects; appends lineage; updates current
  pointers or tombstones and partition/aggregate frontiers; and records an
  idempotent receipt. Any failure rolls all of it back.
- Snapshot completion is payload-free evidence emitted only after all staged
  observations for that provider snapshot. Complete coverage may compare the
  staged object set with the prior published set and derive absence tombstones.
  Partial or unknown completion derives none. Duplicate completion, including
  an authoritative empty root, is a no-op.
- Each partition frontier retains projector/version, binding revision/digest,
  partition, source cursor, sanitized observation sequence, content state,
  coverage, continuation, last complete snapshot, and input/output digests.
  The aggregate frontier is a sorted mapping of binding/partition frontiers plus
  its digest; it never collapses incomparable partitions to a scalar maximum or
  treats the digest as an external cursor.
- `Live` means committed sanitized local evidence at a receipt/frontier.
  `AuthoritativeDeleted` requires explicit deletion or declared absence in a
  complete authoritative snapshot and appends a tombstone while retaining
  history. `Partial` commits admitted evidence and an explicit
  gap/continuation without advancing the last-complete snapshot.
  `TemporarilyUnavailable` retains the prior projection and complete frontier
  while recording unavailable coverage.
- Plan 06 access results (`PolicyExcluded` and `Unauthorized`) are composed by
  Plan 09 and are never persisted as source truth or used to advance a
  frontier. Exclusion blocks use/disclosure but does not execute retention.
  Receipt-bearing local retention expiry is separate and never becomes
  provider deletion. Unauthorized, excluded, partial, and unavailable states
  never emit provider tombstones.
- Corrections append a new occurrence and correction/successor lineage.
  Explicit or complete-snapshot-derived deletion appends tombstone lineage.
  Cycles, cross-owner lineage, unknown-predecessor substitution, or conflicting
  content for one native revision block only the affected partition.
  Reappearance after deletion is a new revision and explicit transition, not
  revival of superseded evidence.
- Projectors consume only committed immutable sanitized observations. They
  never fetch, parse, sanitize, authorize, schedule, infer deletion from
  incomplete absence, or mutate the provider. Capture and projection are
  separate atomic local commits; no distributed transaction or exactly-once
  delivery is claimed.

Historical projector/frontier type names and file/test layouts are evidence of
these requirements, not future rebuild obligations. Later audits must locate
the current projection and store owners and direct regressions before declaring
an old artifact missing.

## Final-shape initialization and regression evidence

The clean final store initializes versioned partition and aggregate frontiers,
lineage, and commit receipts from admitted native observations. A bounded
background rebuild may replay immutable final-shape observations into a staged
generation, validate rows, ordering, anchors, lineage, coverage, and digests,
catch up a bounded suffix, and atomically publish the new aggregate frontier.
This rebuild is same-design recovery, not conversion of older TraceDecay
stores. Failed validation leaves the current generation active; the owning
view alone rebuilds, validates, publishes, rolls back, and later retires
generations with idempotent receipts.

Direct regression evidence covers canonical frontier encoding and
partition-order-independent digests; every content-state transition plus
access/content composition and non-deletion; correction/tombstone lineage,
cycles, and cross-owner rejection; duplicate/reorder/permutation convergence;
empty complete versus partial snapshots and duplicate completion; atomic
compare-and-set and every effect/lineage/frontier/receipt failure point; staged
rebuild equality and failed-publication preservation; stale binding,
project/profile non-disclosure, policy-overlay independence, and restart.
Provider acceptance replays exact checked-in Plan 27 bytes and digests through
the real Plan 03 sanitizer; provider-shaped synthetic fixtures are
insufficient.

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

- Capture: a direct contract test maps the real provider observation to the expected
  existing row with stable identity, provenance, scope, and sanitized content.
- Each provider PR proves duplicate and reordered delivery converge on the same
  output and checkpoint.
- Each view PR proves an injected output failure rolls back effects and
  checkpoint together, then succeeds on replay.
- Each view PR using generations proves rebuild equals incremental at a frozen
  frontier and failed validation leaves the prior generation active.
- Diagnostic tests prove dirty overlays create no durable projection,
  mismatched identities cannot enter current views, and rebuild preserves
  clears and supersession without reviving stale findings.
- Scope tests prove user/project ownership and reject base-checkout fallback for
  branch/worktree code graphs.
- Doctor tests prove Doctor diagnosis remains read-only. Any separately entered
  owner operation is projected only from its authoritative durable receipt;
  Doctor never previews, dispatches, or applies it.
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
