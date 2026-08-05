# V2 store boundary

## Status / Role

PR5 production **session-observation** persistence is complete. External-source
persistence is split (status corrected again 2026-07-26): host observations
reach the daemon-owned `RuntimeExternalSourceStore`, dispatch a
`RepositoryWritePayloadV1::ExternalSource`, and execute `apply_source_commit`
through `ExternalSourceExecutor` before persisting `external_source_states_v1`.
`EXTERNAL_SOURCE_SCHEMA_V1` is installed by the database migration path. The
earlier claim that this reducer had no production caller, adapter, or migration
was wrong for the host-observation specialization. The broader acquisition and
canonical-refetch surface remains without production composition and is still
a retained future seam, not PR8–PR14 work to duplicate.

`tracedecay-store` owns persistence contracts and DTOs; the daemon-owned
`GlobalDb` adapter owns live connections and transactions. This boundary
participates in vertical PRs and does not grow into a second database
implementation. See [the plan index](00-plan-set-index.md) and
[global ownership rules](README.md). Production store paths emit the database
and write-amplification measurements consumed by the end-to-end performance
journey; this plan does not create a separate benchmark milestone.

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

## External-source persistence behavior

The canonical store boundary exposes narrow catalog, ingest, and
view-projection persistence operations; only the daemon-owned `GlobalDb`
adapter implements them in production. Historical trait, DTO, module, table,
test, and fixture names are implementation evidence, not required current
declarations or file layout.

- Catalog reads and publication preserve separate definition and binding
  revision histories. Definition publication uses revision/digest
  compare-and-set and an idempotent receipt through the owning Plan 09
  operation. Plan 20 remains the sole binding-mutation authority: publication
  is the internal step of its protected dry-run/apply transaction and requires
  the matching configuration receipt. Capture, projection, connector, and host
  adapters cannot publish bindings directly.
- A source commit pins definition and binding revisions/digests, an idempotency
  key and request digest, the expected aggregate frontier, immutable sanitized
  observations and receipts, retrieval anchors, correction/tombstone lineage,
  and changed partition frontiers. A projection commit pins projector/version,
  exact owner and partition, expected source/projection frontiers, concrete
  view effects, lineage, and the next frontier. Effects remain view-specific;
  there is no universal projection-effect record.
- Durable representation keeps definition and binding identity/revisions
  separate; enforces exactly one typed project or profile owner on a binding;
  preserves versioned partition and aggregate frontiers, immutable
  occurrence/anchor references and lineage, idempotency receipts, and
  view-owned projection frontiers/receipts; and stores no credentials, raw
  provider payload mirrors, or alternate owner identity.
- One authoritative source transaction verifies an existing idempotency
  receipt; compare-and-sets exact definition, binding, and aggregate-frontier
  state; insert-or-verifies observations, receipts, anchors, occurrences, and
  lineage; appends partition frontiers including coverage; recomputes the
  domain-separated aggregate digest from sorted partition heads; writes the
  aggregate head and receipt; commits; and only then acknowledges.
  A matching duplicate performs no durable work. Reuse of an identity,
  revision, or idempotency key with a different digest is a typed conflict.
  Stale authority, cancellation, a blocked partition, or any write failure
  rolls back content, lineage, every frontier, and the receipt.
- Projection commit similarly compare-and-sets its prior frontier, exact
  definition/binding revision and digest, and source aggregate digest, then
  atomically applies concrete effects, lineage, partition/aggregate frontier,
  and receipt. This is local atomicity under at-least-once delivery, not an
  exactly-once transport claim or distributed transaction with the provider.
- Corrections and tombstones append evidence and lineage; they never overwrite
  history. Coalescing requires the same stable native identity and revision and
  the same sanitized digest. Similar text, title, timestamp, path, or embedding
  never merges evidence. The external source remains authoritative for current
  external state; local immutable observations remain authoritative only for
  what TraceDecay observed.
- Representation families own immutable typed generations and checkpoints. The
  store must not create a generic or monolithic embeddings authority.

An audit of this completed slice must map these behaviors to the current store
ports, daemon adapter, final-shape admission, and direct regressions. A missing
historical name or reorganized schema is not a gap if callable behavior and
regression coverage remain.

## Final-shape admission and regression evidence

TraceDecay creates a clean final-shape store with all required state and
invariants, then publishes definitions and exact bindings before enabling the
writer. Non-final or old TraceDecay store bytes return `ResetRequired`; they
are never read, converted, backfilled, dual-written, or used to seed final
state. External historical host records remain valid acquisition inputs and
are sanitized through the ordinary final-store writer. Definitions precede
bindings, bindings precede frontiers, and source commit precedes projection
commit.

Checked-in native Plan 27 acquisition bytes and recorded origin/version/digest
are the fixture authority. Store expectations reference those same bytes after
sanitization rather than copying or inventing provider-shaped JSON. The retained
cases include event/poll overlap, consistent and drifting whole-root scans,
rate-limit and schema-drift failure, explicit deletion, correction, partial
pagination, and malformed input. Direct regressions must cover
definition/binding compare-and-set, typed project/profile isolation, stale
protected publication, duplicate and conflicting digest behavior, stale
bindings/frontiers, revision/cursor separation, reorder/retry convergence,
correction/tombstone and unknown-predecessor lineage, failure at every durable
boundary, projection effect/frontier rollback, rebuild equality, and the real
capture-to-store-to-projection path.

Plans [09](09-application-crate.md), [13](13-research-provenance-and-context-anchors.md),
[16](16-cross-project-repository-worktree-scope.md),
[20](20-configuration-control-plane.md),
[23](23-session-lcm-temporal-retrieval-and-evaluation.md), and
[27](27-cross-host-agent-plugin-bundles.md) respectively own orchestration,
anchors, scope resolution, configuration/secrets, temporal interpretation, and
host lifecycle. Store persists their typed references but duplicates none of
those authorities.

## Acceptance

- Direct transcript regressions prove restart-safe idempotent replay, complete
  rollback on late cursor or invalid batch failure, concurrent full/offset-only
  convergence, daemon-only writing, read-only no-create, and writer reuse after
  rollback.
- Direct observation kill-point regressions prove complete commit or safe retry
  across observation, receipt, offset, commit, and acknowledgement.
- Each consuming projection proves atomic effect/checkpoint rollback and
  deterministic restart before its view is queryable.
- Diagnostic persistence rejects dirty overlays, mismatched content digests,
  and client-local authority while preserving explicit clears and supersession
  across restart.
- Doctor diagnosis is read-only and exposes no repair, cleanup, GC, retention,
  relink, or recovery operation. Separately entered owning workflows retain
  their own authorization, idempotency, and durable receipts.
- External-source kill-point/restart regressions prove observation, receipt,
  lineage, partition frontier, aggregate digest, and projection effects commit
  completely or not at all under at-least-once replay.
- Storage invariants prove definition/binding separation, exact project/profile
  isolation, no alternate writer or raw-source mirror, and no generic
  embeddings authority.
