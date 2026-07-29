# PR16 Remote Shared Brain Plan

**Goal:** Add enrolled, offline-capable, fenced remote Brain operation with one
writer per mutable shard and verified backup/failover.

## Files and interfaces

- Domain/application remote identity and epoch contracts.
- Daemon enrollment, encrypted/offline spool, replay, query coverage, replica,
  backup/restore, promotion/failover, Doctor/API/dashboard surfaces.

Interfaces: `BrainId`, `NodeId`, `ShardId`, `PlacementRevision`, `Epoch`,
`EnrollmentGrant`, `OfflineEnvelope`, `ReplayReceipt`, `ReplicaWatermark`,
`BackupManifest`, and `PromotionReceipt`.

Writer key is exactly
`(BrainId, shard, generation, placement_revision, epoch)`. Overlays remain
node-local and never become shared mutable authority.

## Ordered slices

1. Remote contracts and monotone epoch ledger.
2. Enrollment, credential rotation, bounded encrypted offline spool.
3. Fenced duplicate-tolerant replay and cross-node query coverage.
4. Verified replica, backup, staged restore, and integrity receipts.
5. Promotion/failover plus API, Doctor, dashboard, and direct journey.

## Tests

Direct: enroll a node, capture offline, reconnect/replay idempotently, query
local/remote coverage, create/verify backup, stage restore, fence the old
writer, promote once, and continue from the exact watermark.

Negative: stolen/expired grant, split brain, stale epoch/placement, duplicate
replay, reordered envelope, spool overflow, corrupt backup, partial restore,
network partition, old-writer recovery, and unavailable replica remain typed.

## Migration, rollback, measurement, deletion

Enroll remote capability without changing local authority. Restore and verify
before promotion; rollback occurs before promotion and never through
multi-primary fallback. Measure offline append/replay, query latency/coverage,
backup/restore, promotion RTO/RPO, and event-to-ready. Delete ad hoc remote
paths only after fencing, duplicate tolerance, failover, backup/restore,
cross-node query, Doctor/dashboard, and normal CI evidence pass.
