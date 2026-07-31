# Storage retention, size, and efficiency

Owner-profile storage must stay proportional to live, retrievable value. This
plan is the product contract for database size and efficiency across every
TraceDecay store. It is grounded in measured dogfood evidence from
2026-07-23, when one owner profile reached **256 GB** and was reduced to
~75 GB purely by removing data the product should never have retained.

## Status

The prior blanket claim that all seven sections are implemented remains
withdrawn (2026-07-26); status is recorded per mechanism. Sections 3 and 4 are
now **engaged**: projection-durable raw LCM payloads default to offload after 30
days and drop after 180 days, projected copies default to dedupe after 30 days,
and the legacy session/raw windows default to 180 days while still requiring
durable summary lineage. Superseded/deleted observation payloads default to a
30-day release window. Superseded `source_cursor_advances` are reclaimed by the
daemon-authorized observation-retention transaction, which preserves the exact
receipt supporting the current frontier and atomically restores the immutable
delete trigger before commit. Direct tests cover dry-run/apply, live-evidence
preservation, current-frontier preservation, authority-loss rollback, and
trigger restoration.

Size evidence published in this plan remains file- and directory-level only.
`SqliteStoreSizeTelemetryPort` in
`crates/tracedecay-rusqlite-runtime/src/telemetry/store_size.rs` implements
`StoreSizeTelemetryPort` over the runtime's read-only SQLite health reader,
including `dbstat` table-payload samples. The daemon Doctor kernel emits those
per-table samples only as tracing; they are not Doctor findings, dashboard
payloads, or output from the separate CLI Doctor implementation. The dashboard
serves per-store size and free ratio plus whole-store history. No plan status
may publish a per-table byte figure until a user-visible product path reproduces
it. In particular, `source_cursor_advances` lives inside the single profile
`global.db`, whose entire measured size is 0.98 GiB; its retention obstacle is
qualitative, not a table-size claim.

Real storage-budget findings preserve unreadable storage roles instead of
converting them to clean zeros. Direct dashboard/API coverage reaches that
behavior, but it does not complete this plan's retention, measured-size,
debris, generation-GC, or full-profile acceptance requirements. Historical
suite names and counts are run evidence, not plan requirements.

## Measured failure classes (evidence, one dogfood profile)

The current measured profile totals 106 GB: `projects/` is 101 GB across 464
shards, all `sessions.db` files total 35.7 GiB, `branches/` totals 30.8 GiB,
`code-index-v1/` totals 22.2 GiB, and `global.db` is 0.98 GiB. The largest
single file is a 15.7 GiB `sessions.db`. These are reproducible file/directory
measurements, not inferred table sizes.

1. **Live branch stores scale as branches × full graph size.** Branch creation
   clones the ancestor graph database wholesale. In the measured project the
   main database is 161 MiB, the median branch database is 164 MiB, and 103
   live branches occupy 17.2 GiB by construction rather than by neglect.
   ext4 accounting is not inflating that result: every branch database has
   inode link count 1 and apparent size equals allocated size. Branch GC can
   collect stores whose branch is gone; it cannot make live branch stores
   lightweight.
2. **Code-index generations have no retention.** `code-index-v1/` contains 28
   immutable generation files totalling 22.2 GiB with exactly one active
   generation. The generation sequence is also 28, matching the file count, so
   no generation has ever been deleted. Code inspection matches the disk
   evidence: publication writes a new immutable generation file and advances
   the active pointer, while removal exists only for temporary files.
3. **Identity-drift orphan stores — ~41 GB.** A project-root path migration
   re-registered repositories under new project IDs; the old-identity stores
   remained silently, invisible to any surface. Registry GC exists but was
   not automatic and was blocked by a daemon configuration-authority bug.
4. **Unbounded session retention with structural duplication.** The measured
   `sessions.db` population totals 35.7 GiB and includes a 15.7 GiB single
   file. Structurally, `lcm_raw_messages` and `session_messages` can retain the
   same conversations in raw and projected form, with FTS shadows and
   append-only evidence beside them. This plan does not assign per-table
   contributions because no cited dogfood measurement has been reproduced
   through the product telemetry path.
5. **Incident debris classifier that existed but never fired on live names.**
   The live profile carried bare `tracedecay.db.corrupt` artifacts totalling
   125 MiB. `IncidentDebrisKindV1::classify` recognized only
   `*.corrupt-*`, `*.recovered*`, and `recovery-*`; all three patterns matched
   zero live profile files. Commit `985cc5d4b` added the bare `.corrupt`
   convention. The collector was reachable before that fix, but its classifier
   could not see the artifacts it was claimed to own.
6. **Free-page bloat.** Large DBs carry unreclaimed free pages; no
   compaction policy exists.

## Product contract

1. **Branch store lifecycle.** Deleting a branch (or a worktree whose branch
   is gone) schedules its branch-DB removal through the daemon. A periodic
   daemon sweep reconciles `branches/` against live git refs. `branch gc`
   remains the manual verb; the automatic path is the default.
2. **Registry orphan detection and collection.** The registry sweep detects
   stores whose project identity no longer resolves to a live repository
   root, reports them as a typed Doctor finding (with age and size), and
   collects them under an owner-visible retention window. Identity
   migrations must re-link or explicitly retire prior-identity stores in the
   same operation — never orphan silently.
3. **Session retention policy.** Raw transcript rows (`lcm_raw_messages`)
   are retained only until their LCM projection/summary lineage is durable,
   then payload-offloaded or dropped per a configurable retention window.
   Projected `session_messages` and their FTS indexes obey the same window.
   Append-only evidence stores (`observations`, anchors, provenance) gain
   generation-scoped retention tied to anchor dispositions — superseded and
   deleted dispositions release their storage.
4. **One content copy.** Raw and projected message content must not be
   duplicated at rest indefinitely. The projection either references raw
   content (content-addressed) or supersedes it after durability; carrying
   both full copies is a defect, not a design.
5. **Incident debris ownership.** Recovery/corruption artifacts are written
   into a single quarantined location with metadata, surfaced by Doctor,
   and collected by the same retention machinery — never left as loose
   sibling files.
6. **Compaction policy.** The daemon schedules incremental vacuum/compaction
   for stores whose free-page ratio crosses a threshold, off the hot path.
7. **Size observability and budgets.** Per-store size and free-page ratio are
   first-class user-visible telemetry, cheap to query. Per-table growth remains
   daemon tracing until it has a typed finding or dashboard/CLI payload. Doctor
   exposes a storage finding family (over-budget store, orphan store, stale
   branch DBs, debris present, retention backlog). Owners can set soft budgets;
   exceeding one is a finding, never silent.

## Delivery

- The Doctor storage finding family is implemented through the PR14 Doctor
  slice (Plan 09) over Plan 26 observability read models; route verification
  remains pending as recorded above.
- Branch lifecycle and registry orphan collection are implemented through
  daemon-owned storage runtime work.
- Session retention is daemon-wired. Projected-message dedupe and raw
  offload/drop have bounded defaults; legacy session/raw pruning additionally
  requires durable summary lineage. Disposition-scoped observation-evidence
  release is active by default with a 30-day recovery horizon.
- Code-index generation retention is not implemented; publication advances the
  active pointer without collecting superseded immutable generation files.
- Superseded `source_cursor_advances` are reclaimed in bounded batches by the
  daemon-authorized retention owner. The exact receipt supporting the current
  source frontier is retained; the delete trigger is suspended and restored
  inside one immediate transaction, with rollback on authority loss.
- `StoreSizeTelemetryPort` is implemented by `SqliteStoreSizeTelemetryPort` at
  `crates/tracedecay-rusqlite-runtime/src/telemetry/store_size.rs`. Its first
  table read establishes scoped watermarks and later reads return `dbstat`
  payload-growth samples. The daemon Doctor kernel emits those samples as
  tracing only; the dashboard exposes per-store size/free ratio and whole-store
  history, while CLI Doctor uses a separate path. This is not a user-visible
  per-table reporting surface or acceptance of historical ad hoc figures.
- Direct tests only: seeded stores with stale branches/orphans/debris must
  produce the findings and the collections; retention windows must be
  provable with ordinary tests. They create no locked gate or PR acceptance
  receipt; required daemon/runtime receipts remain owned by their effects.

## Non-goals

- No lossy deletion of live, referenced evidence: retention acts on
  superseded, orphaned, or projected-and-durable data only.
- No background compaction that competes with foreground writes.
