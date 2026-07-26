# Storage retention, size, and efficiency

Owner-profile storage must stay proportional to live, retrievable value. This
plan is the product contract for database size and efficiency across every
TraceDecay store. It is grounded in measured dogfood evidence from
2026-07-23, when one owner profile reached **256 GB** and was reduced to
~75 GB purely by removing data the product should never have retained.

## Status

All seven product-contract sections are implemented. Commits `4444833b8` and
`76895d201` additionally wire real storage-budget findings and preserve
unreadable storage roles instead of converting them to clean zeros. This
dashboard/API checkpoint is **implemented but unverified** because the Rust
`dashboard_api_test` suite has not completed successfully; do not replan the
behavior as absent and do not report it as verified.

**Audit correction (2026-07-26; see
[`GAP-LEDGER-PR8-PR14.md`](GAP-LEDGER-PR8-PR14.md) P0-7 and P1-a.)** "All seven
sections are implemented" is true of the mechanisms and misleading about
effect. Three retention engines ship configured to do nothing by default:

- Observation evidence retention defaults `enabled: false` with every release
  window `None` (`src/global_db/observation/retention.rs:174-182`), even though
  the daemon calls it (`src/daemon/git_watch/store_maintenance.rs:216-224`).
- LCM retention runs, but `offload_after_days` and `drop_after_days` default to
  `None`, leaving deduplication at 30 days as its only effect
  (`src/sessions/lcm/retention.rs:128-139`). §4's "one content copy" is
  therefore only partly achieved.
- Session-row retention defaults `session_messages_days: None` and
  `lcm_raw_messages_days: None` (`src/retention.rs:74-82`), deliberately,
  because those rows are the lossless session record. Only `analytics_events`
  prunes by default, at 180 days.

None of the five measured failure classes below is addressed by a default
configuration. Whether lossless-by-default remains correct given those
measurements is an open owner question, not an implementation gap.

## Measured failure classes (evidence, one dogfood profile)

1. **Branch graph-DB copies, never collected — 40 GB in one project.**
   `branches/` holds a full graph shard per branch ever worked on. Branch
   deletion does not trigger DB cleanup, and no periodic sweep runs. Manual
   GC of stale entries freed 24.8 GB across projects.
2. **Identity-drift orphan stores — ~41 GB.** A project-root path migration
   re-registered repositories under new project IDs; the old-identity stores
   remained silently, invisible to any surface. Registry GC exists but was
   not automatic and was blocked by a daemon configuration-authority bug.
3. **Unbounded session retention with structural duplication — 15 GB in one
   `sessions.db`.** `lcm_raw_messages` (3.8 GB) and `session_messages`
   (2.4 GB) both retain the same conversations (raw ingest and projected
   form), plus FTS shadow tables over each (~0.6 GB), plus append-only
   `observations` (1.8 GB), `retrieval_anchors` (1.6 GB), and
   `observation_repository_provenance` (1.4 GB), forever.
4. **Incident debris inside stores.** `*.corrupt-*`, `*.recovered*`, and
   `recovery-*` siblings of live DBs accumulate without any owner surface
   (~0.8 GB in one project).
5. **Free-page bloat.** Large DBs carry unreclaimed free pages; no
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
7. **Size observability and budgets.** Per-store size, per-table growth, and
   free-page ratio are first-class telemetry, cheap to query. Doctor exposes
   a storage finding family (over-budget store, orphan store, stale branch
   DBs, debris present, retention backlog). Owners can set soft budgets;
   exceeding one is a finding, never silent.

## Delivery

- The Doctor storage finding family is implemented through the PR14 Doctor
  slice (Plan 09) over Plan 26 observability read models; route verification
  remains pending as recorded above.
- Branch lifecycle and registry orphan collection are implemented through
  daemon-owned storage runtime work.
- Session retention/offload and disposition-scoped evidence release are
  implemented through the LCM/storage owners.
- Direct tests only: seeded stores with stale branches/orphans/debris must
  produce the findings and the collections; retention windows must be
  provable with ordinary tests. They create no locked gate or PR acceptance
  receipt; required daemon/runtime receipts remain owned by their effects.

## Non-goals

- No lossy deletion of live, referenced evidence: retention acts on
  superseded, orphaned, or projected-and-durable data only.
- No background compaction that competes with foreground writes.
