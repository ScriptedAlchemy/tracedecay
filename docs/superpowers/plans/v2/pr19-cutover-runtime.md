# PR19 Cutover and Runtime Plan

**Goal:** Complete API migration apply, migrate released data atomically to one
V2 writer, retire aliases/V1, and extract runtime adapters after inversion.

## Files and interfaces

- Plan 34 planner/apply and source-edit reconciliation.
- Global/store schema migration, backup/restore, writer admission, cutover
  receipts, aliases/archive policy.
- Runtime extraction described by `05c-adapter-runtime-pr19.md`.

Interfaces: `MigrationPlan`, versioned `MigrationManifest`,
`MigrationCheckpoint`, `BackupVerification`, `CutoverGrant`,
`CutoverReceipt`, `AliasDisposition`, and `ArchiveExpiryReceipt`.

## Ordered slices

P0. Inventory every V1/compatibility/runtime path and disposition.
P1. Complete Plan 34 API-migration preview/apply/rollback journey.
P2. Run resumable migration with verified backup and checkpoints.
P3. Atomically fence and promote exactly one V2 writer.
P4. Retire compatibility façades and aliases with public evidence.
P5. Extract dependency-inverted rusqlite/daemon/MCP/LSP adapters.
P6. Expire archives and delete V1 only after retention conditions.

## Tests

Direct: plan/apply a real API migration, migrate released fixtures from every
supported version, interrupt/resume at each checkpoint, verify backup/restore,
cut over once, restart all surfaces, exercise SDK/host compatibility, and
expire archives under policy.

Negative: dual writer, reverse cutover, lazy migration, schema-only success,
missing backup, corrupt checkpoint, stale grant, partial alias retirement,
runtime extraction before inversion, and archive deletion without receipt are
hard no-go states.

## Migration, rollback, measurement, deletion

Migration is resumable and roll-forward. Before promotion, rollback restores
the verified backup; after promotion, recovery advances V2 and never re-enables
V1 writes. Measure plan/apply, migration throughput, downtime, restart,
package/runtime edit classes, and backup/restore.

Delete each alias, V1 schema/path, old adapter, and archive only after its
inventory disposition, production caller search, released-data fixture,
rollback/recovery, cross-platform package, SDK/host, and retention evidence
passes.
