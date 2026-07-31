# PR19 Cutover and Runtime Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.

**Goal:** Complete API migration apply, migrate released data atomically to one
V2 writer, retire aliases/V1, and extract runtime adapters after inversion.

## Historical file and interface inventory

- Plan 34 planner/apply and source-edit reconciliation.
- Global/store schema migration, backup/restore, writer admission, cutover
  receipts, aliases/archive policy.
- Runtime extraction described by `05c-adapter-runtime-pr19.md`.

Interfaces: `MigrationPlan`, versioned `MigrationManifest`,
`MigrationCheckpoint`, `BackupVerification`, `CutoverGrant`,
`CutoverReceipt`, `AliasDisposition`, and `ArchiveExpiryReceipt`.

## Historical ordered slices

P0. Inventory every V1/compatibility/runtime path and disposition.
P1. Complete Plan 34 API-migration preview/apply/rollback journey.
P2. Run resumable migration with verified backup and checkpoints.
P3. Atomically fence and promote exactly one V2 writer.
P4. Retire compatibility façades and aliases with public evidence.
P5. Extract dependency-inverted rusqlite/daemon/MCP/LSP adapters.
P6. Expire archives and delete V1 only after retention conditions.

## Product outcome contributed

The work contributed resumable released-data migration, verified backup and
restore, atomic single-writer cutover, compatibility retirement, and
dependency-inverted runtime extraction. Current direct behavior, recovery, and
acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Migration is resumable and roll-forward. Before promotion, rollback restores
the verified backup; after promotion, recovery advances V2 and never re-enables
V1 writes. Measure plan/apply, migration throughput, downtime, restart,
package/runtime edit classes, and backup/restore.

Delete each alias, V1 schema/path, old adapter, and archive only after its
inventory disposition, production caller search, released-data fixture,
rollback/recovery, cross-platform package, SDK/host, and retention evidence
passes.
