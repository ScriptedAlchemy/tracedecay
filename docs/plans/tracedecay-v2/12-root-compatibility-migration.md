# TraceDecay V2 Root, Migration, and Cutover

## Status / role

Normative PR19 plan. PR19 performs the complete forward migration from released
V1 data and root wiring, atomically makes V2 authoritative, provides bounded
recovery, and deletes migration-only and superseded V1 paths.

Here V1/V2 name the released product path and the TraceDecay V2 product
cutover, not a blanket requirement to version every branch contract. Migration
inventory includes predecessors proven on `origin/master`, in a published
package/release, an independently deployed client, a live host installation, or
live persisted data. It also fail-closes over potentially installed callable
aliases and potentially persisted branch shapes until an authorized census
proves absence. Pure source-only aliases, internal adapters, and internal DTO
helpers change to their final shape in place; wire-visible DTO revisions follow
the installed-client/host census gate.
Branch-written schemas, stores, spools, files, journals, checkpoints, and
receipts remain in the migration inventory until a separately authorized
registered-store/profile census proves absence; Git history alone cannot remove
their archive, compatibility-reader, or migration obligations.

**Memory cutover correction — SUPERSEDED (2026-08-02).** The Memory V1→V2
cutover drains, the V1 feedback-history repair and backfill pipeline, their
scheduler drains, and the `memory-cutover` CLI verb were all removed as
branch-added migration machinery: fresh V2 stores are created at their final
schema, so there is no V1 memory bank left to drain. What survives is the
archive-merge half in `crates/tracedecay-migrate/src/memory_cutover.rs` (branch
retirement still preserves memory through it) and the fail-closed receipt gate
in `crates/tracedecay-runtime-core/src/db/memory_v2/cutover.rs` that guards the
legacy-payload purge path. `memory_status` no longer reports
`legacy_backfill_complete`. The paragraph below is retained for historical
context only and no longer describes shipping code.

The delivered Memory
V1→V2 branch-store cutover is complete. Its typed owner archive covers all 33
authoritative Memory V2 families; the physical adapters are checked for
one-to-one family parity, archive construction validates referential closure,
imports merge idempotently, and cutover/removal receipts bind the owner,
archive schema, source generation, and archive digest before the old branch
store can be reclaimed. Public reads and consolidation/cutover regressions
exercise the resulting V2 authority. The V1-shaped memory bank remains an
explicitly owned permanent compatibility projection maintained by the
canonical memory transaction; it is not an alternate writer or a superseded
archive. Generic deletion language in this plan does not authorize removing
that compatibility projection. This closure does not complete PR19's broader
released-installation migration, recovery window, or V1 product-path deletion.

**Root lifecycle correction (2026-07-26).** The four former non-test
`TraceDecay::lifecycle` stubs are implemented. Direct project init, project
open, read-only open, and branch open now acquire the exact profile's owned
exclusive maintenance scope and delegate to the registered production
authorities; `configuration_runtime_unavailable` remains only in test-transport
builds.

**Localized integrity-repair correction (2026-07-27).** Bundled-SQLite FTS
blob corruption is now covered by a direct open-and-self-heal regression that
restores search while leaving whole-database corruption fail-closed. This
closes the localized FTS repair regression only; it does not weaken the
maintenance fence or authorize replacement of a generally corrupt database.

**Schema-disposition evidence correction (2026-07-26).** A consolidation
inventory test accepted any non-`None` disposition, so labelling a table
`"merged"` passed without proving corresponding merge SQL existed.
`external_source_states_v1` exposed this false green. A disposition label is
inventory coverage only; it cannot certify migration delivery. Only an
executable source-to-target migration with direct row/identity/semantic
verification satisfies this plan.

Earlier migration fixture names, family inventories, packet layouts, and
intermediate cutover scaffolding are historical evidence, not prerequisites
or artifacts that PR19 must recreate. Published API aliases and persisted data
formats remain compatibility contracts until their declared migration or
retirement completes; acceptance otherwise follows the direct upgrade,
recovery, platform, and regression behavior below.

## Future package-boundary seam

Plan 12 does not prescribe crate-breakup sequencing, source moves, package
counts, worktrees, commits, or delivery gates. Plans 05, 19, 25, and 33 own any
future query, code-index, convergence, or build-performance boundary.

A package boundary is retained only when a direct same-host developer journey
improves and production callers preserve public contracts, generated schemas,
packaging, feature behavior, runtime authority, migration, and normal CI.
Source scans, line/file counts, dependency-shape tables, and moved-module
layouts are diagnostic observations, not acceptance. If no measured product or
developer benefit appears, the current module placement remains valid.

## User outcome

An existing user upgrades without losing supported data or changing the
meaning of queries. After cutover, clients reconnect to one V2 daemon authority
and continue through supported surfaces. A failed or interrupted upgrade either
resumes safely before cutover or rolls forward from verified recovery material
after cutover; it never restores V1 as a writer.

## End-to-end production path

1. The owning daemon acquires a maintenance fence that pauses ingest, sync, and
   other writers for the affected store. Preflight discovers every evidenced
   released/live V1 family and every potentially persisted branch-era family
   not yet eliminated by an authorized census, including schema/version, source
   path, destination scope, required space, and blocking corruption. It returns
   an actionable outcome when complete migration cannot be proved.
2. Before any source mutation, the daemon creates and verifies a recoverable
   backup. The only usable copy is never overwritten.
3. The daemon imports every detected supported family into isolated V2 staging
   in bounded transactions. Durable checkpoints bind the migration and source
   epochs, family and deterministic range, transform revision, exact
   project/user destination identity, counts, and digest. Restart validates those inputs
   and is idempotent or fails closed.
4. Verification compares identities, references, counts, normalized content
   and content hashes where applicable, normalized query results,
   deletion/correction/quarantine lineage, scope mapping, searchability, and
   representative reads. Missing, corrupt, partial, or noisy proof yields
   `insufficient_evidence` or a typed blocking failure, never an optimistic
   cutover.
5. Cutover atomically publishes the verified V2 store and schema epoch. Before
   that point V1 is the only authority; after it, the V2 daemon is the only
   writer and all CLI, MCP, hook, API, LSP, dashboard, and SDK clients
   reconnect through it rather than opening storage.
6. The verified V1 archive remains read-only for one declared recovery window.
   It records source version, checksum, timestamp, migration ID, and restore
   instructions.
   Recovery restores forward into a new verified V2 epoch, reapplies every
   newer disposition, rebuilds affected derivatives, and fences older daemons.
   There is no reverse cutover, dual write, production shadow read, or lazy
   read migration.
7. When the recovery window and deletion policy are satisfied and an authorized
   registered-store/profile census proves no store remains dependent on the V1
   archive or migration-only code, PR19 deletes them and reports what was
   removed.

Before PR16 one local daemon owns the live store. With remote shared Brain,
exactly one fenced daemon authority owns each mutable shard; migration never
creates another writer.

The maintenance fence also covers integrity repair, index rebuild, cutover,
and offline recovery so scheduled sync/ingest cannot race verification. An FTS
or projection repair is admitted only after corruption is localized to
deterministically rebuildable derived state; whole-store or authoritative
family corruption remains preserve-and-escalate.

## PR19 implementation defaults

- Use the existing SQLite `VACUUM INTO`/backup path, migration and
  fault-injection seams, and proptest coverage. These replace a new backup
  copier, migration orchestration framework, and separate crash/property
  harness while retaining maintenance fencing, isolated staging, bounded
  transactions, durable checkpoints, semantic verification, atomic cutover,
  and forward-only recovery.
- If a source store or supported platform cannot create and verify the required
  snapshot through those seams, block cutover and use the existing explicit
  verified-backup fallback. Unknown, partial, or unverifiable backup state
  remains a typed failure; do not add a migration framework or treat
  schema-only equality as success.

## Implementation slices

### Complete staged migration

- Ship preflight, verified backup, maintenance fencing, family-by-family
  staging, durable checkpoints, and restart/resume as one callable daemon
  upgrade path.
- Migrate all detected supported released/live families and every potentially
  persisted branch-era family not eliminated by the authorized census. Unknown
  or corrupt required data blocks the upgrade with Doctor guidance; PR19 has no
  skipped-family or deferred-family success state.
  Pure source-only/internal API/DTO families are finalized in place.
  Wire-visible branch-era families remain until the authorized
  installed-client/host census proves absence; branch-written persisted
  families remain until the registered-store/profile census proves absence.
- Classify corruption by family. Deterministically rebuildable indexes and
  projections may be repaired under exclusive maintenance authority;
  authoritative facts, observations, sessions, and receipts are preserved and
  escalated.

### Verify and cut over atomically

- Run semantic-equivalence checks against real reads and searches while V1 is
  still authoritative and any comparison remains read-only.
- Publish one verified V2 epoch atomically, reconnect clients, and reject an
  older or stale client/daemon with an actionable upgrade error.
- Preserve stable public compatibility names proven by `origin/master` or a
  published package/release, plus branch-era callable names until an authorized
  installed-client/host census proves absence, as thin delegates to canonical
  V2 application operations. They own no storage, policy, lifecycle, or
  migration logic.

### Recover forward, then delete

- Validate daemon replacement during upgrade handoff and recover to the last
  verified V2 state if replacement fails.
- Keep the archive only for the bounded recovery policy, including provenance
  required to preserve erasure, correction, quarantine, and derivative
  ownership.
- Block archive eligibility until deletion, correction, quarantine, and
  derivative ownership are captured. Restoration replays every newer
  disposition before serving and rebuilds affected derivatives; provenance
  never overrides erasure.
- Delete obsolete V1 root wiring, direct database clients, source-only
  temporary adapters, and source-only/internal dead flags after the recovery
  boundary passes. Potentially installed CLI/configuration flags remain until
  the authorized installed-client/host/profile census proves absence.
  Delete migration-only dependencies/features/build inputs and their dedicated
  test support only after the authorized registered-store/profile census proves
  no remaining archive or store depends on them.

## Replacement and deletion

PR19 removes duplicate transport/admin handlers, handler-local database and
query logic, obsolete root-owned product implementation, writable fallbacks,
external `ast-grep` capability probing, and subprocess outline/rewrite paths.
Source-only temporary wrappers are deleted after their named internal consumer
has migrated. Callable branch-era wrappers remain until the authorized
installed-client/host census proves absence. Stable published aliases remain,
but delegate to the same canonical operation and preserve equivalent
authorization, errors, redaction, effects, pagination, streaming, cancellation,
and retry behavior.

The final root package owns composition, daemon lifecycle, discovery, upgrade
handoff, and stable compatibility entry points. It is not a catch-all product
implementation or a permanent migration runtime.

## Direct acceptance

- Real V1 fixtures derived from evidenced released/live families and fixtures
  for every potentially persisted branch-era family not eliminated by the
  authorized census migrate through the production upgrade entry point and
  produce semantically equivalent V2 reads, searches, identities, references,
  and lineage.
- Fault injection at every preflight, backup, staging transaction, checkpoint,
  verification, cutover, archive, and forward-recovery boundary proves
  crash/restart behavior and that no partial authority is published.
- Multi-client tests prove one writer, maintenance fencing, reconnect behavior,
  and refusal by older binaries after the schema-epoch fence advances.
- Upgrade-handoff tests quiesce writes, validate the replacement daemon,
  preserve client reconnection, and recover to the last verified V2 state
  without re-admitting an older daemon as writer.
- Linux and Windows upgrade journeys cover clean success, insufficient space,
  required-family corruption, interrupted migration, failed daemon handoff,
  post-cutover forward recovery, and archive expiry.
- Doctor reports actionable preflight, lock, version, corruption, incomplete
  migration, archive, and recovery states and performs only explicitly selected
  safe repairs.
- Ordinary aggregate repository checks pass after the journey tests; no
  separate acceptance gate is created. PR19 leaves no dual-write/read path,
  lazy migration, reverse cutover, direct writable client, skipped family,
  migration TODO, generated inventory, or migration-only implementation.

## Not in PR19

- New product-domain behavior or a second compatibility implementation.
- Autonomous Git history mutation.
- Indefinite archive retention or V1 writer restoration.
- A migration dashboard, execution ledger, schema-only conformance suite, or
  placeholder acceptance baseline.
