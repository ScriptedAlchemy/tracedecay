# Storage retention, size, and efficiency

Owner-profile storage must stay proportional to live, retrievable value. This
plan is the product contract for database size and efficiency across every
TraceDecay store. It is grounded in measured development evidence from
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

Corrected 2026-07-31: failure class 2 and the matching Delivery entry claimed
code-index generation retention was not implemented. That claim was stale —
retention has been implemented and converging since before this correction. Both
sections are restated below; the residual gap is stranded scope roots and the
byte-budget gates that made the retention findings unreachable, both addressed
by the same change that carries this correction.

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

## Measured failure classes (evidence, one historical owner profile)

**Superseded 2026-07-31.** The 106 GB / 22.2 GiB figures below described the
profile as measured on 2026-07-23 and are retained only as the historical
evidence that motivated this plan. The current census of the same profile
(2026-07-31) is: `branches/` ≈ 43 GB, `sessions.db` ≈ 39 GB, `sessions.db-wal`
≈ 13 GB, `code-index-v1/` ≈ 13 GB, `tracedecay.db` ≈ 7.1 GB, with 17 sealed
generation files spread across 11 scope roots. The live tracedecay scope sits at
generation `…00000082` with 4 superseded generations, 17 retention receipts
dated Jul 27–29, and an empty quarantine — that is generation retention running
and converging, not a store that has never collected.

The historical measurement follows. The profile totalled 106 GB: `projects/` was
101 GB across 464 shards, all `sessions.db` files totalled 35.7 GiB, `branches/`
30.8 GiB, `code-index-v1/` 22.2 GiB, and `global.db` 0.98 GiB. The largest single
file was a 15.7 GiB `sessions.db`. These are reproducible file/directory
measurements, not inferred table sizes.

1. **Live branch stores scale as branches × full graph size.** Branch creation
   clones the ancestor graph database wholesale. In the measured project the
   main database is 161 MiB, the median branch database is 164 MiB, and 103
   live branches occupy 17.2 GiB by construction rather than by neglect.
   ext4 accounting is not inflating that result: every branch database has
   inode link count 1 and apparent size equals allocated size. Branch GC can
   collect stores whose branch is gone; it cannot make live branch stores
   lightweight.
2. **Code-index storage is unreachable above the scope root** (restated
   2026-07-31; the original "generations have no retention" reading is
   withdrawn). The 2026-07-23 observation — 28 immutable generation files,
   22.2 GiB, one active generation, sequence equal to file count — was accurate
   for its date and is no longer the failure. Liveness-based generation
   retention is implemented and converging: mark-and-sweep over
   {active} ∪ vector-readable sources ∪ a newest-superseded rollback floor, an
   exclusive per-scope store lock, a journal → quarantine → durable receipt →
   unlink transaction with crash replay, and four lease-holding call sites
   (daemon maintenance cadence, semantic runtime publication, Doctor kernel,
   storage report).

   The residual failure is one level up. Retention's unit is
   `code-index-v1/<sha256(canonical_project_root)>/`, and every caller derives
   exactly one scope from the project root it was handed, so nothing enumerated
   the siblings. One repository carried three scope directories under a single
   repository discriminator — two stranded by deleted agent worktrees — holding
   7.2 GiB that no retention pass could reach and no report counted. Scope-root
   reconciliation now closes this: it collects a stranded scope through the same
   journal/quarantine/receipt ordering, only under the maintenance writer lease,
   only against a *proven* live-root set (registry plus git's own worktree
   registry per repository; any unreadable source collects nothing and emits a
   named degradation), only past a seven-day minimum stranding age, and never by
   recursively removing anything outside the journaled quarantine path.

   A second, quieter failure sat beside it: the Doctor and storage-report
   entry points guarded this family with byte budgets (64 MiB and 32 MiB) that
   are smaller than a single ~1 GiB generation file, so Doctor answered
   `Unknown` and the report answered `Unavailable` on every profile that had
   anything to report, making `code_generation_retention_finding` structurally
   unreachable. The census cost is directory entries (4–5) and cheap metadata
   reads; only digest verification is expensive. Doctor now gates on entry count
   and censuses from metadata, and the report keeps its digest budget but
   degrades to a metadata-only entry instead of discarding a readable census.
3. **Identity-drift orphan stores — ~41 GB.** A project-root path migration
   re-registered repositories under new project IDs; the old-identity stores
   remained silently, invisible to any surface. Registry GC exists but was
   not automatic and was blocked by a daemon configuration-authority bug.
4. **Unbounded session retention with structural duplication.** The measured
   `sessions.db` population totals 35.7 GiB and includes a 15.7 GiB single
   file. Structurally, `lcm_raw_messages` and `session_messages` can retain the
   same conversations in raw and projected form, with FTS shadows and
   append-only evidence beside them. This plan does not assign per-table
   contributions because no cited telemetry measurement has been reproduced
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
- Code-index generation retention is implemented and engaged (corrected
  2026-07-31; the prior "not implemented; publication advances the pointer
  without collecting" entry was stale). Collection is liveness-based
  mark-and-sweep inside one scope root, transactional through a journal,
  quarantine, and durable receipt, with crash replay on the next pass. It runs
  from the daemon maintenance cadence, semantic-runtime publication, the Doctor
  kernel, and the storage report, each under a lease and each pinning the vector
  inventory before any sweep.
- Code-index *scope-root* reconciliation is implemented and engaged. It is the
  only pass that reaches a scope directory whose canonical project root no
  longer exists. It runs beside generation retention on the maintenance cadence,
  fails closed on any unreadable live-root source, holds the target scope's own
  retention lock, refuses a scope with a pending generation-retention journal,
  and applies a seven-day minimum stranding age.
- Code-index retention observability is reachable. The Doctor storage finding
  reports superseded, collectable, and stranded-scope counts and bytes; stranded
  scopes make the finding non-clean even when the in-scope generation census is
  clean, because a clean scope-local census structurally cannot see them.
- Open gap: reconciliation is bounded by what the live-root enumeration can
  prove. A repository whose worktree registry cannot be read retains every scope
  and reports a named degradation, which is correct but leaves the bytes in
  place until the enumeration succeeds.
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
