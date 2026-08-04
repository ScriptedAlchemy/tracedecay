# Repository/worktree recovery

Recovery is daemon-owned. Do not inspect, copy, rename, edit, attach, vacuum,
or delete SQLite/Grafeo files directly.

## Diagnose

Use status and read-only Doctor to identify:

- exact registered project/worktree identity;
- selected repository snapshot and published graph generation;
- stale, partial, unavailable, corrupt, or reset-required state;
- active writer/refresh/retention operation and its receipt;
- SQLite and Grafeo health, size, checkpoint/compaction, and publication
  watermarks.

Keep the typed state and operation/trace identifiers. A missing or unavailable
authority is not a successful empty result.

## Recover

- Stale/refresh-required: submit an explicit daemon refresh or wait for the
  already admitted operation; reads never trigger it.
- Interrupted publication: the daemon replays its canonical outbox/journal and
  serves no graph past the acknowledged watermark.
- Durability uncertain/corrupt: the handle closes and reads remain unavailable
  until daemon reopen/recovery validates the exact store or rebuilds a derived
  graph from canonical V2 events/content manifests.
- Identity mismatch: correct enrollment/registry identity through the daemon;
  never move a store between project IDs.
- Incompatible store: explicitly recreate the selected V2 profile/project
  store. No legacy reader, migration, backfill, or adoption path exists.
- Deleted worktree/ref: unregister the stale routing identity. Retention may
  later collect unreachable derived generations, but project facts and source
  evidence remain.

Doctor only diagnoses. Every maintenance effect has a distinct application
operation, authorization check, idempotency key, lease/fence, receipt, and
restart semantics.

## Verify

After recovery, verify the exact project/worktree route, graph and relational
watermarks, generation freshness, fact continuity, lossless LCM source
expansion, and an ordinary code/graph query before resuming mutation. A partial
or unavailable verification does not prove recovery.
