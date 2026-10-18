# Storage redesign: project stores by class on one bounded executor

Status: accepted design, delivery in progress. Owner: the storage lane.
Decided 2026-10-16 through the architect playbook: ground (how and why), an arena of
four whole-shape candidates, a cross-judge, synthesis, and an interrogate pass whose
findings revised this text (section "Interrogate verdict"). The trail of every
decision is [`decisions.tsv`](decisions.tsv); its `storage-arch/...` evidence paths
map to [`evidence/`](evidence/), which holds the grounding, the four arena
candidates, the judge's scores and the three interrogate reviews. The why synthesis
and the deliberation notes quote private session transcripts and are not published;
their constraints are restated in this document. The target type sketch is
[`sketch.rs`](sketch.rs). This plan supersedes the conflicting parts of plans
[02](../tracedecay-v2/02-store-crate.md),
[38](../tracedecay-v2/38-storage-retention-size-and-efficiency.md),
[39](../tracedecay-v2/39-embedded-grafeo-graph-database.md) and
[40](../tracedecay-v2/40-partitioned-generation-storage-and-zero-copy-replay.md);
section "What this supersedes" lists each point.

## Problem

TraceDecay's storage is correct row by row and has almost no store-level
mechanisms. Every incident of 2026-10-16 follows from that:

- The profile reached 153 GB and nobody measured it (#4099). Evidence tables are
  append-only, retention moves 500 rows a day on mounted stores only, orphan GC
  keeps a store Live through the shared `.git`, Grafeo WALs are checkpointed only
  at open and close, and doctor cannot see a store larger than 64 MiB.
- One project `sessions.db` carries the observation log, its projection queue,
  LCM, temporal tables, Work, Workflow, handoff grants, git-index transactions and
  native-integration journals. A corrupt in-heap TEMP cache on its writer
  connection (#4094) was string-matched as file corruption, and the fail-closed
  drain froze all transcript ingest for 6 days. The same file's WAL ratcheted to
  229 MB through 245,264 retry wakes (#4097) and stalled Work and git with it.
- Every open SQLite file costs 1 writer and 3 reader threads, and mounted projects
  never close: 30 projects cost about 244 threads at rest.
- Callers reach storage through four execution classes, chosen per call. The
  typed `dispatch_read` parks a native async worker inside an async port
  (2,683 waits per `mcp_suite` run, #4085); sync Work ports park writers (#4052).
- Identity keys on the git directory, so each of the host's 14 full clones of one
  repository minted its own store.
- The profile was deleted under a running daemon and the daemon kept writing to
  112 GB of unlinked files. Nothing checks a store's path after attach.

The design honors these fixed inputs: one engine, SQLite through rusqlite (Zack,
2026-10-12: "I want one only"); the daemon is the only mutable authority (#480);
settlement outlives the request (#3711); no backups, ever; durable memory survives
indefinitely at full durability and is never evicted for size; project identity is
the repository, not the git directory; the projection drain gets the dead-letter
path its 2026-07 design specified; durability is chosen per storage class; thread
count must not scale with file count.

## Usage (caller's view)

The daemon owns one `Storage`. Everything else gets a `Project` from it, then a
class handle. Every method is `async`, admits or refuses at once, and never parks
the caller's thread.

```rust
// Daemon start: starts the fixed storage executor (N threads, default 6).
let storage = Storage::open(profile_root, StorageConfig::from(&config.storage)).await?;

// Any request with a cwd: identity is the repository; every clone and worktree agrees.
let project = storage.project(&Checkout::discover(&cwd)?).await?;

// MCP Work handler. One idempotent journal transaction; decisions are pure contracts
// functions over the aggregates the transaction loads.
let outcome = project
    .work()
    .transact(operation_id, move |tx| work::request_cancellation(tx, attempt_id, &cmd))
    .await?;

// Session refresh worker: capture into evidence, project per stream into derived.
let sessions = project.sessions();
sessions.capture(CaptureBatch { source, frames, cursor }).await?;
let pass = sessions.project_pending().await?;   // dead-letters a poisoned row, never stalls

// Memory and retrieval.
project.memory().commit(fact).await?;
let hits = sessions.search(SessionQuery::text("checkpoint ratchet").limit(20)).await?;

// Code graph of one worktree; code indexes stay per worktree.
let reader = project.code(&worktree).await?.open_head().await?;

// Doctor, `tracedecay storage report`, wipe and forget all go through the daemon.
let usage: StorageUsage = storage.usage().await;
storage.reset(project.id(), ResetScope::KeepDurable).await?;
storage.forget(project.id()).await?;
```

Errors are domain-typed: `Busy { retry_after }`, `Unavailable { class, reason }`,
`Refused(..)` and `NotFound`. No SQLite text, error code, WAL size or wire enum
reaches a caller.

## Shape

### On-disk layout

```
P/                                 profile root (identity, lock and socket files unchanged)
  catalog.db                       projects, checkouts, identity anchors and aliases,
                                   configuration revisions                         FULL
  telemetry.db                     hook events, usage history, findings, loss receipts NORMAL
  projects/<project_id>/           one store set per repository: every clone and worktree
    project.json                   identity record; makes the directory self-describing
    journal.db                     Work, Workflow, handoff grants, scope sets, git-index tx,
                                   native integration, graph heads, idempotency ledger FULL
    memory.db                      facts and the retrieval anchors they cite, holographic
                                   banks, preferences, decisions, memory graph      FULL
    evidence.db                    observations, deflated bodies, cursors, observation
                                   anchors, git evidence, LCM summaries            NORMAL
    derived.db                     sessions, LCM index (contentless FTS5), temporal model,
                                   relation topology, projection cursors, dead letters  NORMAL
    worktrees/<worktree_id>/       code-index scope and sealed graph generations
    code/segments/                 content-addressed code-index segments, shared by worktrees
    spool/                         host-admission spool (bounded, format unchanged)
    scratch/                       seal staging; deleted after each seal and at every open
  projects/_profile/               memory, evidence, derived for sessions with no project
  remote/<node>/journal.db         Remote Brain replay spool (journal class)
  response-handles/                24 h TTL and a byte cap, swept every maintenance tick
```

The store key stays the existing `tracedecay_domain::ProjectId`, so the 657 files
that name it and the MCP `project_selector` keep their meaning. What changes is how
a `ProjectId` is minted and which checkouts resolve to it (section 2).
`WorktreeId` is the existing domain type.

### 1. Engine

SQLite through rusqlite holds every mutable byte. Three mutable Grafeo containers
move into SQLite tables: the project memory graph (into `memory.db`) and both
session-relation graphs (into `derived.db`). Memory belongs in SQLite on
durability grounds: Grafeo can swallow WAL append errors
(`graph-db/src/location.rs:42-45`), and memory is a FULL class.

Grafeo stays only as the file format of immutable sealed code generations, with
no WAL and no mutation. Its costs all came from the long-lived mutable staging
container: whole-container heap replay (an 8.6 GB container cost 20+ GB of heap),
WAL segments with no periodic checkpoint, and head-of-line staging release. That
container becomes per-seal scratch, deleted when the seal publishes. Whether
sealed generations later become SQLite packs (candidate 1) is an engine question
this redesign does not need to settle; the last cutover step measures it.

### 2. Unit of storage and sharing

- One file holds one storage class of one project. Nothing is ATTACHed.
- A project is a repository. Resolution, strongest first:
  1. The existing `ProjectId` of this checkout: its marker
     `<git_common_dir>/tracedecay-project.json`, read only when this profile holds
     the named project (today's `is_initialized_project_root` rule), so a marker
     written by another profile never steers this one.
  2. An alias match in `catalog.db`: the checkout's identity anchors are looked up
     as aliases of an existing project, and a match adopts that project.
  3. A new project, minted as today from the canonical root, with its anchors
     recorded as aliases.
- Identity anchors: every parentless commit reachable from HEAD by a full parent
  walk (all of them become aliases, so a later merge of unrelated histories adds an
  alias and never re-keys); for a shallow clone, the normalized remote URL; for an
  initialized subdirectory of a worktree (a monorepo subproject, which today's
  `discover_project_root` supports), the pair of root commit and relative path.
  On this host 7 of the 14 tracedecay clones have a local-path origin, 5 https and
  2 ssh, so only the root commit converges all of them; the 14 clones and 32 linked
  worktrees resolve to one project.
- A project never re-keys. An unshallowed clone, a first commit in an unborn
  repository, or a new root after a fetch adds an alias to the existing project.
  So durable memory is never stranded under an old id.
- `tracedecay project link <project_id>` and `tracedecay project split` record an
  explicit alias or refusal in `catalog.db`, scoped to this profile, for forks that
  should not share and unrelated repositories that should.
- A cwd outside any repository and not explicitly initialized belongs to
  `projects/_profile/`. It never mints a project of its own.
- Code indexes stay per worktree in `worktrees/<worktree_id>/`. Segments are
  content-addressed per project, which plan 40 decided and never built.

### 2a. Partitioning

Answer to "So maybe we need many databases not just 1 sessions db?": yes. Each
project gets four SQLite files by storage class, the profile gets a catalog and a
telemetry file, and all of them share one executor.

| Data class | File | Write cadence | Retention | Scope | Kind |
|---|---|---|---|---|---|
| Projects, checkouts, anchors, aliases, configuration revisions | `catalog.db` | low; a checkout row is written on first sight and refreshed at most hourly | forgotten projects' rows deleted; configuration kept | profile | durable |
| Hook events, usage history, findings, loss receipts | `telemetry.db` | per hook, per tick | events 30 d, usage 90 d, 256 MiB cap | profile | telemetry |
| Workflow definitions, native-integration approvals, scope sets | `journal.db` | low | kept until deleted by their owner | project | durable |
| Work runs, attempts, controls, placements, handoff grants, git-index tx, graph heads, idempotency ledger | `journal.db` | small transactions per request | terminal aggregates 90 d, grants at expiry, idempotency 7 d | project | authoritative |
| Facts and the retrieval anchors they cite, holographic banks, preferences, decisions, memory graph | `memory.db` | low | forever; never size-evicted; a finding past 2 GiB | project, `_profile` | durable |
| Observations, bodies, cursors, observation anchors, git evidence | `evidence.db` | hot append | 180 d window by capture time, 8 GiB per project, summarized rows evicted first | project, `_profile` | evidence |
| LCM summaries (DAG keyed by observation identity) | `evidence.db` | per summary | kept while any evidence of the session is kept, then a 365 d floor | project, `_profile` | not rebuildable |
| Sessions, LCM index, temporal model, relation topology, projection cursors, dead letters | `derived.db` | bulk, behind evidence | follows evidence; dropped and rebuilt only on corruption or schema change | project, `_profile` | rebuildable |
| Code-index scope, sealed generations | `worktrees/<id>/` | per publication | own code budget: active plus 2; idle heads evicted first; removed when the worktree root is gone | worktree | rebuildable |
| Segments | `code/segments/` | per publication | unreferenced sweep | project | rebuildable |
| Seal staging | `scratch/` | per seal | deleted after the seal and at open | project | ephemeral |

Each split removes one recorded coupling:

- `journal.db` leaves the sessions file, so a transcript fault (#4094, #3415) or
  WAL pressure (#4097) can no longer stall Work, git or grants. The split costs no
  cross-file atomicity: no Work, Workflow or handoff SQL touches an observation,
  LCM or queue table (grep of `rusqlite-runtime` `work*`, `workflow*`, `handoff*`,
  candidate 4's evidence).
- `memory.db` leaves the graph journal, so a schema change of either never resets
  the other, and the one never-evicted class lives in a small file that is cheap to
  check. Facts carry a foreign key to the retrieval anchors they cite
  (`memory_v2/schema/baseline.rs:114-127`), so those anchors live in `memory.db`
  too; observation anchors stay in evidence.
- `derived.db` leaves `evidence.db`, so the class the projection writes in bulk can
  be deleted and rebuilt without losing anything authoritative.
- `telemetry.db` leaves the catalog, so per-hook writes never share a FULL fsync
  lane with identity, and the catalog keeps one retention rule.
- Hot append and bulk derivation get separate WALs and writers. Separate files are
  the one write-concurrency lever the record measured (1.37x with four files,
  `benchmark_data` MEASUREMENTS.md of 2026-10-09).
- Deleting a worktree or a project deletes a directory.

LCM summaries live in evidence because they can be LLM-generated and are not
rebuildable, and raw eviction depends on them. Their DAG references evidence
identities (observation ids and sequence ranges), never derived row ids, so a
derived rebuild leaves them intact. Graph heads join the journal because head
compare-and-swap is a small transaction.

No transaction spans two files. Every cross-file relation runs producer to
consumer, and the consumer keeps its cursor in its own file: `derived.db` keeps
`projected_through(stream)`, and `journal.db` keeps graph heads over
content-addressed sealed files. "Projection keeps its effects, rows and checkpoint
in one transaction" (the July rule) holds inside `derived.db`. Because evidence
and derived are separate NORMAL files, a power loss can leave a derived cursor
ahead of evidence. Opening `derived.db` therefore compares each stream's cursor
with evidence and rewinds any cursor that is ahead; projection is idempotent by
observation id, so the rewind only re-projects.

This keeps ADR-003's rejection of one giant database. It replaces ADR-003's
profile-wide `activity.db` with per-project evidence and derived files: a
profile-wide file would make one 100 GB failure domain and one writer for all
ingest, and forgetting a project would become row deletes on it.

### 3. Durability and WAL ownership

- Durability per class, from one table (`StorageClass::policy()` in the sketch):
  FULL for catalog, journal and memory; NORMAL for telemetry, for evidence (it
  keeps its cursor in the same transaction as its rows and re-reads host files
  after a lost tail), and for derived (rebuildable).
- The executor is the only checkpointer. Every connection runs
  `wal_autocheckpoint=0` and `NO_CKPT_ON_CLOSE`. Checkpoints are always TRUNCATE.
- The checkpoint runs under a reader fence. The fence is total because three facts
  hold by construction: the daemon is the only process that opens these files; every
  read is an executor job with a deadline, so no snapshot outlives a job or crosses
  an `.await`; and the executor holds every connection's interrupt handle.
- Fence sequence, when the WAL passes the class soft limit (8 MiB for small
  classes, 32 MiB for evidence and derived): new reads of that file queue; in-flight
  reads finish or are interrupted when their deadline passes (measured from the
  moment a pool thread starts the job: 2 s for foreground classes, 5 s for
  background ones); TRUNCATE runs with zero readers; the fence drops; interrupted
  reads run again. The WAL peaks at the soft limit plus one batch.
- A file idle for 60 s gets a TRUNCATE and closes its connections.
- A fence lasts at most one read deadline plus the TRUNCATE. If TRUNCATE is still
  busy with zero executor readers, the reader is another process (a developer's
  `sqlite3` shell, say). The fence drops so reads resume, the lane records a
  finding naming the file and process, retries with backoff from 1 s to 60 s, and
  keeps accepting writes. Past 512 MiB of WAL that file's writes return `Busy`, so
  growth stays bounded. Nothing spins.

There is no hard-drain state, no 100 ms retry loop, no PASSIVE-or-RESTART
question, and no "pending, blockers: []". The #4097 ratchet has no state to live
in.

### 4. Execution class

- One class for every caller: async submit, oneshot reply. A handle method
  validates, builds a job over owned inputs, `try_send`s it to the file's lane
  (full means `Busy`), and awaits a oneshot. No storage API is sync, so nothing
  parks a native worker, a Tokio worker or a blocking-pool thread on SQLite.
- Jobs are re-runnable: `Fn(&WriteTx) -> Result<R>` and `Fn(&ReadTx) ->
  Result<R>` over owned or shared inputs, with no effect outside their transaction.
  That is what lets the executor run a job again after a fence interrupt or a
  reconnect. Batched writes take one savepoint per job, so a job's ordinary error
  rolls back only that job. A suspect-connection error rolls back the whole batch;
  each job of the batch then runs alone on a fresh connection.
- One fixed pool, `tracedecay-store-N` (default 6, clamp 2 to 16), serves every
  file. A lane is per-file state: a write queue (the existing deficit-round-robin
  `FairQueue`), a read queue, one write connection, at most two read connections,
  and fence state. Any pool thread can run any lane; at most one thread holds a
  lane's write connection at a time. One thread is reserved for foreground classes
  (catalog, journal, memory) so bulk derivation cannot starve Work. Open lanes are
  LRU-capped at 64 and the cap evicts only idle lanes. Thread count is N whatever
  the file count; today it is about 244 at rest.
- No interactive transactions. A write is one job, which cannot `.await` and so
  cannot hold the writer across a round trip; today an exact-SQL transaction can pin
  the writer for 120 s.
- Admitted writes run to commit or rollback even if the caller drops the future.
- Writes carry a 30 s statement deadline. Opens and WAL recovery run on a pool
  thread.
- The single sync door is `Executor::block_on`, for offline CLI threads that own no
  runtime. It refuses on pool, native and Tokio threads in every build profile.

Deleted by the end of the cutover: `dispatch_read`'s blocking-pool hop, the sync
`ExactSqlHandle` API, `recv_writer_reply`, `off_native_thread` around Work (#4052),
both native wait counters, and the git-index, native-integration and remote-replay
actor threads.

### 5. Corruption containment

Faults are classified by `rusqlite::ErrorCode` inside the engine, never by message
string. Schema code creates no TEMP tables.

| Fault | Detection | Blast radius | Recovery, no operator |
|---|---|---|---|
| Bad row in projection | typed projector error | one stream (provider, session) | retry with backoff; after 3 attempts the row moves to `dead_letters` in derived and the stream cursor advances past it with a recorded gap; a lane-wide outage never dead-letters (only a row that fails while other streams of the file project does); each dead letter is re-tried alone, without rewinding its stream, on a projector version change and once per daemon incarnation |
| Bad row in another write | savepoint error | one request | savepoint rollback, typed refusal |
| Process-lifetime connection or heap fault (#4094) | first `SQLITE_CORRUPT`, `NOTADB` or I/O error on a lane | one batch | drop every connection of that file and run each job of the batch again alone on a fresh connection; a pass means the fault was process-local and is only counted |
| Poison input | the rerun fails the same way and `PRAGMA quick_check(1)` passes | one row or one request | treated as a bad row: projection dead-letters it, any other write gets a typed refusal; the file keeps serving |
| Corrupt file | the rerun fails and `quick_check(1)` fails | one class of one project | derived and code: unlink and rebuild. Catalog, journal, memory, evidence: row-by-row salvage into a new file and an atomic swap, with a finding; evidence then re-ingests what host files still hold for the gap and writes a loss receipt. The salvage is a repair, not a backup: no copy is kept |
| Corrupt sealed generation | digest proof or latched read error | one generation of one worktree | discard it; the worktree re-seals from its code-index generation |
| Store unlinked under the daemon | each tick: `fstat(pinned).st_nlink == 0` or a path-inode mismatch | that lane | close the lane, which frees the space; reopen on the next job |
| Profile deleted or replaced | `profile-identity.json` unlinked or replaced | the daemon | `ProfileLost`: close everything and exit; the supervisor restarts on the new profile |

`quick_check` scans the whole file, so it runs as a background-lane job while that
file answers `Unavailable { reason: Recovering }`. Other projects and other classes
never see any of these faults.

### 6. Retention and bounded growth

- `Storage::maintain` owns every budget. One kernel per class owns that class's
  bytes, and each class has its own budget and its own lever, so pressure in one
  class never evicts another: code evicts non-head generations, then the heads of
  the idlest worktrees (rebuilt on demand); derived trims rows behind the evidence
  window; evidence evicts by capture time, summarized rows first; the journal
  prunes terminal aggregates; memory only raises a finding. A profile budget
  (default 64 GiB, memory excluded) is enforced by the class levers in that order,
  rebuildable classes first, and never by evicting evidence to pay for code.
- Measurement: each tick records `page_count * page_size`, freelist and WAL bytes
  per file, plus directory bytes for `worktrees/`, `code/` and `scratch/`, into
  `telemetry.storage_usage`. The history persists, so growth rate survives a
  restart, and doctor reads it with no size blind spot. Doctor's storage report and
  the #4099 measurement lane converge on this history.
- Cadence: every 10 minutes, time-sliced at 200 ms. A slice that reports has-more
  runs again at once at background priority. Retention covers every project in
  the catalog, mounted or not.
- Every file is created with `auto_vacuum=INCREMENTAL` and eviction batches run
  `incremental_vacuum`.

Collection never deletes durable data by inference. Every project directory carries
`project.json`. A directory the catalog does not name is re-registered from that
file, never deleted; a directory with neither loses only its rebuildable classes
and its `memory.db` raises a finding. Only `forget` deletes `memory.db` or durable
journal rows.

Each growth source from #4099:

1. Per-worktree stores never collect. Worktree data lives only in
   `worktrees/<id>/`, and its liveness is the worktree root's existence, never the
   shared `.git`. A project with no live checkout for 30 days keeps only its durable
   classes (memory and the durable journal rows); the rest is collected.
2. The SQLite hard-pressure drain. Impossible by construction (section 3).
3. Grafeo WALs. No long-lived mutable Grafeo container exists.
4. LCM payloads. Bodies are deflated blobs in `evidence.db`, stored once and
   committed with their row, so no orphan file can form.
5. Observation projection. No queue table exists; the queue is evidence beyond the
   consumer cursor. Dispositions and topology are derived and cascade with evidence.
6. Generation staging. `scratch/` is deleted after each seal and at every open.
7. `hook_analytics.jsonl` becomes `telemetry.hook_events` (30 d plus a row cap).
8. `response-handles` are swept every tick (24 h TTL, 512 MiB cap).
9. The writer idempotency ledger is pruned at 7 d of age in the journal, which ends
   the per-incarnation stranding. `maintenance/` inventory logs are deleted; the
   catalog holds that state. `host-components` keeps the two newest bundles.

### 7. Schema evolution

Each file carries `user_version` and a schema digest. The rule follows the class:

- authoritative and durable classes (catalog, journal, memory, evidence):
  forward-only, additive migrations in the crate, applied by the executor at open.
  A non-additive change copies into `<class>.next` and renames over the file; no
  copy is kept. No user-authored state is reset by a schema change.
- rebuildable classes (derived, code): a mismatch means unlink and rebuild.

Upgrade path for beta installs: `tracedecay wipe --stale` plus `tracedecay init`
remains the reset for anything this design does not carry. Durable classes are
carried, not reset. Because the store key stays `ProjectId` and the directory stays
`projects/<project_id>/`, each project converts in place the first time the new
binary opens it: `memory_v2_*` rows and the anchors they cite move from
`tracedecay.db` to `memory.db`, workflow definitions and native-integration
approvals move from `sessions.db` to `journal.db`, configuration revisions move from
`global.db` to `catalog.db`, and the old files are deleted after the copy commits.
Evidence is re-ingested from host files rather than copied. The conversion runs once
per file, is deleted two releases later, and is not a compatibility reader: no code
reads the old files after the conversion. The operator host was wiped on 2026-10-16
and needs none of it.

### 8. Public port surface

| Type | Public methods |
|---|---|
| `Storage` | `open`, `open_ephemeral` (tests), `project`, `profile`, `configuration`, `usage`, `maintain`, `reset`, `forget`, `shutdown` (10) |
| `Project` | `id`, `memory`, `sessions`, `work`, `code` (5) |
| `Configuration` | `get`, `set`, `history` (3) |
| `WorkJournal` | `get`, `list`, `transact` (3) |
| `MemoryStore` | `commit`, `current`, `as_of`, `lineage`, `get`, `list`, `search`, `related`, `contradictions`, `history`, `feedback`, `status` (12) |
| `SessionStore` | `capture`, `project_pending`, `summary_candidates`, `record_summary`, `search`, `load_session`, `cursor`, `dead_letters` (8) |
| `CodeGraph` | `head`, `begin_seal`, `publish`, `open_head` (4) |

That is 45 methods on 7 handle types, plus value types (`Checkout`,
`StorageError`, `StorageUsage`, `MaintenanceReport`, `StorageConfig`,
`ResetScope`) and the existing `ProjectId` and `WorktreeId`. `WorkJournal::transact`
takes a caller-supplied operation id and a re-runnable job over a `JournalTx`. The
ledger lookup runs first, so a retried operation that already committed returns its
recorded outcome. Inside the job, `JournalTx` loads and stores any journal
aggregate, so invariants that span rows (capacity admission, placement uniqueness,
run control across sibling attempts, fence epochs) hold in one transaction; the
decisions themselves are pure contracts functions over the loaded aggregates.

Today's census, all deleted: 19 store traits with 143 methods; 24 sync Work ports
with 77 methods; the kernel ports (`PhysicalRuntimeAttachment` 11, resolver 2,
publisher 1, `VerifiedGraphRuntimePortV1` 6, retained leases 7, probe 5 with 13
production implementations); the closed serde store protocol
(`RuntimeReadOperationV1`, the 15-variant `RepositoryWritePayloadV1`); and the SQL
DDL, WAL limits and corruption string matching exported from `tracedecay-store`.
The surface hides the engine, files, WAL, batching, execution class, retention,
identity and recovery.

### 9. Lifecycle

- `init`: resolve identity (section 2), record the checkout and anchors, queue the
  worktree's index. It opens no store file eagerly.
- `wipe`: a daemon request, `reset(project, KeepDurable)`. It closes the project's
  lanes and unlinks evidence, derived, `worktrees/` and `scratch/`, and prunes the
  journal to its durable rows. Identity, `memory.db` and the durable journal rows
  stay. `wipe --all` stops the daemon under the lifecycle lease and removes the
  profile, memory included.
- `projects forget`: `forget(project)`. It closes the lanes, unlinks
  `projects/<id>/` including memory, and deletes the catalog rows and aliases.
- `doctor`: reads `usage()`, findings, dead letters and loss receipts through the
  daemon and never opens a store. Without a daemon it reports file sizes by `stat`.
- `maintain`: idle checkpoints, budgets, class levers, worktree and project
  collection, usage history and the unlink probe.

## Synthesis decision

Four runners produced whole shapes: profile-wide class files (candidate 1, Opus),
per-repository class files (candidate 2, Opus), an event log with read models
(candidate 3, Sonnet) and today's files with a governor (candidate 4, Sonnet). A
Fable cross-judge scored them against a rubric the runners never saw: candidate 2
27/30, candidate 1 24, candidate 3 20, candidate 4 19.

Base: candidate 2 (one store set per repository, partitioned by class, on one
bounded executor). The judge and the orchestrator picked it independently. It is
the shape a maintainer can extend without breaking an invariant: each class is one
row of `StorageClass::policy()`, each file has one writer, one retention rule and
one corruption rule, and the public surface is 45 methods.

All four candidates converged on the executor model (a fixed pool, closure jobs,
no sync storage API, no interactive transactions), on fenced TRUNCATE with bounded
reads, on per-stream dead letters with connection-first fault classification, on
worktree liveness by root, on root-commit identity, on an unlink probe, and on
persisted usage with enforced class budgets. That agreement is the strongest signal
in the arena and those parts ship as written. The divergence was the unit of
storage and the graph engine.

Grafts:

- From candidate 4: the evidence that a journal split costs no cross-file
  atomicity; the clone census that ranks the root commit above the remote URL;
  adopting an existing project id through an alias instead of re-keying; the single
  sync door `block_on` that refuses on runtime threads; and its warning that the
  shared-pool rewrite is the riskiest code change, which shaped the cutover order.
- From candidate 3: the confirm ladder (drop connections, rerun, then
  `quick_check(1)`: a pass means poison input, a fail means a corrupt file), so one
  bad row never quarantines a healthy file; the normalized remote URL for shallow
  clones; no TEMP tables; contentless FTS5 so a body is stored once.
- From candidate 1: `ProfileLost` through an inode watch on
  `profile-identity.json`; idempotency pruned by age instead of incarnation; and
  SQLite code packs as the measured exit from Grafeo, not a precondition.
- From candidate 4 through the judge: a fence with a bounded duration, so a foreign
  reader degrades to a finding and a write ceiling instead of stalling reads.

Rejected:

- Candidate 1's profile-wide class files. One corrupt `views.db` would degrade
  every project, all ingest would share one writer, and forgetting a project would
  become row deletes on a 100 GB file.
- Candidate 3's framed log as the evidence authority. It is a second persistence
  format for the largest class with no measured reason to leave SQLite, and Zack's
  ruling stands without one. Its per-record checksum argument is recorded under open
  questions.
- Candidate 4's same-files layout. It keeps journals and evidence in one file and
  one failure domain, against ADR-003 and the incident record, and its own trigger
  rule concedes the journal split is cheap.
- Candidate 2's new `RepositoryId` and `repos/` directory. Reviewer A showed they
  duplicate `ProjectId` and `WorktreeId`, which 657 files and the MCP wire surface
  use; the store key stays `ProjectId` and only its minting changes.
- Time-segmented evidence files (candidates 1 and 3; the judge's top graft). With
  `auto_vacuum=INCREMENTAL` set at creation, row eviction returns pages to the
  filesystem, so segments would add query fan-out and a second retention mechanism
  for reclaim the incremental vacuum already gives. The judge dissents. The budget
  step measures reclaim time on a full evidence file; if eviction cannot keep pace,
  the evidence class splits into time segments inside `projects/<id>/` with no
  change to `SessionStore`.
- The judge's cutover order (the shared executor first). All three interrogate
  reviewers showed the executor's fence is total only after interactive
  transactions, exact-SQL snapshots and foreign online-backup readers are gone, so
  those steps go first (section "Cutover plan").

## Interrogate verdict

Three reviewers (Opus, Opus, Sonnet) attacked the synthesized package; 50 findings,
most raised by two or three of them. Every "act on" finding below changed this
document.

Act on, and the change made:

- The unregistered-directory sweep could delete `memory.db` when a catalog row was
  lost (A, B). Directories are now self-describing (`project.json`) and only
  `forget` deletes durable data.
- Evidence and journal were reset or unlinked on a schema change, destroying LLM
  summaries, history past host retention, workflow definitions and approvals (A, B,
  C). All authoritative and durable classes now migrate forward; corruption salvages.
- Facts cite retrieval anchors through a foreign key, so anchors could not live in
  evictable evidence (B). Cited anchors live in `memory.db`.
- `FnOnce` jobs made the reconnect-and-rerun ladder and fence reruns impossible,
  and a bad job failed its batch-mates (A, B, C). Jobs are re-runnable with
  per-job savepoints.
- `transition` ran `decide` before the idempotency check and covered one aggregate
  (A, B, C). `transact` checks the ledger first and loads any aggregate.
- The executor step came before the steps that make every read a bounded job (A,
  B, C). The cutover is reordered.
- A memory schema change was planned before the memory conversion existed (A).
  Memory durability is now the second step.
- The profile budget could evict irrecoverable evidence to pay for rebuildable code
  generations (A, B, C). Each class has its own budget and lever.
- Root-commit identity could re-key and strand memory, followed HEAD, let one
  profile's marker steer another, and dropped monorepo subprojects (A, B, C).
  Identity now adopts through aliases, never re-keys, scopes overrides to the
  profile and keeps initialized subdirectory projects.
- Separate NORMAL files could leave a derived cursor ahead of evidence after power
  loss (C). Derived rewinds such cursors on open.
- Per-request FULL catalog writes and mixed retention in the catalog (A). Telemetry
  moved to its own NORMAL file; checkout rows refresh at most hourly.
- Dead-lettering after an hour turned outages into gaps, and a requeue rewound a
  whole stream (A, C). Only row-specific failures dead-letter, and dead letters are
  re-tried alone.

Considered and kept: `quick_check` cost (it now runs on the background lane while
the file reports Recovering); fsync stalls holding a pool thread (measured in the
executor step). Noted: step 1's proof counter cannot see the wait it moves to the
blocking pool; that hop is intentional and the executor step removes it.

## Tradeoffs accepted

- We accept one large `tracedecay-storage` crate in exchange for one owner of every
  byte on disk.
- We accept losing cross-page snapshot consistency, because every read is a short
  job, in exchange for a total checkpoint fence. Pagination uses keyset cursors.
- We accept that forks sharing a root commit share a project until `project
  split`, in exchange for clones converging with no configuration.
- We accept partial derived search during a derived rebuild, in exchange for
  corruption and schema changes that heal without an operator.
- We accept a 30 s ceiling per write job; bulk work pages.
- We accept Grafeo as an immutable file format. This is not a second engine: it has
  no WAL and no mutation, like the code-index generation files.
- We accept a one-shot in-place conversion of durable rows for two releases,
  because "durable memory survives" forbids a reset of that class.

## Alternatives considered

- Profile-wide class files (candidate 1). Same interface depth. Loses on
  containment: a whole-file fault spans every project. Loses on reclaim: forgetting
  a project is row deletes plus vacuum.
- Event log as the only authority (candidate 3). Deep at the read side, but it adds
  a persistence format, rewrites capture, projection, LCM and temporal in one move,
  and still needs this executor, fence and budget machinery.
- Today's files with a governor (candidate 4). Smallest change and a flat port
  count (220 methods before, 223 after), so it gains no depth at the surface; it
  keeps the Work, git and ingest coupling #4094 and #4097 exposed.
- Keep thread-per-store and evict idle projects. Thread count still scales with
  active projects times files, which forces a false choice between partitioning and
  a thread bound.

## Cutover plan

Each step migrates every caller of what it replaces and deletes the old path in the
same wave. Every step runs Bazel clippy and rustfmt on touched crates, the touched
crates' unit tests, and `//crates/tracedecay:unit_test`, `:daemon_suite`,
`:mcp_suite` and `:session_suite`. The order makes every read a bounded job before
the executor's fence depends on it, and makes memory durable before any step
changes a file that holds memory.

1. Read boundary (#4085). The typed read becomes an awaited job:
   `StoreRuntimeClientLease::dispatch_read` and its seven production callers await
   it, and no read parks a native worker. Proof: a native task awaiting a typed read
   leaves the native reader-wait counter unchanged (a mutation that reads inline
   fails the test).
2. Memory durability. The stores that hold memory run at FULL; the memory tables
   get additive migrations instead of `ResetRequired` with their first schema
   change. Proof: those stores' writers report `synchronous=FULL` and the others
   `NORMAL`.
3. Growth stops now. Worktree liveness by root instead of the shared `.git`, a
   collector for unregistered directories that never deletes durable data,
   response-handle sweeps every tick, `hook_analytics.jsonl` rotation, age-pruned
   idempotency rows. Built on the #4099 measurement lane's persisted usage, not
   beside it. Proof: a deleted worktree's data is collected in one tick; a
   directory without a catalog row keeps its memory.
4. Merged into step 8 (see "Implementation reconciliation"): dead letters ship with
   the partition, where corrupt derived authority can be rebuilt instead of skipped.
5. Async journal. `WorkJournal::transact`, `get` and `list`; Work and Workflow
   decisions become pure contracts functions. Delete the 24 sync ports, interactive
   exact-SQL transactions, `off_native_thread` for Work and the three actor threads.
   Proof: the writer-wait counter reads 0, then both counters are deleted.
6. Bounded reads everywhere. Engine snapshots get deadlines; doctor and storage
   report read through the daemon; delete `sqlite_read_snapshot` and the remaining
   sync `ExactSqlHandle` reads. Proof: no read path outlives its deadline in a test
   that holds a snapshot past it.
7. Shared executor. Replace each attachment's writer and reader threads with lanes
   on the fixed pool, keeping `FairQueue`, batching and the ledger. Add fenced
   TRUNCATE, idle close, the unlink probe and `ProfileLost`. Delete the hard-drain
   state and the attachment-local blocker inventory; #4097 closes as superseded.
   Proof: a test opens 64 stores and asserts the thread count is N; continuous
   readers keep the WAL at or under soft plus one batch; p99 under `mcp_suite` load
   is not worse than today; deleting the profile under a running daemon gives
   `ProfileLost` within one tick with the descriptors closed.
8. Partition the sessions file and identity by repository. Split each project into
   journal, memory, evidence and derived; telemetry leaves the catalog; identity
   adopts through aliases. The in-place conversion of durable rows ships here.
   Per-stream cursors and dead letters replace the global queue: row poison
   dead-letters per stream, and corrupt derived authority (ghost or divergent
   provenance) rebuilds `derived.db` instead of being skipped. Delete
   `projection_queue`, `lcm-payloads/`, the string classifier, the store locator
   resolver and standalone registries. Proof: two full clones and a linked worktree
   resolve to one project with three worktree directories; converted facts,
   workflow definitions and approvals read back identically; a poisoned row
   dead-letters while other streams project; corrupt provenance triggers a derived
   rebuild and the stream resumes.
9. Graph narrowing. The memory graph and session-relation graphs move to SQLite
   tables; the staging container becomes `scratch/`. Proof: no `*.grafeo.wal`
   exists after a seal; heap after open is independent of generation size.
10. Class budgets and levers. Kernels per class, the profile planner, doctor
    findings. Delete `store_soft_budgets_bytes`, in-memory watermarks and the 64 MiB
    doctor cap. Proof: an ingest journey over budget keeps evidence within budget;
    code pressure evicts code, never evidence.
11. Graph engine gate. Measure SQLite code packs against sealed Grafeo on this
    repository (activation RSS, traversal p50 and p95). Keep whichever wins; delete
    the other.
12. Growth acceptance. Run a daemon on a fresh profile for 24 hours under a
    recorded ingest and code-index load, and read `telemetry.storage_usage`. Proof:
    every class stays within its budget, and the WAL of every file is at or under
    its soft limit plus one batch at each sample.

## What this supersedes

- Plan 02's "No asynchronous database API" and its interactive repository
  transactions: every storage API is async and every transaction is one job.
- Plan 38's "Store soft budgets are owner-configured and inert by default": budgets
  have defaults and are enforced by `Storage::maintain`.
- Plan 39's move of graph-shaped data off SQLite: mutable graph data (memory and
  session relations) lives in SQLite; Grafeo remains an immutable sealed format.
- Plan 40's monolithic per-worktree generation segments: segments are
  content-addressed per project.
- The roadmap's reset-only rule for released stores, for the durable and
  authoritative classes: they migrate forward; derived and code still rebuild.
- The 2026-07-15 fold of sessions into the project-wide store, which had no
  recorded reason: ADR-003's failure-domain split is restored by class.

## Implementation reconciliation

- Step 1 (2026-10-16, branch `refactor/storage-read-boundary`). The typed read runs
  as a job on the native blocking pool rather than on an executor lane, because the
  executor arrives in step 7; the public shape (`async fn dispatch_read` taking
  `Arc<dyn RuntimeRequestProbeV1>`, matching `dispatch_submit`) is the durable part
  and step 7 replaces only the body. The remote exact-observation query port is
  still a sync port running on the blocking pool (`tracedecay-api` remote routes),
  so it awaits the read through `native_runtime::block_on`, which refuses on a
  native thread; step 5 makes that port async. Accepted by the storage lane under
  the autonomy grant; the shared contract is unchanged.

- Step 2 (2026-10-16, branch `refactor/storage-memory-durability`). The stores
  that hold durable memory today, the project store (`tracedecay.db`) and the
  profile memory store (`user-memory.db`), commit at `synchronous=FULL`; every other
  store keeps `NORMAL`. `global.db` stays `NORMAL` although the catalog class is
  FULL, because it still takes per-hook analytics writes until telemetry splits out
  in step 8. The additive-migration rule for memory ships with the first change to a
  memory table (the conversion in step 8), not as machinery with no migration to
  run. The class contract is unchanged.
- Step 3 (2026-10-16, done). Four PRs stopped the growth this step names:
  - #4120. Orphan-store liveness no longer counts the shared git common
    directory, so a deleted worktree's store becomes collectable while its
    repository lives.
  - #4126. The maintenance tick runs one bounded page (8 directories, 5 s) of the
    unregistered-directory sweep, which only Doctor previewed before. The sweep
    still refuses a directory that holds memory rows or a payload it cannot
    inspect. Re-registering such a directory from a self-describing `project.json`
    needs the step 8 layout, so until then it stays whole.
  - #4128 (the #4099 lane). Every tick sweeps expired response handles, and a
    fully imported `hook_analytics.jsonl` over 64 MiB is truncated under its
    append lock.
  - #4129. Each commit retires the idempotency rows of an incarnation (a daemon
    generation) whose checkpoint on that shard has not moved for 7 days. The
    committing incarnation is never aged out. This departs from the literal "7 d
    of age". Pruning a live writer's rows by age needs an index on
    `committed_at_micros`. That index changes the exact final schema shape, and
    stores that hold memory have no migration path before step 8. Retiring
    exited incarnations ends the stranding the design names, and the age rule
    arrives with `journal.db`.
  Collecting the non-memory files of a store whose memory rows pin it moves to
  step 8, where memory gets its own file.
- Step 5 (2026-10-16, in progress). Four decisions:
  - The journal shape is "service inside the job". `ExactSqlHandle::transact`
    runs one whole write transaction on the writer thread as a job (#4133). A
    Work or Workflow operation runs its existing synchronous contracts decision
    inside that job, over a storage scoped to the job's transaction, and that
    storage holds no `ExactSqlHandle`. Effects outside storage (Git, providers,
    other stores) stay outside the job.
  - Shipped so far: handoff grants (#4133), pause and resume of a run (#4139),
    the git-index store, whose actor thread is deleted (#4140), placement and
    the leaf Work ports (#4142), native integration (#4143), remote replay with
    its actor thread deleted (#4157), the attempt chain and product mutations
    (#4158), and Workflow (#4235).
  - Workflow slice (2026-10-17). The Workflow ports are implemented on the Work
    job storage itself, the design's one `JournalTx`, not on a second job type.
    Effects, run-journal appends, artifact persists and census writes are writer
    jobs; a workflow effect (reserve, claim, apply, commit, reconcile) is one
    job, so a failure after the reservation now rolls the reservation back too.
    Every Work and Workflow dispatch now runs as a detached native task that the
    request awaits (`off_native_worker` is deleted), so a caller that stops
    waiting abandons only the wait and settlement outlives the request (#3711).
    The attempt task is an ordinary native task. Sync reads run through a
    `read` helper on the blocking pool, which step 7 replaces with a reader
    lane. Evidence retrieval stays whole on the blocking pool, because its
    contracts service interleaves sync Work reads with awaited session-evidence
    reads. Proof: a fan-out run driven from a native worker adds no native wait
    at any Work or Workflow storage call site.
  - `transact` has no operation-id ledger yet. The ledger is new schema on stores
    with no migration path, so it ships with `journal.db` in step 8. Until then
    each domain keeps its own idempotency.
  - Reads use `query_async` and preview/apply use `on_blocking_pool`. Both are
    blocking-pool hops that step 7 replaces.
- Step 5, job transactions (2026-10-18, #4244).
  The interactive transactions that remain become writer jobs without rewriting
  the async helpers that run inside them. `ExactSqlHandle::transact_async` takes an
  async body and polls it once, with a no-op waker, inside the job. The body's
  statements are synchronous, so a body that awaits only them completes on that
  poll. A body that awaits anything else returns `Pending`, and the job rolls back
  with `ExactSqlError::JobAwaitedForeignFuture` instead of holding the writer
  across the wait. That is section 4's "a write is one job, which cannot `.await`"
  as a test. The job transaction is now `Sync`, so `&ExactSqlJobTransaction` crosses
  the `Send` boundaries the helpers already require. It holds the writer task's
  transaction in a `Mutex` behind a trait object, which keeps the type covariant
  and adds no `unsafe impl`. `JobPolicy::AuthorizedLongLease` with
  `transact_with_policy` renews the lease after each successful statement, as the
  interactive long lease does, and `execute_batch` runs a batch inside the job.
  Existing `transact` callers are unchanged. Next, in order: runtime-core `WriteTx`
  over the executor traits with `Database::transact` and `transact_bulk`, then the
  call sites one crate per PR (runtime-core, global-db in five slices, sessions,
  session-memory, schema stages and invariants, `remote/*`, graph publication),
  then deletion of the interactive APIs in runtime-core and global-db, and last in
  rusqlite-runtime.
  - Slice 2 (2026-10-18, #4248). runtime-core's
    `WriteTx` implements `QueryExecutor` and `Executor` over the job transaction,
    so a helper written against `&impl Executor` runs inside a job unchanged.
    `Database::transact` and `transact_bulk` check the write scope at begin and
    advance `graph_transaction_generation` inside the job, as a committed
    `DatabaseWriteTransaction` does, and hold the writer lane so they queue
    behind interactive writers instead of retrying `Busy`. `set_metadata`, the
    retrieval-anchor disposition and derivative writes, and
    `execute_write_engine` and `execute_write_batch` run as jobs.
- Step 5i-3, global-db configuration and rollup writes (2026-10-18, branch
  `refactor/storage-global-db-config-jobs`). `RegisteredGlobalDb::transact` runs a
  body as one writer job over `Database::transact`, handing it a
  `RegisteredGlobalDbWriteTx` that implements the executor traits, so the existing
  `&impl Executor` helpers run in it unchanged. It checks the daemon write scope
  before queueing; the runtime verifies the same authority again at COMMIT, so a
  scope that ends mid-body rolls the job back. The configuration store, control and
  revision writes and the observability rollup rebuild, claim, release, frontier
  and compaction writes now use it. The interactive transaction stays for the other
  global-db sites, which move in the next slices. Bodies are `Fn`, so each site
  clones what it owns into the closure. A rollup rebuild that finds its stored
  receipt still ends by rolling back, so replay does not advance the transaction
  generation.
- #4138 (2026-10-16). Shutdown store close waits up to 3 s for leases to drain
  before deciding a store's WAL cannot be truncated. Under load, owners joined
  before store close could still be unwinding the tasks that held their leases,
  and `graceful_stop_truncates_profile_and_project_store_wals` failed. This is an
  interim remedy; the step 7 executor owns close.
- Step 6, first slice (2026-10-17, branch `fix/storage-bounded-hard-drain`). The
  hard-pressure drain gets the section 3 fence on today's writer, ahead of the
  executor, because the live #4097 incident (a 229 MB WAL pending for 6.8 hours
  through 245,264 retry wakes) cannot wait for step 7:
  - Past one read deadline (5 s) the fence revokes this pool's snapshots that are
    older than it: the worker interrupts any running statement, rolls the read
    back, and the snapshot's later reads fail as `SnapshotExpired`.
  - A fence lasts at most 6 s. Then whatever still holds the WAL is named (a
    reader by id, or another process by command and pid from `/proc`), the fence
    drops, writes resume, and the checkpoint retries with backoff from 1 s to
    60 s. Past twice the hard limit (512 MiB at the default) writes wait again,
    so the WAL stays bounded and nothing spins.
  - The expired fence publishes `CheckpointPressure::FenceExpired`, which status
    reports as `fence_expired` and Doctor reports as a `WalCheckpointPinned`
    storage finding naming the holders.
  Step 7 moves this policy onto lanes unchanged; only the 100 ms interval and the
  hard limit become the class soft limits.
- Step 6, snapshot lifetimes (2026-10-17, branch `refactor/storage-snapshot-deadlines`).
  Every read snapshot ends itself when it has sat 30 s with no statement running
  (`READ_SNAPSHOT_IDLE_LIMIT`) or 1 hour after it began (`READ_SNAPSHOT_LIFETIME`),
  whichever comes first. The reader worker rolls it back with the same machinery
  the fence uses to revoke one, and every statement on it also stops at the
  1-hour end. So a forgotten snapshot, or one held across unrelated work, frees the
  WAL within 30 s, while a legitimately long read of a 5.5 GB store (a convergence
  audit, a dbstat walk of 64-95 s, a paged rebuild) keeps working as long as it
  keeps issuing statements; each single statement keeps its own budget (30 s per
  request statement, none for convergence). A fixed 30 s total lifetime was
  considered and rejected: it would fail legitimate multi-statement reads on large
  profiles. The session temporal refresh dropped its verification snapshot before
  its commit phase, which it used to hold through. The design's 2 s and 5 s
  deadlines apply to read jobs, which arrive with the executor (step 7).
- Step 6, foreign readers (2026-10-17, branch `fix/storage-report-header-free-pages`).
  The storage report sized each registered store's free pages by opening its own
  read-only SQLite connection, a reader outside the daemon's pool on a live file.
  It now reads the free-page and page counts from the database header with a plain
  file read, which takes no lock and no WAL read slot. The open probe and its
  pragma helper had no other caller and are deleted. `sqlite_read_snapshot` stays
  for now: its remaining production uses read a private copy (the report's
  `global.db` rows) or a store the daemon does not mount (orphan collection's
  memory check), and neither holds a read slot on a live file. It goes with step
  8, where a project directory describes itself and memory has its own file.
- Step 7, shared pool (2026-10-17, #4242). Every
  attached file's writer and readers are tasks on one fixed pool of
  `tracedecay-store-N` threads, so the thread count no longer grows with the file
  count. N defaults to the host's available parallelism clamped to 6..16, not the
  flat 6 of section 4: with 6, the `//crates/tracedecay:unit_test` binary, which
  opens many stores at once in one process, timed out cold opens in 3 of 5 runs
  and ran 24% slower than master; with 16 it passed 3 of 3 and ran faster. A task
  holds a pool thread only while it runs SQL; idle waits, batch dwell, snapshot
  polls, busy-begin and WAL-recovery retries are awaits on the pool timer. A synchronous store wait issued from a pool thread
  (writer start, reply waits, reader acquire and snapshot calls, task join, drain)
  refuses with the existing unavailable errors in every build profile instead of
  parking. FairQueue, batching, the ledger, the checkpoint controller and the
  step 6 fence are unchanged. Deviations, each with the slice that closes it:
  - The override is `TRACEDECAY_STORE_THREADS`, not daemon configuration; it moves to
    `StorageConfig` with the executor's public surface.
  - No thread is reserved for foreground classes yet. Today's files mix classes
    (Work shares `sessions.db` with observation ingest), so a reservation by file
    has nothing to key on until step 8. Until then N statements running long at
    once delay every other store. Measured under host load at N=6, three runs
    each: `mcp_suite` 193 to 242 s on this branch against 224 to 227 s on master,
    `daemon_suite` 92 to 123 s against 97 to 124 s.
  - Fenced TRUNCATE on lanes and idle close are the next slices of this step.
  Proof: 64 attached stores add exactly N threads, all of them pool threads.
- Step 7, unlink probe and ProfileLost (2026-10-18, branch
  `feat/storage-unlink-probe-profile-lost`). Each writer checks its pinned file
  every 2 s, between batches and from its idle wait: `fstat` link count 0, or a
  path that is missing or names another inode, means the file is lost. The
  writer then fails its queued work as `Unavailable { reason: FileLost }`, drops
  its connection and pinned descriptor, and tells its attachment, which tells
  every reader to interrupt and exit and drops its own handles; none of this
  waits on a pool thread. Later submits and reads on that attachment answer
  `FileLost`. The registry unmaps a lost runtime on the next lookup or open of
  its key, so that open resolves and publishes a fresh runtime under the
  existing creation rules. The daemon pins `profile-identity.json` by inode at
  start and checks it every second from its accept loop; once the file is gone
  or replaced it logs `daemon_shutdown outcome=profile_lost` and leaves through
  the ordinary graceful shutdown, which closes every store, then exits with
  status 66 (`EX_NOINPUT`). The launchd unit restarts only after a failed exit
  (`SuccessfulExit=false`), so a zero exit would leave the daemon down; systemd
  uses `Restart=always` and restarts on either. Evicting a lost runtime also
  clears the profile pins recorded on its key, so a reopened key does not count
  them. Deviations:
  - Read-only attachments have no writer and so no probe. Their file stays open
    until the store closes.
  - A lost attachment dropped its reader pool at once and the registry dropped
    the runtime without waiting for the reader workers to exit. The next entry
    closes this (#4258).
  - Owners that hold a lease on the lost runtime (mounted project and profile
    owners) keep it, so they answer `FileLost` until a restart remounts them.
    #4130's reset-required record therefore stays, with its reason updated:
    the space is now released, and the restart only remounts those owners.
  - The watch runs on the Unix daemon only; the loopback daemon has no inode.
  Proof: an unlinked store reports itself lost and holds no descriptor on its
  files within one probe interval, and the next open serves a fresh store;
  deleting the profile under a running daemon logs `profile_lost` within one
  tick and the daemon exits with every store closed.
- Step 7b follow-ups, lost-file readers (2026-10-18, branch
  `fix/4258-lost-file-readers`, #4258). Closing a lost file now finishes its
  work. The reader pool's pinned descriptor is closed on loss for every holder of
  it, so a reader lease or retained snapshot that keeps the pool object alive no
  longer keeps the unlinked inode open. A reader worker that a growing acquire
  spawned while the pool was closing is shut down as it registers instead of
  idling until the pool drops. The attachment keeps the lost pool only until its
  workers have exited. The registry keeps a lost runtime mapped until then:
  its lookup answers `Evicting` and an open is refused with
  `RuntimeEvictionInProgress`, so no successor publishes beside connections still
  open on the old file. The next lookup or open after the last worker exits
  unmaps it as before. This is a short window, in practice a few milliseconds,
  and a worker stuck in native work holds it open, as a stuck close does for
  capacity eviction. The lost writer's own connection closes when its task
  returns right after the notice, which the next entry adds to the check
  (#4280). Proof: with the
  reader pool held across the loss, the unlinked file's descriptors are gone and
  `lost_file_closed` turns true; removing `close_for_lost_file`, the descriptor
  release, the late-registration shutdown or the registry hold each fails its
  own test.
- Step 7b follow-ups, lost writer and blocked acquires (2026-10-18, branch
  `fix/4280-lost-writer`, #4280). `lost_file_closed` now also waits for the lost
  writer's task to finish, because the writer's SQLite connection closes only when
  that task returns, after the loss notice has run. The attachment keeps the lost
  writer beside the lost reader pool and marks the file lost, moves both handles
  and closes the pool in one lock hold, so no check sees a lost file with nothing
  left to wait on. A caller blocked in the reader pool's acquire when the file is
  lost, or acquiring afterwards, now gets `FileLost` instead of `Draining`; the
  pool flips to draining and records the loss in one lock hold. Proof: with the
  writer task held after the notice and the readers already exited,
  `lost_file_closed` stays false and turns true once the task returns; removing
  the writer check or the `FileLost` reason each fails its own test.
- Step 8a, repository identity (2026-10-17, branch `feat/storage-repository-identity`).
  A second full clone, a shallow clone, and a checkout after a merge of unrelated
  histories open the project that first enrolled the repository, with no `init`.
  Anchors are rows in the existing `global.db` `project_aliases` table under the
  prefixes `git-root-commit:`, `git-subproject:` and `git-remote-url:`, beside the
  path and `git-common-dir:` rows. There is no `catalog.db`, no schema change, and
  no `tracedecay project link` or `project split`. Registration keeps the first
  enrolled checkout as `canonical_root`, never re-points an anchor, and caches
  each checkout's registration on its own. MCP scope admits a checkout outside the
  registered root when its own marker names the project. The daemon source binding
  digest names the project, so every checkout verifies one binding, and a binding
  under the old root-derived digest rebinds once. Deviations from section 2:
  - A shared root commit adopts only beside agreement on the remote: the
    checkout's normalized `origin` equals the `git-remote-url:` anchor an
    enrolled checkout of the project recorded, or its `origin` is a local path or
    `file://` URL that is itself an enrolled checkout. A shallow clone adopts on
    its remote alone. Forks, re-pointed templates and subtree-merged libraries
    therefore no longer share a project, so they need no `project split`.
    `ssh://`, scp-style and `https://` spellings of one host path normalize
    equal, and a `file://` URL equals its plain path.
  - A subproject's remote anchor carries its path in the checkout
    (`git-remote-url:<url>#<path>`), so two subprojects of one repository stay
    apart.
  - Anchors that name two projects adopt neither, and the checkout mints its own
    project instead of reporting a conflict.
  - Registration and lookup resolve in one order: a marker naming a store this
    profile holds, then path and `git-common-dir:` aliases, then anchors. Two
    clones enrolled under separate ids before this step keep both ids.
  - A marker naming a store this profile does not hold yields to the registry and
    is replaced by the id the profile resolves; one naming a held store is never
    rewritten. At first touch, when nothing else names the checkout, the profile
    enrolls under the marker's id.
  - The daemon's enrollment authority for a project admits another root when that
    root's own marker names the project, and withdrawing a registration removes
    only the roots it added.
  - The history walk runs at most once per checkout and HEAD in a process, on the
    blocking pool. Lookup walks only a checkout that no held marker, path or
    common-dir alias names; registration walks a named checkout only until its
    project records a history anchor. A root that a fetch or merge adds to an
    enrolled checkout is therefore recorded when a checkout no alias names (a
    fresh clone) registers.
  - Only the registered root writes `code_projects.git_remote_url` and
    `default_branch`. The GitHub source binding follows each checkout's own
    `origin`, which adopted clones share or which is a local path that binds
    nothing, so it does not flip between clones.
  - Two clones on one branch share the `profile_graph_scope_id(store, branch)`
    row but not graph data: the code scope carries each checkout's worktree id
    and repository id, so the scope id is unchanged. A test pins that two clones
    on one branch resolve distinct code scopes.
  Open: the lifecycle and the claim count a marker as held when its shard
  directory exists, the registry when a `store_instances` row names it. After an
  upgrade, a repository's anchors belong to whichever enrolled clone registers
  first. `branch add` of one branch from two clones overwrites that branch's
  `branch_meta.json` provenance. Code-index scope reconciliation counts as live
  only the lease repository's worktrees and mounted roots, so it collects a second
  clone's code-index scope once that clone has been unmounted for 7 days, and the
  clone re-indexes on its next open.
- Growth sources from #4099 still open, each with the step that closes it:
  1. LCM payloads that no summary covers yet: step 10's evidence lever.
  2. Session databases of unmounted projects get no retention: step 10, whose
     retention covers every project in the catalog.
  3. Observation projection history (active and unavailable skeleton rows):
     step 8 makes it derived and step 10 trims it behind the evidence window.
  4. The projection disposition ledger: step 8. Dispositions become derived rows
     that cascade with evidence.
  5. Generation staging release: step 9 (`scratch/`).
  6. The 512 MiB response-handle cap on top of #4128's TTL sweep: step 10.
  7. Hook events into `telemetry.hook_events`: step 8.
  8. Closing a store's lane when its file is lost, instead of a daemon restart:
     closed by step 7's unlink probe. Owners that cache the lost runtime's
     lease still need #4130's restart to serve it again.
  9. A 24-hour acceptance run on a fresh profile showing bounded growth: step 12.
- Step 4 merged into step 8 (2026-10-16). A dead-letter valve on today's global
  queue was built and then dropped: the projection code deliberately keeps corrupt
  projection authority (ghost or divergent provenance, divergent workflow state) a
  hard error, pinned by three `observation_collision_tests`, because skipping would
  launder corrupt derived state into a durable skip. The design's answer to corrupt
  derived state is a derived rebuild, which needs derived in its own file, so dead
  letters ship with the partition. This is one deviation, not a pattern of them, so
  the sketch stands (architect Phase E). The attempt is kept out of the tree; the
  remaining row-poison class (domain anchor errors) was too thin to ship alone.

## Open questions and risks

- Do dependency-bearing graph generations need staging rows? If so, the fallback is
  one scratch container per worktree that lives until the next cold seal and is
  collected with the worktree. Step 9 decides.
- Does a pool of 6 match today's p99 under `mcp_suite`, including an injected fsync
  stall? Step 7 measures it before it lands.
- Does per-stream projection fit `rebuild.rs`'s global frontier? Step 4 replaces
  that frontier.
- Is a per-record checksum worth adding for evidence (candidate 3's argument that
  SQLite has no page checksums)? SQLite's checksum VFS shim is the one-engine way to
  get it; it is deferred until a silent-corruption incident is on record.
- Are 64 GiB per profile and 8 GiB of evidence per project the right defaults?
  Step 10 calibrates them from the first weeks of persisted usage.

## Next implementation step

The rest of step 5: the native writer-wait counter becomes a refusal of any
synchronous writer wait on a native thread and both counters are deleted; then
the interactive exact-SQL transactions (56 direct sites and about 190 behind the
runtime-core async `Transaction`) become jobs. Step 7's first slice, writers and
readers as tasks on one fixed pool, is in progress in parallel.
