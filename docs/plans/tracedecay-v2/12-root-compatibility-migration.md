# TraceDecay V2 Root, Migration, and Cutover

## Status / Role

Normative PR19 plan. This is the bounded final cutover from the V1 root implementation and stores to the V2 daemon and crates. PR19 completes migration, verification, cutover, archive, and obsolete-code/data deletion.

## Outcome

One daemon is the sole database authority. Thin clients and hooks communicate with it through supported APIs. Existing user data is migrated once, verified, cut over safely, archived for the defined recovery window, and then deleted under explicit policy.

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

- The daemon alone opens live project and profile databases for reads and writes; MCP, CLI, hooks, API, and dashboard are clients.
- Hooks send bounded events or signals and return; daemon scheduling, deduplication, sync, retries, and writes are authoritative.
- Refuse concurrent migration for the same store and record a durable migration ID and phase.
- Preflight identifies every supported V1 data family, schema/version, source path, destination scope, required space, and blocking corruption.
- Create and verify a recoverable backup before mutation; never overwrite the only usable copy.
- Import into isolated V2 staging in bounded transactions with deterministic identity mapping and restart-safe checkpoints.
- Migrate all detected supported families in PR19; an unknown or corrupt required family blocks cutover with actionable Doctor output.
- Verify counts, identities, referential integrity, content hashes where applicable, scope mapping, searchability, and representative reads.
- Cut over atomically only after verification. Before cutover, V1 remains authoritative; failed staging is safely discardable or resumable.
- After cutover, clients reconnect to the V2 daemon without opening stores directly.
- Archive the V1 store with version, checksum, timestamp, migration ID, and restore instructions for one defined recovery window.
- Doctor can diagnose preflight, incomplete migration, archive, daemon-version, lock, corruption, and recovery states without unsafe automatic deletion.
- Upgrades quiesce writes, preserve client reconnection, validate the replacement daemon, and recover to the last verified state on failure.
- Delete archives and migration-only code when the recovery policy permits and verification remains valid; report exactly what was removed.
- Do not keep compatibility fallbacks for stale clients. Return a clear upgrade/reconnect error instead.
- Remote/shared-brain support must still route through one authoritative daemon per live store; it never introduces extra database clients.

## Acceptance

- End-to-end fixtures migrate every supported V1 data family and prove representative V2 reads and searches.
- Crash/restart tests cover each migration phase, daemon upgrade, pre-cutover failure, post-cutover recovery, and archive restoration.
- Multi-client tests prove only the daemon accesses live databases and concurrent hooks/clients cannot corrupt them.
- Doctor reports actionable states and performs only explicitly selected safe repairs.
- PR19 leaves no dual-write path, generated inventory, compatibility runtime, obsolete direct DB client, skipped family, or migration TODO.
- Archive deletion follows the documented recovery policy and is tested without risking the sole verified backup.
