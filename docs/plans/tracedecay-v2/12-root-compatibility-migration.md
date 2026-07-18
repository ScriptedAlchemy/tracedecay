# TraceDecay V2 Root, Migration, and Cutover

## Status / Role

Normative PR19 plan. This is the bounded final cutover from the V1 root implementation and stores to the V2 daemon and crates. PR19 completes migration, verification, cutover, archive, and obsolete-code/data deletion.

## Outcome

Before remote delivery, one local daemon is the sole database authority; PR16
preserves exactly one fenced daemon authority per mutable shard. Thin clients
and hooks communicate with the owning authority through supported APIs.
Existing user data is migrated once, verified, cut over safely, archived for
the defined recovery window, and then deleted under explicit policy.

## Owns

- Root composition, process lifecycle, daemon discovery, startup, shutdown, and upgrade handoff.
- V1 store detection and direct family-by-family import into V2 staging.
- Preflight, backup, verification, atomic cutover, recovery, archive, and deletion.
- Doctor diagnostics and safe, explicit healing actions for migration and daemon/storage health.
- Removal of obsolete V1 root wiring, direct database clients, and migration-only code after success.

## Does not own

- Long-lived dual reads, dual writes, shadow execution, or broad compatibility fallbacks.
- Generated compatibility inventories, baseline ledgers, source parsers, route registries, or parity dashboards.
- Product business logic already owned by domain/application/store/query crates.
- Task-plan execution, workflow JavaScript, edit bundles, or developer orchestration.
- Indefinite retention of migrated stores or skipped/deferred migration families.

## Required behavior

- The owning daemon alone opens each live project or profile database for reads
  and writes; MCP, CLI, hooks, API, LSP bridges, and dashboard are clients.
- Hooks send bounded events or signals and return; daemon scheduling, deduplication, sync, retries, and writes are authoritative.
- Refuse concurrent migration for the same store and record a durable migration ID and phase.
- Preflight identifies every supported V1 data family, schema/version, source path, destination scope, required space, and blocking corruption.
- Create and verify a recoverable backup before mutation; never overwrite the only usable copy.
- Import into isolated V2 staging in bounded transactions with deterministic
  identity mapping and durable range/family checkpoints. A checkpoint is
  written only after the destination commit and binds migration/source/checksum
  epoch, family, deterministic order/range, transform and privacy revisions,
  destination identity, counts, and digest. Resume revalidates those inputs and
  is idempotent or fails closed.
- Migrate all detected supported families in PR19; an unknown or corrupt required family blocks cutover with actionable Doctor output.
- Verify counts, identities, referential integrity, content hashes where
  applicable, deletion/correction lineage, scope mapping, quarantine,
  searchability, normalized query results, and representative reads. Any
  shadow comparison is read-only and isolated from production reads and
  effects while V1 remains sole authority.
- Cut over atomically only after verification. Before cutover, V1 remains authoritative; failed staging is safely discardable or resumable.
- After cutover, clients reconnect to the V2 daemon without opening stores directly.
- Archive the V1 store with version, checksum, timestamp, migration ID, and
  restore instructions for one defined recovery window. Archive eligibility is
  blocked until deletion, correction, quarantine, and derivative ownership are
  captured. Restore replays every newer disposition and rebuilds affected
  derivatives before serving; provenance never overrides erasure.
- Doctor can diagnose preflight, incomplete migration, archive, daemon-version, lock, corruption, and recovery states without unsafe automatic deletion.
- Doctor classifies corruption by data family before prescribing recovery.
  Derived, deterministically rebuildable structures — external-content
  full-text shadow tables, projection generations, caches — get an explicit,
  safe, in-place rebuild action under quiesced maintenance authority.
  Authoritative families (facts, observations, sessions, receipts) keep
  preserve-and-escalate. A corrupt derived index must not escalate the whole
  store to offline recovery: PR7 dogfooding hit a malformed external-content
  FTS index whose generic "unrecoverable" prescription concealed a
  one-statement, loss-free rebuild.
- Upgrades quiesce writes, preserve client reconnection, validate the replacement daemon, and recover to the last verified state on failure.
- Upgrade handoff is fenced roll-forward: once a newer binary has migrated a
  shared store, an older daemon must refuse to reopen it as writer (schema
  epoch fence) rather than wedging or contending. Recovery from a failed
  upgrade replaces the binary and rolls forward; it never re-admits the old
  authority over migrated state (a PR7 upgrade wedged exactly this way when an
  old daemon rejected a newer schema it still claimed to own).
- After publication, rollback means forward restoration into a verified V2
  schema epoch under a new fence. The V1 archive is bounded recovery input,
  never renewed writer authority; no reverse cutover, lazy read migration,
  production shadow read, or long-lived dual-write path is admitted.
- Sensitive store operations hold a maintenance fence that pauses scheduled
  sync and ingest for the affected store until the operation commits and
  verifies: integrity repair, index rebuild, migration, cutover, and offline
  recovery never race a sync cycle. PR7 dogfooding demonstrated the failure
  this forbids — a sync cycle ran immediately after an index repair and
  re-amplified latent divergence before the repair could be verified.
- Delete archives and migration-only code when the recovery policy permits and verification remains valid; report exactly what was removed.
- Delete migration-only dependencies, feature edges, build-script inputs, and
  test harnesses with the code they served. The final root package is
  composition and compatibility only, not a catch-all compilation boundary for
  V2 product implementation.
- Do not keep compatibility fallbacks for stale clients. Return a clear upgrade/reconnect error instead.
- Remove external `ast-grep` capability probing and subprocess outline/rewrite,
  duplicate transport/admin handlers, handler-local query/render/database logic,
  and semantic aliases whose compatibility window has closed. Surviving names
  delegate to canonical application operations until their stated removal.
- Remote/shared-brain support must still route through exactly one fenced
  authoritative daemon per mutable shard; it never introduces extra database
  clients.
- Use measured focused-test compilation to right-size integration-test targets.
  Split an oversized shared test binary when a narrow test selection repeatedly
  recompiles unrelated subsystems; do not multiply binaries when the added link
  cost is greater than the measured feedback-time benefit.

## Acceptance

- End-to-end fixtures migrate every supported V1 data family and prove representative V2 reads and searches.
- Crash/restart tests cover each migration phase and checkpoint boundary,
  daemon upgrade, pre-cutover failure, post-cutover forward restoration, and
  archive restoration with newer deletion/quarantine overlays.
- Parity fixtures compare complete families, identities, references,
  normalized digests, query results, quarantines, and representative reads
  without letting shadow execution write or serve production traffic.
- Multi-client tests prove only the daemon accesses live databases and concurrent hooks/clients cannot corrupt them.
- Doctor reports actionable states and performs only explicitly selected safe repairs.
- PR19 leaves no dual-write path, generated inventory, compatibility runtime, obsolete direct DB client, skipped family, or migration TODO.
- Archive deletion follows the documented recovery policy and is tested without risking the sole verified backup.
- Same-host before/after evidence shows the root package's dependency fan-in,
  warm incremental check, and representative focused-test compile scope after
  migration-only code is removed. Any retained high-cost edge has a current
  product owner and measured justification.
