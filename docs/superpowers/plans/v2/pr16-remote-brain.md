# PR16 Remote Shared Brain Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language cannot resurrect
> branch-only transient scaffolding. Potentially persisted enrollment files,
> spools, replica journals, backups, checkpoints, and receipts keep
> backward-read/replay/recovery until a separately authorized machine/profile
> census proves absence.

**Goal:** Add enrolled, offline-capable, fenced remote Brain operation with one
writer per mutable shard and verified backup/failover.

## Historical file and interface inventory

- Domain/application remote identity and epoch contracts.
- Daemon enrollment, encrypted/offline spool, replay, query coverage, replica,
  backup/restore, promotion/failover, Doctor/API/dashboard surfaces.

Interfaces: `BrainId`, `NodeId`, `ShardId`, `PlacementRevision`, `Epoch`,
`EnrollmentGrant`, `OfflineEnvelope`, `ReplayReceipt`, `ReplicaWatermark`,
`BackupManifest`, and `PromotionReceipt`.

Writer key is exactly
`(BrainId, shard, generation, placement_revision, epoch)`. Overlays remain
node-local and never become shared mutable authority.

## Historical ordered slices

1. Remote contracts and monotone epoch ledger.
2. Enrollment, credential rotation, bounded encrypted offline spool.
3. Fenced duplicate-tolerant replay and cross-node query coverage.
4. Verified replica, backup, staged restore, and integrity receipts.
5. Promotion/failover plus API, Doctor, dashboard, and direct journey.

## Product outcome contributed

The work contributed enrolled, offline-capable remote operation with fenced
single-writer authority, duplicate-tolerant replay, query coverage, and
verified backup/failover behavior. Current direct behavior and acceptance live
in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Enroll remote capability without changing local authority. Restore and verify
before promotion; rollback occurs before promotion and never through
multi-primary fallback. Measure offline append/replay, query latency/coverage,
backup/restore, promotion RTO/RPO, and event-to-ready. Delete ad hoc remote
paths only after fencing, duplicate tolerance, failover, backup/restore,
cross-node query, Doctor/dashboard, and normal CI evidence pass.
