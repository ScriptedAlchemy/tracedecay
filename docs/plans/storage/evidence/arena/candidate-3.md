# Candidate 3: event log as the only authority for evidence, everything else derived

Shape in one line: framed, append-only log segments hold all evidence and journals; every other table is a
rebuildable read model with per-stream cursors and a dead-letter path; durable memory is the one SQLite
class that is authoritative. One SQLite engine, one trivial append format, seven storage threads.
Companion sketch: `sketch.rs`. Tree: origin/master e5a8ede6ea.

## Problem

Storage today is 32 inventory rows behind one runtime that gives every SQLite file 1 writer + 3 readers
(about 244 threads at 30 projects). A project `sessions.db` mixes four workloads: hot append-only evidence
(63 GB live), rebuildable projections, small transactional journals (Work, Workflow, git-index,
native-integration, handoff grants), and the projection queue. Each incident of 2026-10-16 is one of those
workloads breaking another through the shared file, the shared writer, the shared WAL or the shared queue:

- the 229 MB WAL: a checkpoint that cannot see its own blockers (PR #4097, 245k wakes);
- the 6-day ingest freeze: a fail-closed head with no dead-letter path, triggered by a connection-local TEMP
  fault that an error-string classifier read as file corruption (#4094, #4107);
- 2,683 native reader waits per mcp_suite run (#4085) and parked Work writers (#4052): sync ports over an async runtime;
- 153 GB unmeasured: no byte owner, retention of 500 rows per kernel per day on mounted stores, orphan GC that
  keeps a deleted worktree's store alive through `git_common_dir`;
- 112 GB written into unlinked inodes after the profile was deleted: nothing re-checks the path after attach.

Constraints I honor (fixed inputs and grounding): daemon is the sole mutable authority; SQLite stays (Zack:
"I want one only"); memory is FULL-durable, never size-evicted, no backups; identity is the repository;
the drain needs its dead-letter path; durability is per class; settlement outlives the request (#3711);
ADR-003's failure-domain split stands (I extend it).

The direction asked for here, argued at full strength: the append-only part of the workload is the part that grew
to 150 GB and froze for 6 days, and it does not need a database. Take it out of SQLite, make it a log, and
the checkpoint, vacuum, queue-head and "rows to delete" problems disappear for most of the bytes.

## Usage (caller's view)

Capture (the host-transcript refresh worker, in `tracedecay-sessions`). There is no cursor table: the cursor is
the end offset of the last chunk in the source's stream.

```rust
let repo = storage.repo(RepoProbe { root }, Intent::Open).await?;
let log  = repo.log::<Evidence>();
let from = match log.tail(source.stream()).await? { Some(t) => t.ev.end_offset(), None => 0 };
for chunk in host.read_from(source, from)? {             // pure parsing, no storage
    log.append(EvidenceEvent::Chunk { source, session, host, bytes: chunk.range, at, body: chunk.body }).await?;
}
```

An MCP tool that appends and then searches (read-your-writes without a cross-file transaction):

```rust
let a = repo.log::<Evidence>().append(ev).await?;
let hits = storage.conversation(ConvScope::Repo(repo.id()))
    .search(q, Freshness::AtLeast(a.last, Duration::from_millis(500))).await?;
if !hits.coverage.complete { note("results partial: {:?}", hits.coverage.rebuilding); }
```

A Work admission. No port, no SQL, no thread park. `decide` is a pure function the Work crate owns:

```rust
impl Aggregate for Placement {
    fn stream(c: &Admit) -> StreamId { StreamId::of(("placement", c.worktree)) }   // invariant boundary = worktree
    fn decide(&self, c: &Admit, now: UnixMs) -> Result<Vec<JournalEvent>, Busy> {
        if self.holder.is_some_and(|h| h != c.attempt && !self.expired(now)) { return Err(Busy) }
        Ok(vec![Work(Admitted { attempt: c.attempt, until: now + LEASE })])
    }
    ...
}
let r = repo.run::<Placement>(Admit { worktree, attempt }).await;   // load stream, decide, append_if(At(n)), retry on conflict
```

The operator view. `tracedecay storage report` prints bytes by class and repo from `statx`, with budgets and findings.
`rm -rf ~/.tracedecay` under a running daemon makes `storage.health()` flip to `Fenced` within 5 s, fds closed,
daemon exits non-zero. `tracedecay wipe --repo X` removes `repos/X/` including its identity; nothing recreates it
until an explicit `Intent::Init`.

## Shape

Authority rule: the log is the sole authority for evidence and journals. A view is a pure function of
(log prefix, projector version, source files). Anything not reproducible (an LLM summary, a git sample, a clock
reading) must already be an event. Two directions only: log to view, view to nobody. No transaction ever spans two
files, so there is no outbox, no watermark handshake between stores, and no cross-file atomicity problem.

### 1. Engine

- **SQLite via rusqlite stays** for the two things that need queries and transactions: durable memory, and the
  views (including the per-worktree code index). Decision from 2026-10-12 honored: no Turso, no second query engine.
- **Log segments are framed files, not SQLite files.** This is the argument against the one-engine decision, made
  directly. Zack ruled out running two *database engines* (a Turso that was slower and diverged from SQLite). A framed
  append log has no query planner, no schema, no locking protocol and no recovery semantics beyond "CRC and
  truncate". It is the repo's existing second persistence mechanism: framed append logs already ship in the
  hook admission ledger (1,746 lines), the host-admission spool (frames.rs, quarantine.rs, recovery.rs), the
  observability delivery spool, and the automation effect journal and run ledger, on
  `tracedecay-private-fs::framed_log` (the host-admission spool frames on `tracedecay_domain::framed_log`). Five
  bespoke framed logs become one. Net engine count goes from two (SQLite +
  Grafeo) to one query engine plus one 700-line format.
- Why not SQLite files for segments, concretely:
  1. Corruption granularity. SQLite has no page checksums. A flipped bit in an interior B-tree or overflow page can
     return wrong bytes without an error, or lose a subtree. A per-record CRC32C plus a per-segment footer hash
     detects it and bounds the loss to the record. For the authoritative copy of evidence, silent corruption is worse
     than detected loss.
  2. Error taxonomy. #4094 happened because one engine reports connection-heap faults and file faults with the same
     code. A framed reader has no heap-side state; a CRC verdict is a statement about bytes only.
  3. No WAL, no checkpoint, no vacuum for the largest class. Segment drop is `unlink`. The `auto_vacuum` creation-time
     trap (explorer 1, #9) and incremental compaction no-ops cannot occur.
  4. Write amplification: one sequential write and one fdatasync per group commit, against WAL double-write plus
     overflow-page chains for 100 KB bodies.
  5. Bodies are stored once. Today a line is stored up to four times in one file (observations, `lcm_raw_messages`,
     FTS text, `session_occurrences` + FTS). Views hold a pointer (segment, offset, length) and a contentless FTS5
     index (`content=''`, `contentless_delete=1`), not a copy.
- Cost accepted: about 700 lines of new format code with fuzz tests, and Windows needs a `DropPending` state for
  segments with open readers (private-fs already carries the Windows rename/sync helpers).
- **Grafeo is dropped, gated.** Evidence against it, all in the grounding: heap residency is O(rows) (8.6 GB container,
  20+ GB heap); the WAL checkpoints only at open and close; it can swallow WAL append errors; it is a git-pinned
  fork; its disk tier was judged NO-GO and retired; the compact digest proof failed at scale; vector indexes are
  retired; the SQLite comparison (plan 39 Task 12) was never run; and "one engine only" is already violated by its
  existence. Code graph becomes one in-place SQLite file per worktree. Graph traversal on SQLite (recursive CTE over
  indexed edge tables) is the unproven claim, so cutover PR3 is a measured gate. If it fails, Grafeo survives
  behind the same `CodeIndex` port as one file per worktree with a mandatory timer checkpoint, and every other
  decision here stands. This is the one axis where I pre-register an adaptation.

### 2. Unit of storage and sharing across worktrees

- **One repository = one directory** `repos/<RepoId>/`. One database is one *class* inside it. A file is a segment, a
  view shard, a memory file, or a worktree index. Registry is the directory: `profile/alias/<hash(key)>` (file whose
  content is a RepoId) plus `repos/*/repo.json`. `global.db` registry tables, `store_instances`, `graph_scopes`,
  `store_artifacts`, `project_aliases` are deleted. `wipe` and `forget` are the same operation, so a wipe cannot leave
  a registry row that re-creates the store.
- **Identity ladder** (decided input 3). A full clone has its own git dir, so identity cannot be the git dir:
  1. marker `<git_common_dir>/tracedecay-repo.json` (survives moves and renames);
  2. normalized origin/upstream URL (scheme, case, `.git` suffix stripped);
  3. root commit set, skipped when `.git/shallow` exists (shallow clones report the wrong root);
  4. path-local, which never matches another checkout.
  Every key found is written as an alias, so a clone with a different remote joins through the root commit. A fork that
  shares the root commit joins the same repo; that is intended for this operator (14 clones of one repo), and is an open
  question for forks that diverge.
- **30+ agent worktrees**: 30 `worktrees/<key>.json` records and 30 `code/<key>/index.db` files in one repo directory.
  The only per-worktree bytes are the code index. Everything mutable and shared (evidence, journal, memory, views) is
  per repository, so 14 clones + 32 linked worktrees of tracedecay are one `repos/<id>/`.
- **Attribution**: a transcript source belongs to the repository of its host project directory (Claude encodes the
  launch cwd in the path). Sources with no project go to the pseudo-repo `_loose`. ADR-003's reason for a
  profile-wide activity store ("a session may touch zero, one or many projects") is met by fan-out at the read
  boundary: `ConvScope::AllRepos` queries every repo's views and merges. The cap of 32 shards per query from the July
  plan applies.
- **Profile-wide data**: `profile/memory.db` (user memory, configuration revisions, preferences), `profile/log/telemetry`,
  `profile/views/telemetry.db`, `profile/cache/`, `profile/alias/`, `profile-identity.json`. Observations and transcripts
  are per repo, not profile-wide. LCM payload files do not exist: large bodies are frames in the evidence log.

### 2a. Partitioning table

Axes: failure domain (a file is the unit of containment; a record is the unit inside a log), write cadence,
retention (declared per class in `LogClass::RETENTION`, enforced by one governor), sharing scope.
Defaults are tunable; what matters is that each exists, has one owner, and is observed.

| Data class | File(s) | Cadence | Retention | Scope | Authority |
|---|---|---|---|---|---|
| Evidence log (transcript chunks, source resets) | `repos/R/log/evidence/seg-*.seg`, sealed at 64 MiB or UTC midnight | hot append, Window(250 ms) | 180 d and 8 GiB per repo, whole-segment drop | repo | authoritative |
| Digest log (LLM summaries) | `repos/R/log/digest/` 8 MiB or 30 d | rare, Fsync | 2 y, 512 MiB | repo | authoritative (not reproducible) |
| Journal log (Work, Workflow, handoff grants, git-index tx, native integration, hook admissions; remote replay in profile scope) | `repos/R/log/journal/` 8 MiB or 7 d | low, Fsync | 90 d, 1 GiB; live aggregates re-snapshotted at roll | repo (+profile) | authoritative |
| Telemetry log (hook route analytics, savings, observability) | `profile/log/telemetry/` 16 MiB or 1 d | steady, Never | 30 d, 256 MiB | profile | expendable |
| Conversation views (sessions, message index, LCM tree, temporal) | `repos/R/views/conv-YYYYMM.db` | background batches | dies with its month of log | repo | rebuildable |
| Journal views (Work/Workflow lists, topology, git-index) | `repos/R/views/journal.db` | background | with its log | repo | rebuildable |
| Telemetry views | `profile/views/telemetry.db` | background | with its log | profile | rebuildable |
| Code index (symbols, edges, trigram/FTS) | `repos/R/code/W/index.db`, in-place incremental | per refresh | gone when root gone; 30 d idle; 8 GiB per repo | worktree | rebuildable from source |
| Repo memory | `repos/R/memory.db`, synchronous=FULL | low | forever, never size-evicted | repo | authoritative, transactional |
| Profile memory and config | `profile/memory.db`, FULL | low | forever | profile | authoritative, transactional |
| Identity | `repo.json`, `alias/*`, `worktrees/*.json`, marker, `profile-identity.json` | rare | until forget | repo | authoritative, tiny |
| Cache (response handles, host bundle stage) | `profile/cache/` | per call | TTL swept by governor tick | profile | ephemeral |

Splits and merges, each justified:
- **Evidence vs Journal vs Digest vs Telemetry are separate logs** because durability (Window vs Fsync vs Never),
  retention (180 d vs 90 d vs 2 y vs 30 d) and blast radius differ. A hot evidence segment never shares a sync with a
  Work admission. Digest is separate because a summary outlives its sources by design and cost real model calls.
- **Work, Workflow, handoff, git-index, native-integration, admissions merge into one Journal log.** Same cadence,
  durability and retention, all aggregate streams. A bad record loses one record, not a file, so a finer split buys
  nothing. This is the merge that deletes two std actor threads per project and 24 sync ports.
- **Memory is its own file per scope**, not a table in a view file: it is the only class that cannot be rebuilt, so
  nothing rebuildable may share its file, WAL or writer lane.
- **Views are sharded by month of log time** so a corrupt shard rebuilds from at most a month of segments and retention
  drops a shard with its segments.
- **Per repo, not one profile file**: `forget` is a directory delete; one repo's fault cannot reach another;
  nothing is "mounted", so an idle repo costs zero threads, connections and heap.
- The ADR-003 argument ("one giant database couples privacy, backup and failure domains") is accepted. Backup is moot
  by decision; privacy is now a directory; failure domain is the whole point of the table.
- **Threads do not scale with files.** One shared executor (section 4): 7 threads total versus 4 per store today.

### 3. Durability and WAL/checkpoint ownership

- Durability is a constant on the class, in types: `LogClass::DURABILITY`. Journal and Digest `Fsync` (group commit,
  dwell 2 ms). Evidence `Window(250 ms)`: legal because the capture cursor is derived from the log tail and the host
  file is the re-readable source, so a crash re-captures at most 250 ms. Telemetry `Never`. Memory
  `synchronous=FULL` (plus `fullfsync` on macOS). Views `synchronous=OFF`: they are rebuilt, not recovered.
  This closes the NORMAL-versus-FULL drift: the writer's global NORMAL (`connection/mod.rs:812`) is gone, and the
  receipt-replay guarantee sits on the Journal's fdatasync.
- **Logs have no WAL.** There is nothing to checkpoint for the bytes that grew to 153 GB.
- **Views and memory: the owning lane checkpoints, always `TRUNCATE`, always under a barrier.** Triggers: WAL over
  8 MiB (views) or 4 MiB (memory) after a commit, 30 s idle with a non-empty WAL (fixes "idle stores never
  checkpoint"), shard seal, close. Barrier: mark the file Draining so new read leases queue (they wait, they are not
  rejected); outstanding leases end within their deadline; checkpoint TRUNCATE; reopen.
- **Why the PASSIVE-under-overlapping-readers ratchet cannot occur.** (a) There is exactly one attachment per file, so
  the blocker set is exact; today `ReaderCheckpointBlockers` sees only its own pool and "pending, blockers: []" is
  normal. (b) Every read runs under `progress_handler` (rusqlite `hooks` is already enabled) that aborts it at its
  deadline (2 s foreground, 30 s background; longer queries page by keyset), so a lease is bounded by construction.
  (c) The mode is TRUNCATE after admission is closed, which returns busy rather than "complete" while a reader exists;
  PASSIVE is never used on views. (d) There is no sticky hard-drain mode with a retry loop; one checkpoint is a bounded
  procedure whose failure moves to a finite ladder. The barrier waits by event, so no lane blocks.
- Residual: a process outside the daemon holding a read mark on a view file (a developer's `sqlite3` shell). Ladder:
  views over 64 MiB WAL drop the shard and rebuild; memory falls back to PASSIVE and raises
  `Finding::ExternalReader` (memory WAL growth is bounded by its tiny write rate). CLI and doctor never open files;
  offline commands take the profile lifecycle lock.

### 4. Execution class

- Model: **seven OS threads for the whole daemon**: 2 foreground lanes (log appends, memory.db writes), 2 background
  lanes (view writes, rebuilds, maintenance, identity probes), 2 read lanes (read-only SQLite connections, segment
  `pread`), 1 clock thread that only enqueues timer jobs. Threads are named `td-fg-0`, `td-bg-1`, so `/proc` can tell them
  apart (today they truncate to `tracedecay-rusq`).
- A file maps to a lane by hash, so each file has exactly one writer thread and no file has its own. Connections are
  thread-affine inside a lane, in an LRU of at most 16 per lane (about 100 open files daemon-wide, a count not a
  thread count). Idle repos hold zero connections.
- Foreground lanes never run view work, so a 50 ms projection batch cannot delay a Work admission. Each job is bounded
  (foreground under 5 ms of CPU, background steps under 50 ms), and long work is chunked into jobs, not held.
- **Async/sync boundary: the public surface is 100% `async fn`, and there is no synchronous storage entry point.**
  A call enqueues a job on a bounded `async_channel` (typed `Saturated`, per #883) and awaits a oneshot. A native
  worker only ever awaits a oneshot, so it cannot park on storage. `dispatch_read`, `ExactSqlHandle`,
  `recv_blocking` and the `off_native_thread` wrappers around Work handlers have no counterpart to call, which makes the
  native reader/writer wait counters (#4083) read zero by construction.
- Work/Workflow become `Aggregate`: `decide`/`evolve` are pure CPU functions, run on the caller's thread. Their
  only I/O is `Log::stream` and `Log::append_if`, both async. That retires class (c) and (d) of the four execution
  classes in the explanation (sync contracts port, std actor with `block_on`).
- Settlement outlives the request: once enqueued, the job owns its data and completes if the caller drops the future;
  the reply send fails silently. No runtime-owned task is needed for it.
- Open, WAL recovery and all identity probing (`/proc/self/mountinfo`, canonicalize, gix) run on lanes. Nothing
  inline on a native poll.

### 5. Corruption containment

| Fault | Blast radius | Detection | Recovery, no operator |
|---|---|---|---|
| Bad record in a log | that record; `Log::stream` returns `StreamItem::Gap`; other streams, other projections unaffected | CRC32C on read; `verify` | reader resyncs at the next magic whose header chain validates for two frames; the gap is written to `<seg>.gaps`; byte ranges of `Chunk` events are contiguous, so capture sees the hole and re-reads it from the host file if the file still exists |
| Torn tail after crash | un-acked frames only (Fsync class) or the last 250 ms (Window class) | recovery scan of the active segment at open | truncate to the last valid frame; capture re-derives its cursor from the new tail |
| Corrupt sealed segment | that segment's Position range; views of that month show `Coverage.gaps` | footer hash on first read and in `verify` | marked Lost in the manifest; becomes a Gap; ages out by retention; projections never wait on it |
| Bad poison input (an event that cannot project) | one event | `ApplyError::Rejected`, or 3 failed confirmations | dead-letter row (stream, seq, code) in the view; the stream continues; counted in `Coverage.dead_letters` |
| Corrupt row or page in a view | that shard (one month of one repo) | SQLITE_CORRUPT, then `confirm` | shard deleted and rebuilt from the log; queries answer `Coverage.rebuilding` meanwhile |
| Process-lifetime fault (#4094) | one job on one lane; no stream is penalized | `confirm`: drop the lane's connections, rerun the record on a fresh one | passes: `ProcessLocal`, counted, continue. Fails the same way: `quick_check(1)`; check fails: `FileCorrupt`; check passes: `PoisonInput` |
| Corrupt code index (the graph) | that worktree | `quick_check` at open, SQLITE_CORRUPT in a read | delete `index.db`, next refresh re-indexes from source; other worktrees untouched |
| Corrupt `memory.db` | that scope's memory | `quick_check` at open and daily on the background lane | row-wise salvage into a fresh file, then swap; `Finding::MemoryCorrupt{salvaged}`. No backup exists by decision, so unsalvageable pages are lost; this is the one residual I do not remove |
| Profile deleted or replaced | everything | `ProfileAnchor` (section 9) | fence and exit |

How the #4094 class is made rare and harmless: projection scratch state lives in Rust memory, never in TEMP
tables; projection connections are recycled every 10,000 jobs or 10 minutes; and no code classifies by error
string, because the verdict comes from the experiment. The 6-day freeze needs a head that blocks unrelated work;
section "Projection" below removes the head.

**Projection (the dead-letter path, decided input 4).** Order matters only inside a stream, so the cursor is per stream.
Each view file holds `view_watermark` (log position below which every non-parked stream is applied or dead-lettered),
`view_streams(stream, applied, parked)` and `view_dead_letter`. A Transient failure parks that stream only
(later records of the stream wait, "never skip ahead" holds inside it); a parked stream retries with backoff and after
1 h dead-letters. A Rejected record dead-letters immediately, as today's Contract and SanitizationRefused skips do,
now durable and counted. The watermark advances in the same transaction as the rows. The July rule "no checkpoint
advances over a gap" becomes "no stream advances over a gap except through a recorded dead letter".

### 6. Retention and bounded growth

One owner: `Governor`, one tick, stateless (the filesystem is the state, so a crash anywhere is safe). Size is observed
from `statx` `st_blocks` per file, per class, per repo, so it works at any size and without opening a database (today's
64 MiB cap and the "other" bucket for Grafeo files vanish). The report is published every tick and persisted as telemetry.
Retention is whole-segment unlink or shard drop; no row-level DELETE exists in the system. At budget: shed in order
cache, telemetry, code index (LRU), evidence (oldest segment first, across repos weighted by size); Digest, Journal and
memory are never shed by size. Each shed raises `Finding::OverBudget` with the byte count. Profile cap defaults to
40 GiB (not 153).

Growth sources from `growth-sources-4099.md`:
1. **Per-worktree stores never collect.** There are no per-worktree stores. The per-worktree unit is `code/<key>/`;
   the governor checks every `worktrees/*.json` each tick and removes the code dir at once when the root is missing or
   `.git` no longer resolves to this repo. This separates repository liveness from worktree liveness, the conflation
   that makes `live_git_common_dir_keeps_a_linked_worktree_store_live` true. Unregistered `proj_*` dirs: any dir under
   `repos/` without a valid `repo.json` is deleted after 24 h. The "any memory row blocks collection" problem is
   inverted into policy: memory is a separate KB-sized file, so a repo whose roots are all gone keeps only `memory.db`
   and `repo.json` while its logs age out; nothing else is held hostage. `/fast/tmp` roots are not special: liveness is
   root existence plus `last_seen`.
2. **Hard-pressure drain retrying PASSIVE forever.** Removed. Section 3: no hard-drain state, TRUNCATE under a
   barrier, bounded leases, finite escalation (drop and rebuild the shard).
3. **Grafeo WALs with no periodic checkpoint.** Removed with Grafeo. If the PR3 gate fails, the surviving per-worktree
   Grafeo file is covered by the same idle-timer rule (a trait obligation on the engine adapter).
4. **LCM payloads.** Bodies are frames in the evidence log, dropped by age or budget. The "summary must cover the raw row"
   dependency is gone (summaries are Digest events with their own horizon). No payload files, so no orphans, no
   write-before-transaction crash window, no "offload frees nothing".
5. **Observation projection** (active, skeleton and disposition rows forever). Rows live in month shards and die with
   the shard. Dispositions are events. No tombstoned-payload rows.
6. **Generation staging.** Does not exist: no staging container, sealed generations, replay pool, hard links, or
   release cursor with head-of-line.
7. **`hook_analytics.jsonl`.** Becomes the Telemetry log (rolled, 30 d, 256 MiB).
8. **`response-handles`.** `Cache` with TTL swept every governor tick, not once at MCP build.
9. Also: writer idempotency ledger (stale-incarnation rows live forever) is replaced by `EventId` + stream expectation,
   no ledger table; `maintenance/` inventory logs and host bundle stage fall under the cache sweep.

### 7. Schema evolution

- **Log events**: each frame carries `kind` and `ver`. `LogClass::upcast` (in `tracedecay-events`, where the domain
  knowledge is) maps old versions forward. An unknown future kind decodes to `None` and is retained as raw bytes. A
  segment with a newer format parks that class as `Unsupported`, not corrupt.
- **Views**: bump `ViewSpec::VERSION` and every shard is dropped and rebuilt from the log, newest month first. "Regenerate,
  never migrate" (Zack 2026-07-24) becomes the mechanism, not a reset command. Queries report `Coverage.rebuilding`.
- **Memory**: the only class with stepped, forward-only migrations (`user_version` plus numbered steps, each one
  transaction, golden-file tests). Today's reset-required is not available for authoritative data.
- **Code index**: version mismatch deletes the file.
- **Beta upgrade path.** `wipe --stale` plus `init` still works, and the design needs exactly one more thing: a one-shot
  `tracedecay storage import-memory` reading `memory_v2_*` from legacy v39 `tracedecay.db` and `user-memory.db`
  (plain rusqlite SELECT) into the new `memory.db`. Evidence is not migrated; the new capture re-reads host JSONL from
  byte 0. The importer is a command, not a layer, and is deleted one release later. `profile-identity.json` gains
  `layout: 2`; an older layout gives `OpenError::NeedsReset`, naming what is removed and what is kept.

### 8. Public port surface

Public: 2 traits (`LogClass`, `Aggregate`), 3 handles (`Storage`, `Profile`, `Repo`) plus `Worktree`, `Log<C>`,
`Conversation`, `JournalViews`, `CodeIndex`, `Memory`, `Cache`, and plain data. Method census from the sketch:
`Storage` 12, `Profile` 4, `Repo` 6, `Worktree` 2, `Log` 5, `Conversation` 7, `JournalViews` 5, `CodeIndex` 4, `Cache` 2
= 47 storage-owned methods, plus 9 trait functions, plus 40 `Memory` methods carried over unchanged (FactStore 9 +
ProjectMemoryFactStore 30 + graph 1; I do not claim to shrink the fact API). Total about 96.
Today: 19 store traits / 143 methods, 24 sync Work/Workflow ports / 77 methods, 15-variant `RepositoryWritePayloadV1`, a
sync `dispatch_read`, ExactSql handles, and about 20 sync Remote ports, so 220 methods and 43 traits before those.
What the surface hides: segment and frame format, executor and lanes, connection LRU, checkpoint barrier, projectors,
shard layout, identity ladder, governor, anchor. What it does not carry: SQL text, WAL limits, corruption strings,
serde "store-protocol" enums, `Code{Worktree|Branch|Snapshot}` pseudo-shard scopes, DDL constants. The code-reader query
surface (callers, impact, search) is `tracedecay-graph-query`, ported onto SQL inside the crate, and is not recounted.

### 9. Lifecycle

- `init`: `storage.repo(probe, Intent::Init)`; idempotent; mints from the strongest key, writes aliases, marker and worktree record.
- `wipe`: `Storage::wipe(scope)` through the daemon. It fences appends to the scope, closes its handles, removes the
  directory including identity, reopens empty. `--stale` is `WipeScope::Derived`: everything except memory. Memory goes
  only with `memory: true`.
- `projects forget`: `wipe(Repo{memory: true})`. Same code path as wipe, so there is no registry row to survive it.
- `doctor`: `Storage::verify(depth)`: segment footers and frame CRCs, view `quick_check`, memory `quick_check`,
  budgets, dead letters, gaps. Repairs only what maintenance repairs anyway.
- maintenance: `Governor::tick`, on a timer and on demand; idempotent; resumable.
- **Profile deleted while the daemon runs.** `ProfileAnchor` holds a directory fd and the inodes of the profile root and
  `profile-identity.json`. It checks at every group commit (`fstat` `st_nlink` of the active segment fd, nanoseconds),
  every 5 s (`stat(path).ino` against the held inode), and at every file open. On mismatch the storage is fenced: all
  calls return `ProfileGone`, all fds close (the 112 GB of unlinked inodes are released at once), `Health::Fenced`
  publishes, the daemon exits non-zero for the supervisor. A profile recreated at the same path is caught by the inode
  comparison.
- Per-class result for the 24 h project-open grant freeze (#4100, not traced, `project_server_lifecycle.rs` unread):
  grants become Journal aggregates on a foreground lane with no WAL, no hard drain and no projection queue behind them.
  This removes the adjacent mechanisms the explanation lists. I do not claim the root cause.

### 10. Cutover plan

Riskiest unknown first. Each PR deletes its legacy in the same wave; no dual write, no compatibility layer. Bazel targets
are the touched crates' unit tests, clippy and rustfmt, plus the named suites.

1. **PR1 events + log + `Aggregate`, no callers.** New crates `tracedecay-events`, `tracedecay-storage` (log only, plain
   threads). Proves the riskiest design claim: Work invariants fit single streams. Transcribe the three hardest ports
   (placement admission, run control, fan-out census) into aggregates and run the existing scenario tests against old and
   new as oracles; plus crash-injection (kill at random byte), bit-flip fuzz (loss is confined to the record), torn-tail
   and resync tests. Targets: crate unit tests, clippy, rustfmt. Gate: an invariant that needs two streams moves that port
   family to a SQLite transactional `ledger.db` class for Work only (the named adaptation) before anything else starts.
2. **PR2 executor, connection LRU, checkpoint barrier.** Proves #4097 and thread bound as tests: thread count equal for
   1, 100 and 500 files (read `/proc/self/task`); continuous writers plus overlapping readers keep WAL at or under soft
   plus one batch; no job outlives its deadline plus epsilon; injected connection fault yields `ProcessLocal` and no
   penalty (the #4094 repro). Targets: storage unit tests, clippy, rustfmt.
3. **PR3a code-index gate (additive).** `CodeIndex` on SQLite with parity against Grafeo on three real repos for callers,
   callees, impact, call_chain, search; RSS and p95. **PR3b** migrates graph callers and deletes the staging container,
   sealed lane, publication journal, replay pool, `code-index-retention` generations if the gate passes (else deletes only
   the staging container and namespaces). Targets: graph-query and code-index unit tests, `:mcp_suite`, `:daemon_suite`.
4. **PR4 identity, layout, governor, anchor.** Directory-as-registry, alias ladder, worktree liveness, `SizeLedger`,
   `wipe/forget/init/doctor` on `Storage`, deletion of registry tables and orphan GC. Daemon_suite tests: delete the
   profile under a running daemon and assert fence in 5 s with no deleted fds in `/proc/self/fd`; 14 clones + 32 worktrees
   of a fixture repo resolve to one directory; a deleted worktree's index is gone within one tick.
5. **PR5 memory.** `memory.db` at FULL, importer, FactStore callers migrated, `memory_v2_*` removed from `tracedecay.db` and
   `user-memory.db`, configuration revisions into profile memory. Power-loss fault injection through private-fs; `:mcp_suite`,
   `:unit_test`.
6. **PR6 evidence, digest, conversation views, dead letters.** Capture appends; projector with parked streams and dead
   letters; search/LCM/temporal queries on views; delete observations, projection queue, `projection_drain`, host-admission
   spool, payload files, `sessions.grafeo`. Proves: one poison record per hundred leaves the rest projected with one dead
   letter; `view == rebuild(view)` property test; bytes per message against the old four-copy layout. Assumption to verify
   in this PR: the 12 `external_source_*` ledgers exist only for exactly-once between capture and projection, which
   `EventId` plus the watermark transaction replace. Targets: `:session_suite`, `:mcp_suite`, `:unit_test`.
7. **PR7a git-index, native-integration, hook admissions to Journal** (removes two std actor threads per project).
   **PR7b Work, Workflow, handoff aggregates** (deletes 24 sync ports, `ExactSqlHandle` sync API, the `off_native_thread`
   wrappers of #4052). Acceptance: native reader and writer wait counters are zero in `:mcp_suite` and `:daemon_suite`.
8. **PR8 telemetry log, cache, budgets enforced.** Fill-to-budget tests drop the oldest segment; ENOSPC sheds and recovers.
9. **PR9 sweep.** Delete what is left of `rusqlite-runtime`, `store`, `store-runtime`, `global-db`, `runtime-core` `db/`
   and `shard_runtime/`. Proof: `bazel query rdeps` shows no users of deleted crates; all suites green. Should be small
   because PR3 to PR8 each delete their own legacy.

## Module map

New: `tracedecay-events` (ids, event enums, `Aggregate`, `LogClass`, upcasters; no I/O), `tracedecay-storage` (modules
`log/`, `exec/`, `views/{conversation,journal,telemetry}`, `memory/`, `code/`, `identity/`, `governor/`, `anchor/`).
Kept: `tracedecay-private-fs` (durable file primitives used by the log), `tracedecay-domain`, `tracedecay-framing` (wire
frames, unrelated), `tracedecay-daemon-identity` (authority election), `tracedecay-graph-query` (queries, re-based on
SQL), `tracedecay-sessions` (host parsers, now emit events), `runtime-core`'s `native_runtime` and `ProfileRoot`.
Deleted: `tracedecay-rusqlite-runtime`, `tracedecay-store`, `tracedecay-store-runtime`, `tracedecay-global-db`,
`tracedecay-graph-db` (if the gate passes), `tracedecay-code-index-retention` generations,
`tracedecay-session-temporal-store` (types to events, projection into `views/`), the SQL, schema, retention and payload
parts of `tracedecay-lcm` (summarizer stays), `host-admission` spool and drain, the retention half of
`tracedecay-maintenance`, the 24 Work/Workflow and sync journal ports in `tracedecay-contracts` (services become
aggregates in place). Net: 2 crates added, about 8 removed or gutted. Call chain for any read or write is three files:
caller, `Log`/`Conversation` handle, executor job.

## Synthesis decision

Filled by arena.

## Tradeoffs accepted

- We accept a new 700-line file format with its own fuzz suite in exchange for removing WAL, checkpoint, vacuum and
  silent page corruption from the largest class. This is the choice most open to the "one engine" objection.
- We accept two-step reads (log then view) and a `Freshness` parameter in exchange for no cross-file transactions.
- We accept event-sourced Work/Workflow, a rewrite of 24 ports, in exchange for removing the sync-port and
  shared-file failure classes. Cross-stream invariants must be modelled by choosing the stream key.
- We accept that a view shard can be briefly partial (`Coverage.rebuilding`) in exchange for never migrating a view.
- We accept month-sharded conversation views and fan-out reads in exchange for bounded rebuilds and free retention.
- We accept that unsalvageable `memory.db` pages are lost. No backups is a decision; the design shrinks the exposure
  (FULL, small file, quick_check, row salvage) and does not claim to remove it.
- We accept `Window(250 ms)` for evidence: a crash can lose a quarter second that the host file re-supplies.
- We accept that a fork sharing a root commit shares memory with its parent repo.
- The clock thread, fixed lane counts and absence of tuning knobs may look like missing configuration; it is deliberate
  ("limits are symptoms, not knobs").

## Alternatives considered

- **Keep today's shape and patch** (one `sessions.db` per project, fix the drain, RESTART checkpoint, add budgets).
  Surface stays at 143 + 77 methods with WAL limits, DDL strings and wire enums in the contract crate; the coupling of
  evidence, projection, journals and WAL in one file remains and #4097-style bugs return. Loses on depth.
- **One SQLite file per data class (profile-wide or per repository), authoritative evidence still in SQLite.** Better
  failure domains than today, but the 90% of bytes that are append-only still live behind a WAL, a checkpoint, a
  vacuum story and a queue, and projections need a cross-file outbox or watermark protocol between the evidence file
  and the projected files. The shared executor fixes threads, not those. I keep the file-per-class idea for the
  derived and transactional classes and take only the append-only class out of the engine.
- **Log segments as SQLite files.** Same logical design, one engine in letter. Loses on silent corruption, error
  taxonomy and write amplification (section 1). Segment format is private to `log/segment.rs`, so this is a swap
  inside one file if Zack reads "one engine" strictly: cost is a per-row CRC column and roughly 2x write amplification.
- **Full event sourcing including memory.** Rejected: memory needs point updates and FULL commit, and a mirrored
  journal would be a backup by another name.
- **Embedded KV (redb, LMDB, sled) for the log or the views.** A third persistence engine with its own recovery
  semantics, for no property a framed file lacks. Rejected on the one-engine ruling and on depth.

## Implementation reconciliation

None yet.

## Open questions and risks

- Does "one engine only" cover a framed append log? If yes, what is the cost Zack accepts for SQLite segments
  (roughly 2x write amplification and a per-row CRC)?
- Can every one of the 24 Work/Workflow ports be a single-stream aggregate? PR1 answers it on the three hardest. If one
  needs two streams, is a `ledger.db` for that family acceptable? This is the biggest risk in the design.
- Is SQLite fast enough for transitive graph queries on the largest real repo at the memory ceiling we want? PR3a decides.
- Evidence retention 180 d / 8 GiB per repo and a 40 GiB profile cap: are these the right defaults for Zack's host?
- Rebuild time for a month shard from several GB of segments is unmeasured; the design assumes minutes, not hours.
- A source first captured into `_loose` stays there after its repo is initialized. Is re-attribution worth an event?
- Forks sharing a root commit share one repo and one memory. Wrong for a fork that diverges; right for this operator.
- #4100 (24 h grant freeze) is plausibly closed but not traced; `project_server_lifecycle.rs` was not read.
- Digest events are the only record of LLM summaries; losing a Digest segment costs money to regenerate. Should Digest
  have a second copy? ("No backups" says no.)

## Next implementation step

Create `tracedecay-events` and the `log/` module of `tracedecay-storage`, and write the crash-injection and bit-flip tests
for it before any other code, then port the placement-admission port to an `Aggregate` and run the existing scenarios
against both implementations.
