# Candidate 1: one store set per profile, partitioned by storage class

Shape in one line: a profile owns six class stores (catalog, journal, memory, evidence segments, views, code
packs). The count of mutable SQLite files is fixed (5 + at most 14 weekly evidence segments) whatever the
number of repositories, clones or worktrees. Project, repository and worktree are key columns, never files.
One bounded executor of N OS threads (default 4) serves every file. Grafeo is dropped. Code generations become
immutable, content-addressed SQLite packs.

Source base: origin/master e5a8ede6ea. Citations point at `ground/explorer-*.md` and code paths they verified.

## Problem

TraceDecay's storage grew by accretion. It has one SQLite file pair per project id (`projects/<id>/tracedecay.db`,
`sessions.db`), three profile files, up to 128 `remote.db`, and four kinds of mutable Grafeo container. Each
SQLite file costs 1 writer and 3 reader OS threads at rest (explorer 1 headline). Every cost, collector and
failure domain is therefore indexed by *directory*, and the directory is indexed by project id. The 2026-10-16
incidents all follow from that:

- Collection is per directory, so a directory nobody owns is never collected. Orphan GC keeps a linked
  worktree's store Live through `git_common_dir` (`orphan_stores.rs:136-144`). Unregistered `proj_*` dirs have
  no collector, and one `memory_*` row pins a whole directory forever.
- Retention runs only on mounted directories, 500 rows a day (explorer 3, "Retention execution reality").
- Each file has its own checkpoint lane, so N files means N lanes that can ratchet (PR #4097: 229 MB WAL,
  245k wakes) or sit idle with a WAL nobody revisits.
- One `sessions.db` mixes the hot append log, its projections, Work/Workflow, handoff grants, git-index and
  native-integration journals. One strictly ordered queue per file let a process-lifetime TEMP-cache fault
  (#4094), misclassified by string matching (`tracedecay-store/src/projection.rs:687`), freeze all ingest for
  6 days.
- Nothing measures size, and nothing re-checks the path after attach. That is how the daemon held 112 GB of
  unlinked inodes (explorer 1, live `/proc`).

Constraints honoured: daemon-only mutable authority (#480); one writer connection per file; one engine,
SQLite via rusqlite (Zack, 2026-10-12); settlement outlives the request (#3711); projection rows and their
checkpoint commit in one local transaction; no backups ever; per-class durability; repository identity, not
git dir; the projection dead-letter path (fixed inputs 1-5).

## Usage (caller's view)

The daemon opens one `Storage` per profile. Nothing else opens a database. The CLI and hooks talk to the
daemon. Offline destructive commands take the profile lock, which `Storage::open` also takes.

```rust
use tracedecay_storage::{Storage, StorageConfig, ResolveMode, Lane};

// Daemon start. Takes daemon-authority.lock, opens or creates P/store/*, starts N executor threads.
let storage = Storage::open(profile_root, StorageConfig::default()).await?;

// Hook or MCP request: cwd -> worktree identity. No files are created per project.
let wt = storage.catalog().resolve(&cwd, ResolveMode::Register).await?;
// wt.repo: RepoId (shared by all clones and worktrees of one repository)
// wt.worktree: WorktreeId (one per checkout root; scopes code heads only)
```

Call site 1, a Work request handler. Before: a sync port on exact SQL, wrapped in `off_native_thread` (#4052).
After: one awaited call that runs as one transaction on an executor thread.

```rust
let outcome = storage.journal().work()
    .request_cancellation(wt.repo, attempt_id, reason, Lane::Foreground)
    .await?;          // no caller thread parks; the closure never round-trips to the caller
```

Call site 2, transcript ingest. Capture appends evidence. The drain projects it into views, one stream at a
time, and a bad head dead-letters only its own stream.

```rust
let receipt = storage.evidence().capture(CaptureBatch { source, frames, expected_cursor }).await?;
let report = storage.views().drain(&session_projector, DrainBudget::per_pass()).await?;
// report.dead_lettered: Vec<DeadLetterRef> (stream, sequence, FaultClass); other streams kept moving
```

Call site 3, a code-graph query from an MCP tool.

```rust
let snap = storage.code().snapshot(wt.worktree).await?;     // pins an immutable pack chain by digest
let callers = snap.callers(symbol_id, Depth(3)).await?;     // read lane; mmap'd read-only SQLite
```

Call site 4, status and doctor.

```rust
let s = storage.status();      // sync, in-memory snapshot; no I/O
for c in &s.classes { println!("{:?} {} / {} bytes, wal {}", c.class, c.bytes, c.budget, c.wal_bytes); }
storage.lifecycle(LifecycleOp::Forget { repo }).await?;   // idempotent, resumable via a catalog tombstone
```

Callers never see a connection, file path, WAL, checkpoint, executor, budget, retention or recovery.

## Shape

### On-disk layout

```
P/                                   (profile root; identity/lock/socket files unchanged)
  store/
    catalog.db        identity (repos, checkouts, worktrees, aliases), config revisions, code heads,
                      pack refcounts, size history, lifecycle tombstones
    journal.db        Work, Workflow, handoff grants, git-index tx, native integration, remote replay spool
    memory.db         facts (memory_v2_*), holographic banks, preferences, decisions
    views.db          sessions, LCM raw + FTS5, temporal tables, summaries, session relations, memory-graph
                      edges, projection cursors, dead letters, response handles
    evidence/2026-W42.db   current weekly segment (writable)
    evidence/2026-W41.db   sealed segments (cold; WAL truncated; deleted whole by retention)
    code/packs/<digest>.pack     immutable SQLite code-graph + index packs (content-addressed)
    code/staging/<nonce>.pack    in-flight builds (swept by age)
    spool/            host-admission spool (bounded framed files, format unchanged)
```

### 1. Engine

SQLite through rusqlite, for every byte. **Grafeo is dropped.** The evidence against keeping it:

- It is a second storage engine, and Zack ruled "I don't want 2 engines tho. I want one only" (2026-10-12).
  He said this about Turso, but the principle is engine count.
- The cheap-open property never shipped at scale. `COMPACT_ROUND_TRIPS_BYTES` stays off because a 431-file
  generation fails its compact digest proof, so real sealed stores serve "in replay form". Their heap is
  O(rows) (`docs/graph-at-rest/README.md` status block). An 8.6 GB staging container cost 20+ GB of heap.
- Dependency-bearing generations must keep their rows in the mutable staging container
  (`sealed_store.rs:17-22`). That container is the Grafeo WAL growth source: `checkpoint_interval` is never
  set, and the WAL is collapsed only at open (#4099).
- It swallows WAL append errors (`location.rs:42-45`). It is a fork the plan forbade. The SQLite comparison
  (plan 39 task 12) was never run, and plan 40 records that the earlier SQLite adjacency "proved the opposite
  trade-offs work".
- Grafeo is confined to one crate (`tracedecay-graph-db`; no other Cargo.toml names it), behind a
  domain-shaped API (`VerifiedGraphSnapshot`, traversal). Replacing the engine under that API is contained.

A code generation becomes an immutable SQLite pack. It holds `node`, `edge(src, kind, dst)` with covering
indexes in both directions, `file`, `symbol_fts` and `text_fts`. It is written once into `code/staging`,
fsynced, digest-proven, renamed, and opened read-only with `immutable=1` and mmap. SQLite B-trees are
byte-addressable at rest, so activation costs O(1) heap, which is what the compact Grafeo form promised.
Refreshes keep today's layering rule from `sealed_layer.rs`: a delta pack over one base, and a refresh that
changes more than 1/8 of files seals cold.

### 2. Unit of storage and sharing

- **One file = one storage class of one profile.** A class store is a database. The evidence class is a
  time-ordered set of segment databases with one writer, so retention deletes a file instead of rows.
- **Thirty worktrees of one repository** map to one `RepoId`. That gives one set of rows in memory, evidence
  and views, and one `WorktreeId` row each in catalog, each with its own code head. Worktrees on the same commit
  share one pack by digest. Worktrees a few commits apart share a base pack plus small deltas.
- **Clones share by repository identity.** `RepoId` = hash of the lexicographically smallest root commit OID
  reachable from HEAD (`git rev-list --max-parents=0`). The existing marker
  `<git_common_dir>/tracedecay-project.json` wins when present, so an operator can split or merge explicitly.
  A repository with no commit gets a provisional path id, upgraded on its first commit. The 14 tracedecay
  clones under `/fast/tmp` collapse to one repo. Their code heads stay per worktree.
- **Profile-wide data** (registry, transcripts, LCM, payloads, telemetry) lives in the same class stores with
  `repo` NULL or set. A session that touches zero, one or many repositories is one row plus a `session_repo`
  link. This was the brain-rewrite plan's reason to keep transcripts profile-wide. It also deletes today's
  double ingest, where profile ingest reads every project transcript to record cursor-only coverage
  (explorer 3).

### 2a. Partitioning

Rule: split where failure domain, write cadence or retention differ. Never split by sharing scope. Scope is a
key column, so a split by scope would multiply files by repositories.

| Data class | File | Cadence | Retention | Scope (key) | Sync | Rebuildable? |
|---|---|---|---|---|---|---|
| Repos, checkouts, worktrees, aliases, config revisions, code heads, pack refs, size history, tombstones | catalog.db | rare, small txn | identity: until forget; worktrees: gone 0 d; size history 90 d | profile, repo, worktree | FULL | Authoritative (worktrees re-derivable by probing) |
| Work, Workflow, handoff grants, git-index tx, native integration, remote replay spool, writer idempotency ledger | journal.db | frequent, small txn | terminal rows 30 d; grants at expiry; ledger 7 d | repo, worktree, node | FULL | Authoritative |
| Facts, holographic banks, prefs, decisions | memory.db | low | never evicted; forget only | profile, repo | FULL | Authoritative |
| Observations, canonical bodies (inline BLOB, replaces `lcm-payloads/`), source cursors, telemetry (replaces `hook_analytics.jsonl`, `analytics_events`, savings) | evidence/<week>.db | hot append | 90 d or budget, whole segment | profile, repo, source | NORMAL | Re-ingestable while the host file exists |
| Sessions, LCM raw + FTS, temporal, summaries, relations, memory-graph edges, cursors, dead letters, response handles | views.db | bursty batches | follows evidence segments; summaries 365 d; handles 24 h | profile, repo, session | NORMAL | Rebuildable from evidence (summaries only while evidence lasts) |
| Code graph, symbols, text index per generation | code/packs/*.pack | once per publication | LRU over non-head packs within budget | content digest | write-once, fsync + rename | Rebuildable from source |

Why each split:

- **catalog and journal are separate.** Catalog is read on every route resolution. Journal holds the
  transactions that today pin a writer for up to 120 s (explorer 1, flow C.4). Separating them means a Work
  burst cannot delay project open: the #4100 grant freeze shared a writer with ingest.
- **journal and evidence are separate.** Small authoritative transactions run at FULL. The hot append log runs
  at NORMAL, and its WAL pressure must never stall grants or git transactions. That stall was the #4097 shard.
- **memory is separate from everything else.** It is the one class that is never evicted and runs at FULL. Its
  file stays small, so a whole-file `quick_check` is cheap, and losing any other class never touches it.
- **evidence and views are separate.** Evidence is authoritative append. Views are rebuildable and carry their
  own cursors, so a views fault is "delete and replay". Today both share a file, so a projection fault looks
  like an evidence fault.
- **evidence is segmented by time.** Retention becomes `unlink` with an O(1) disk return and no
  VACUUM. A runaway week cannot grow the file other weeks live in.
- **code is a separate class of immutable files.** These files have no writer, no WAL and no checkpoint lane.
  They are deleted by unlink, and they are the one place where content addressing pays off across clones.

Merges: there are no per-project files, no `user-sessions.db` vs `sessions.db` split, no `remote.db` per node,
and no `sessions.grafeo`/`user-*.grafeo` (relations become a views edge table). A census of mutable SQLite
files: catalog, journal, memory, views, current evidence, plus at most 14 sealed segments that are cold
(read-only, no writer). Today the count is 3 + 2P (+ remote nodes).

**Threads.** The executor runs N threads (default 4, range 2-8). Connections are objects, not threads: each
mutable file has one writer connection plus at most 4 lazily opened readers, and readers close after 5 min
idle. The packs and sealed-segment LRU caps at 32 open read connections. The thread count is N however many
files, repositories or worktrees exist. Today 30 projects cost about 244.

### 3. Durability and WAL/checkpoint ownership

- **Durability is per class:** `synchronous=FULL` for catalog, journal and memory; `NORMAL` for evidence and
  views. A power-loss-lost evidence commit also loses its cursor, which sits in the same transaction, so the
  source is re-read idempotently. Packs are fsynced before their rename. `auto_vacuum=INCREMENTAL` is set at
  creation, and every file is created by this layer.
- **The executor's per-file writer slot owns checkpointing. No other code calls `wal_checkpoint`.** The mode is
  always `TRUNCATE` with `busy_timeout=0`. PASSIVE and RESTART are never used.
- **The ratchet is impossible by construction.** Every read is a closure that runs on an executor thread with
  a deadline (default 2 s wall, enforced by `sqlite3_interrupt`). A snapshot cannot outlive its closure. The
  executor owns every connection to every file, so the set of WAL readers is exactly the set of running read
  closures. When the WAL passes the soft limit (16 MiB), the slot tries TRUNCATE. If readers are busy, it
  raises a barrier: no new read closure is admitted on that file, in-flight closures finish within their
  deadline, TRUNCATE runs with zero readers and must succeed, and the barrier drops. The worst case is one read
  stall of at most 2 s on one file. There is no sticky drain state and no 100 ms retry loop.
- **Idle WALs are checkpointed.** The 30 s maintenance tick checkpoints any file with WAL > 0 and no write for
  30 s. Today's idle `global.db-wal` held 64.9 MB.
- **Foreign readers cannot happen, but are handled.** If TRUNCATE is busy even under the barrier, the cause is
  another process. The slot records `Finding::ForeignReader{file, pids}` (from `/proc/*/fd`), backs off
  exponentially from 1 s to 60 s, and keeps accepting writes up to a 512 MiB hard WAL. Past that, that file's
  writes return `StorageError::Saturated{retry_after}`. Reads continue. The outcome is bounded and observable,
  and it never spins.

### 4. Execution class

- **One boundary.** `Executor::{read,write}` takes a `Send + 'static` closure over `&ReadTx`/`&mut WriteTx`. It
  returns a future that awaits a oneshot. The closure runs to completion on an executor thread. `ReadTx` and
  `WriteTx` are `!Send` and lifetime-bound, so they cannot escape. Every public storage method is `async` and
  goes through it. **No sync method that touches I/O exists on the public surface**, so no caller can park a
  native worker. This deletes the four execution classes (a)-(d) in the explanation.
- **There are no interactive transactions.** One port call is one closure is one transaction. A multi-step
  Work operation runs its whole state machine inside the closure as pure domain code. Today's 120 s writer pin,
  where `ExactSqlHandle::begin_immediate` waits on caller round trips, cannot exist.
- **Scheduling.** Each file has a writer mailbox, a deficit-round-robin `FairQueue` reused from
  `rusqlite-runtime/admission/queue.rs`, keyed by `(Tenant = RepoId | Profile, Lane)`. Lanes are Health >
  Foreground (weight 4) > Background (1) > Maintenance. At most one thread runs a file's writer at a time.
  Different files' writers and readers run in parallel up to N. Batching keeps today's dwell windows (2 ms
  foreground, 10 ms background), with one `BEGIN IMMEDIATE`, one savepoint per op and one ledger row.
- **Sync-to-async edge.** CPU-heavy domain work (projection transforms, pack building) runs on the existing
  rayon CPU pool between a read closure and a write closure. Storage threads only do SQLite.
- **Deleted:** `ExactSqlHandle`'s sync forms, `dispatch_read`, `off_native_thread` around Work (#4052),
  `recv_writer_reply`, the three std actor threads with `block_on` (git-index, native integration, remote
  replay), the async-shaped sync resolver, and attach-in-a-native-poll. Opens run as executor jobs.

### 5. Corruption containment

Faults are classified by rusqlite extended error code, inside the engine, never by message string.

| Fault | Blast radius | Detection | Recovery (no operator) |
|---|---|---|---|
| Corrupt row or poison input | One projection stream | the projector returns `Contract`, or a read returns `Corrupt` for that row | Retry with backoff (5 s to 5 min) up to K=3 within an incarnation, then a **dead letter** `(stream, seq, FaultClass, detail, attempts)` in views. The stream cursor advances past it and a gap marker records it. Every other stream never paused. Redrive runs once per new daemon incarnation and on a projector version bump. A redrive that fails twice is terminal and appears in doctor |
| Process-lifetime connection or heap fault (#4094) | One connection | any `SQLITE_CORRUPT`/`NOTADB` is first `ConnectionSuspect` | Drop the connection (its TEMP state dies with it), reopen, retry once. Connections also recycle every 6 h. Rule: **no TEMP tables as caches**; caches are Rust structures or ordinary views tables. Only a repeat on a fresh connection plus a failing `PRAGMA quick_check` escalates to file |
| Corrupt file, rebuildable class (views, a pack) | That class, all repos | escalation above, or the pack digest proof on open | Delete the file (no copy kept), create it empty, rebuild by priority: repos with live requests first (views replay from evidence cursors at 0; packs rebuild per worktree on demand). Queries return typed `Rebuilding{repo}` until their repo's streams catch up |
| Corrupt evidence segment | That week, all repos | escalation | Seal it `damaged`, roll a new current segment at once so capture continues, and salvage readable rows to the projector (unreadable rows become dead letters). Re-ingest from host files where cursors allow |
| Corrupt authoritative file (catalog, journal, memory) | That class, all repos | escalation | Class enters read-only `Degraded` (reads of intact pages work) and writes return typed `Unavailable{Degraded}`. Automatic **salvage**: keyset-scan every table into `<class>.salvage`, skip unreadable pages, then atomically rename over. The salvage replaces the file and is not a backup. Catalog worktree rows are also re-derivable by probing roots |
| Corrupt code generation | One digest | proof on open, `sqlite3` read error | Unlink the pack, drop heads naming it, and rebuild once. Every clone and worktree at that digest is healed by that one rebuild |

**Is project-level failure isolation lost?** Yes, for whole-file corruption: a corrupt `views.db` degrades
every repo's session reads until rebuild, where today a corrupt project `sessions.db` hits one repo. It is
recovered as follows:

- (a) The evidence says file corruption is the rare fault and row, stream or connection faults are the common
  one: #480 had multiple writers (prevented by daemon ownership), #3415 was a bad head, and #4094 was
  connection TEMP state. Stream dead letters, per-tenant fair queues and connection recycling give **finer**
  isolation than one-file-per-project did. Today one bad head froze a whole file, and every provider and
  session in it.
- (b) Each class is its own file, so a fault in one class never touches another. Memory, the precious class,
  is the smallest and slowest-written file.
- (c) Rebuildable classes rebuild per repo by priority, so the repo in use recovers first.
- (d) Contention isolation per repo is explicit in the fair queue, which today's per-file writer did not
  give across files.

Today's isolation is already partial: `global.db` and `user-sessions.db` span every project.

### 6. Retention and bounded growth

**One owner.** The `Storage` maintenance lane, a Maintenance-lane job every 30 s, owns every budget. It stats
the ~20 fixed paths and reads `page_count`/`freelist_count`. This is cheap because the file count is fixed.
Each sample appends a size-history row to catalog, so growth is measured at any size and persists across
restarts. `storage.status()` exposes bytes, WAL, budget and state per class.

| Class | Default budget | At budget |
|---|---|---|
| evidence | 16 GiB, 90 d | drop the oldest sealed segment (unlink), then delete views rows keyed by that segment in pages, looping until done |
| views | 8 GiB | drop views of the oldest live segment first (raw rows, temporal), keeping summaries |
| code | 16 GiB | LRU-evict non-head packs, then heads of worktrees idle longest; rebuild on next query |
| journal | 1 GiB | purge terminal rows past TTL early; then `Finding::OverBudget` |
| catalog | 256 MiB | finding only |
| memory | none (finding at 2 GiB) | never evicted (fixed input 2) |

The #4099 growth sources, one by one:

1. **Per-worktree stores never collected.** There are no per-project or per-worktree directories, so
   orphaned `proj_*` dirs and an unregistered-dir collector cannot exist. Worktree liveness is probed per
   **worktree root** (root exists and its `.git` names the recorded git dir), never through `git_common_dir`.
   A gone worktree's code head drops on the next tick, and its packs fall to refcount zero and LRU. Memory rows
   pin nothing outside memory.db. A repo with no live worktree keeps its memory forever and loses everything
   else through ordinary time retention.
2. **The hard-pressure drain retries forever.** Replaced by the TRUNCATE barrier (section 3). It needs no
   reader revocation because no reader can outlive 2 s. A foreign reader escalates with backoff to typed
   `Saturated` at 512 MiB.
3. **Grafeo WALs without checkpoint.** No mutable Grafeo container remains, so there is no Grafeo WAL.
4. **LCM payloads.** Bodies are inline BLOBs in their evidence segment, content-addressed within the segment,
   and die with it. No file is written before a transaction, so no orphan can form. Views store
   `body_hash` and FTS text only, which removes today's four-copies-per-line duplication.
5. **Observation projection.** Projection outputs, dispositions and skeleton rows carry `seg`. They leave
   when their segment does. There is no `projection_queue` table: the queue is `evidence.seq > cursor` per
   stream.
6. **Generation staging.** `code/staging/*` older than 1 h without a live in-process build lease is swept
   every tick, not only at open. No staging container exists.
7. **hook_analytics.jsonl and analytics_events.** These become rows in the evidence telemetry table, with
   segment retention.
8. **response-handles.** These become a views table with `expires_at`, swept every tick.
9. **maintenance inventory logs.** Deleted; nothing remains to inventory. host-components keeps the last 2
   bundles.
10. **Writer idempotency ledger.** Rows are pruned at 7 d of age, a horizon longer than any client retry,
    instead of today's per-incarnation scope that strands every prior incarnation's rows.

### 7. Schema evolution

Each file stamps `application_id` (class) and `user_version`.

- **Rebuildable classes** (views, packs): on a version mismatch, delete and rebuild. Views replay from
  evidence. Packs carry their extractor version in the digest, so a new version is simply a new digest.
- **Evidence:** a new version applies to new segments. Old segments are read through a versioned decoder
  until they age out (at most 90 d). A decoder older than one release is deleted, together with its segments.
- **Authoritative** (catalog, journal, memory): forward-only numbered steps applied in place at open by the
  executor (`ADD COLUMN`, new table, backfill in pages). A non-additive change ships as copy-migrate into
  `<class>.next` with an atomic rename. No copy is kept.
- **Beta upgrade** needs one thing more than `wipe --stale` + `init`. The first start of the new binary finds
  the old layout and runs a one-shot importer of `memory_v2_*` and holographic banks from `user-memory.db` and
  every `projects/*/tracedecay.db` into memory.db, keyed by the new `RepoId` (marker id, then root commit).
  It then deletes the old layout. Code rebuilds from source and transcripts re-ingest from host files. The
  importer is deleted one release later. `wipe --stale` is retired: rebuildable classes self-reset and
  authoritative classes migrate.

### 8. Public port surface

One crate, `tracedecay-storage`. Everything public:

- `Storage`: 10 methods (`open`, six class accessors, `status`, `lifecycle`, `shutdown`).
- Six concrete class handles. They have **no traits**: every store trait today has one production
  implementation (explorer 2, census), and tests use a real temp-dir `Storage`.
- Value types: `StorageConfig`, `Budgets`, `StorageStatus`/`ClassStatus`, `StorageError` (7 variants),
  `StoreClass`, `Lane`, `RepoId`/`CheckoutId`/`WorktreeId`/`WorktreeRef`, `LifecycleOp`/`LifecycleReport`.
- One callback trait, `Projector`, the domain transform the drain calls. It is a pure function from evidence
  to view rows. It exists because storage owns the loop and dead letters while session-runtime owns the
  transform.

The generic surface is 10 methods plus 6 accessors. Domain verbs live on the handles and keep today's
semantics: Work and Workflow carry their about 77 verbs, minus the 5 single-method read ports, which merge
into `WorkJournal::read(WorkRead)`. FactStore and ProjectMemoryFactStore carry their 39. All are now `async`
on concrete structs.

**Deleted from the public surface:** 19 store traits (143 methods), 24 Work ports, the 8 generic services over
them, the runtime protocol (`RuntimeReadRequestV1`, `RuntimeSubmitRequestV1`, `RepositoryWritePayloadV1`'s 15
serde variants, `RuntimeRequestProbeV1`), `ExactSqlHandle`, `StoreRuntimeClientLease`, `Database`, the
purpose adapters, `PhysicalRuntimeAttachment` (11), `StoreShardScopeV1`, the resolver and publisher ports,
`registered_schema::Installer`, `WalBudgetV1` and WAL constants, SQL DDL constants, and
`error_chain_is_sqlite_disk_image_corrupt`. No surface type is serde, carries SQL, names a file or exposes an
engine error.

### 9. Lifecycle

- **init:** `catalog.resolve(cwd, Register)`, then enqueue the worktree's first code build. It creates no
  files.
- **wipe <repo>:** writes a tombstone, then deletes that repo's rows in views, evidence (current and sealed
  segments, through a short-lived maintenance writer) and journal, and its code heads. Memory and identity are
  kept. It is idempotent: a crash resumes from the tombstone. Because no directory exists, "next open
  recreates the store" cannot happen.
- **wipe --all:** with the daemon stopped and the profile lock held, deletes `P/store/`.
- **projects forget <repo>:** wipe, plus memory rows, plus the identity rows; the tombstone is removed last.
- **doctor:** read-only. It returns `status()`, an on-demand `quick_check` per class, dead letters, findings
  and budget headroom. It never opens a file outside the daemon.
- **maintenance:** internal to `Storage`, every 30 s: idle checkpoint, path and inode watch, size sample,
  budgets, segment roll, pack GC, dead-letter redrive, journal TTL, and 256 pages of incremental vacuum per
  file.
- **The unlinked-profile incident (112 GB).** Every tick compares `stat(path)` (dev, ino) with each open fd's
  `fstat`, for about 20 files plus `profile-identity.json`. On a mismatch or absence, `Storage` enters
  `ProfileLost`: it rejects all operations with `StorageError::ProfileLost`, closes every connection (which
  releases the unlinked inodes) and the daemon exits for its supervisor to restart on the fresh profile.
  Detection takes at most 30 s.

### 10. Cutover plan

Each PR migrates its callers and deletes the legacy API in the same wave. Each runs Bazel `clippy`, `rustfmt`
and the unit tests of the touched crates, plus `//crates/tracedecay:daemon_suite`, `:mcp_suite`,
`:session_suite` and `:unit_test`.

1. **Code packs: the riskiest unknown, the engine swap.** Land `storage::code` pack writer and reader behind
   graph-db's existing `VerifiedGraphSnapshot`/traversal API, unwired. Proof: graph-db parity tests run the
   same fixtures against Grafeo and packs and must return identical traversal results and digests. A bench on
   this repo must show activation RSS O(1) and traversal p50 within 1.5x of Grafeo. **Gate:** if the pack
   misses, adapt axis 1 only: keep Grafeo as write-once sealed packs, with no staging container and no WAL.
2. **Shared executor.** Replace per-attachment writer and reader threads in rusqlite-runtime with the bounded
   executor. Add closure reads with deadlines, the TRUNCATE barrier, the idle checkpoint and the inode watch.
   Existing files stay in place. Proof: a new test opens 40 stores and asserts thread count == N; #4097's test
   runs under continuous readers and asserts WAL ≤ soft. daemon_suite passes.
3. **Async ports.** Work, Workflow, handoff, scope-set, remote, FactStore, ObservationStore and the git and
   native actors become async closure calls. Delete the sync `ExactSqlHandle` forms, `dispatch_read`, #4052
   `off_native_thread`, the actor threads and the contracts Work traits. Proof: `NATIVE_THREAD_{READER,WRITER}
   _WAITS` asserted == 0 at the end of mcp_suite and daemon_suite. Today they are 2,683 and 591.
4. **Identity and catalog.** Root-commit `RepoId`, the worktree table with per-root liveness, and catalog.db.
   Delete the resolver, `StoreShardScopeV1`, orphan GC, the cold store and the registry reap. Proof: a
   two-clones-plus-worktree fixture resolves one repo and three worktrees, and deleting a worktree drops its
   head on the next tick.
5. **Journal and memory, plus the importer.** Move Work, Workflow, handoff, git, native and remote into
   journal.db, and facts and banks into memory.db at FULL. Delete `remote/nodes/*`. Proof: the importer
   fixture holds old-layout facts and they read back identically.
6. **Evidence and views, plus the dead-letter drain.** Delete `sessions.db`, `user-sessions.db`,
   `projection_queue`, `lcm-payloads/`, `hook_analytics.jsonl`, the profile and project ingest split, and the
   string classifier. Proof: an injected poison head dead-letters and its sibling streams advance. An injected
   TEMP fault recycles the connection with no dead letter. A segment drop frees its bytes. session_suite
   passes.
7. **Code wiring.** Heads in catalog, packs in `code/`. Delete Grafeo, graph-db's staging and registry,
   `code-index-v1/`, the replay pool and code-index-retention. Proof: mcp_suite graph tests, and two
   worktrees at one commit share one pack.
8. **Lifecycle and budgets.** wipe, forget, doctor and status on the new layout. Delete tracedecay-maintenance
   retention and `wipe --stale`. Proof: a budget test fills evidence past budget and the oldest segment is
   unlinked. A profile-delete test removes `P/store` under a running Storage and gets `ProfileLost` within one
   tick, with the fds closed.

### Module map

```
tracedecay-storage  (new)  lib.rs Storage | engine/{executor,slot,checkpoint,fault,tx} | identity |
                           catalog | journal/{work,workflow,handoff,git_index,native,remote} | memory |
                           evidence/{segment,capture} | views/{drain,lcm,temporal,sessions} |
                           code/{pack,build,snapshot,gc} | budget | lifecycle | migrate_v2 (one release)
```

- **Deleted into it:** tracedecay-rusqlite-runtime, tracedecay-store-runtime, tracedecay-store,
  tracedecay-global-db, tracedecay-session-temporal-store, tracedecay-graph-db, tracedecay-code-index-retention.
  Also runtime-core's `shard_runtime/`, `db/` and `storage/` (runtime-core keeps config and worktree probing),
  the SQL halves of tracedecay-lcm and tracedecay-maintenance, and the drain in host-admission.
- **Kept, now pure policy over storage verbs:** tracedecay-lcm (summarization), tracedecay-sessions (parsers
  and the `Projector` impl), tracedecay-host-admission (spool), tracedecay-code-index (extraction feeding
  `PackBuild`), tracedecay-graph-query (queries over `CodeSnapshot`), and tracedecay-contracts (Work state
  machines, minus storage ports).
- **Crate count:** 51 → 44.

## Synthesis decision

Filled in by arena.

## Tradeoffs accepted

- We accept whole-file blast radius across repos for each class in exchange for a fixed file and thread count,
  one collector per class, and finer stream, connection and tenant isolation inside files.
- We accept a one-crate storage layer (large, with internal modules) in exchange for SQL, engine, WAL and
  recovery living behind one boundary. Today they leak into tracedecay-store.
- We accept losing summaries older than evidence retention if views.db corrupts.
- We accept rewriting graph storage under graph-db's API (PR 1 gate) in exchange for one engine, O(1)
  activation, and no mutable graph container.
- We accept that read closures are capped at 2 s, so long scans must page by key. This is what makes the
  checkpoint barrier total.
- We accept that root-commit identity merges repositories that share a root commit (forks, template-born
  repos) into one memory. The marker file splits them.
- Evidence at NORMAL may lose the last commits on power loss. The cursor is lost with them, so re-ingest is
  exact.

## Alternatives considered

- **One store set per project (the status quo, with ADR-003's catalog/activity/project split).** This keeps
  file isolation, but files, threads, checkpoint lanes and collectors scale with repos. It exposes per-project
  lifecycle (mount, orphan, forget) to callers through `ProjectServerKey`-shaped plumbing. It hides less:
  every caller must pick a shard.
- **One giant profile database.** The smallest file count, but it couples FULL and NORMAL durability,
  append-hot and transactional cadences, and every failure domain in one WAL. It is exactly ADR-003's
  rejected shape, and the depth is the same as this design with no containment.
- **Per-project class files (catalog per profile; journal, evidence, views and memory per repo).** This gives
  the strongest isolation, but it creates 4×P files and brings back per-directory GC and per-repo retention
  loops. Callers would need repo-routed handles.
- **Keep Grafeo for sealed code generations.** This keeps graph-query untouched, but it keeps a second
  engine, keeps the staging container for dependency-bearing generations, and keeps replay-form heap. It is
  the fallback if PR 1's gate fails.

## Implementation reconciliation

Nothing yet.

## Open questions and risks

- Is root-commit identity acceptable for template-born repositories, or should the default be root commit
  plus normalized remote path?
- Should `wipe <repo>` keep memory (proposed) or delete it the way today's local wipe does?
- Do 2 s read deadlines break any current query? LCM search over large FTS and doctor scans must page.
- Can packs match Grafeo traversal latency on dependency-bearing (cross-namespace) generations? This is PR 1's
  gate.
- A one-crate storage layer may cost incremental compile time. Is a two-crate split (engine, domain repos)
  needed?
- Are summaries reconstructible? If summarization is LLM-driven, its cost argues for moving summaries into
  memory.db's never-evict class.

## Next implementation step

Write `storage::code::pack` (pack schema, writer, digest proof, read-only snapshot) and a parity test that runs
graph-db's traversal fixtures against both Grafeo and packs.
