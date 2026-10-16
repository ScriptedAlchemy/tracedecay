# Candidate 2: one store set per repository, partitioned by storage class, on one bounded executor

## Problem

TraceDecay's storage is correct row by row and has almost no store-level mechanisms. Every 2026-10-16 incident follows
from that (ground/explanation.md, "Incidents, by mechanism"):

- One project `sessions.db` holds transcripts, the projection queue, LCM, Work/Workflow, handoff grants, git-index
  transactions and native-integration journals. A connection-local TEMP fault (#4094) froze ingest for 6 days, and the
  same file's WAL ratchet (#4097: 229 MB, 245,264 wakes) stalled Work and git.
- Each open SQLite file costs 1 writer + 3 reader threads, and mounted projects never close: 30 projects means about
  244 threads at rest.
- Callers pick one of four execution classes per call: async port with a sync `dispatch_read` inside the poll (#4085,
  2,683 native waits per `mcp_suite`), async engine, sync exact-SQL Work ports, and std-thread actors that `block_on`.
- Nothing bounds or measures bytes (153 GB, #4099). Retention runs 500 rows per day on mounted stores only. Orphan GC
  keeps a worktree store Live while the primary `.git` exists. Grafeo WALs grow for daemon uptime.
- A full clone gets its own identity because identity keys on the git common dir. The host has about 14 clones plus
  32 linked worktrees of one repository.
- Nothing notices when a store is unlinked under the daemon (112 GB of deleted inodes still held).

Constraints carried in: one SQLite engine (Zack 2026-10-12); daemon is the sole mutable authority (#480); settlement
outlives the request (#3711); no backups; durable memory is kept forever at full durability; projection never skips
ahead without a dead letter; durability is chosen per class; ADR-003's failure-domain split stands.

## Usage (caller's view)

The daemon owns one `Storage`. Everything else gets a `Repository` from it, then a class handle. Every method is
`async`, admits or refuses immediately, and never parks the caller's thread.

```rust
// Daemon start: opens catalog.db lazily, starts the fixed storage executor (N threads, default 6).
let storage = Storage::open(profile_root, StorageConfig::from(&config.storage)).await?;

// Any request with a cwd: identity is the repository, resolved once and cached in the git common dir marker.
let repo = storage.repository(&Checkout::discover(&cwd)?).await?;   // no file opened yet

// MCP Work handler (today: off_native_thread + 24 sync ports + interactive exact-SQL transaction).
// The decision is a pure contracts function; it runs inside one write closure on an executor thread.
let attempt = repo.work()
    .transition::<WorkAttempt, _>(attempt_id, move |current| work::request_cancellation(current, &cmd))
    .await?;

// Session refresh worker: capture into evidence.db, then project per stream into derived.db.
let sessions = repo.sessions();
sessions.capture(CaptureBatch { source, frames, cursor }).await?;
let pass = sessions.project(ProjectionBudget::page(256)).await?;     // dead-letters a poisoned row, never stalls
if pass.dead_lettered > 0 { tracing::warn!(?pass, "projection dead letters"); }

// Retrieval and memory.
let hits = sessions.search(SessionQuery::text("checkpoint ratchet").limit(20)).await?;
repo.memory().commit(fact).await?;

// Code graph of one worktree (code indexes stay per worktree).
let graph = repo.code(&worktree).await?;
let reader = graph.open_head().await?;          // mmap'd sealed generation, zero heap replay

// Maintenance tick (every 10 min, time-sliced): checkpoints, budgets, eviction, GC, usage history.
let report = storage.maintain(MaintenanceSlice::default()).await;

// Doctor and `tracedecay storage report`: sizes for every file class, budget, growth, findings.
let usage: StorageUsage = storage.usage().await;

// CLI wipe / projects forget are daemon requests; the executor closes the files before it unlinks them.
storage.reset(repo.id(), ResetScope::KeepMemory).await?;
storage.forget(repo.id()).await?;
```

Errors are domain-typed: `Busy { retry_after }`, `Unavailable { class, reason }`, `Refused(..)`, `NotFound`. No
SQLite text, error code, WAL number or wire enum reaches a caller. Corruption never surfaces as `Corrupt`; it surfaces as
`Unavailable { reason: Recovering }` while the class policy heals it.

## Shape

### Data layout

```
P/                                   profile root
  catalog.db                         repositories, checkouts→repo, configuration revisions, usage history,
                                     hook events, findings, loss receipts                  (profile, FULL)
  repos/<repo_id>/                   one store set per REPOSITORY (all clones + all worktrees)
    journal.db                       Work, Workflow, handoff grants, git-index tx, native integration,
                                     graph heads, idempotency ledger                       (FULL)
    memory.db                        facts, holographic banks, preferences, decisions, memory graph (FULL)
    evidence.db                      observations, bodies (inline blobs), cursors, anchors, summaries (NORMAL)
    derived.db                       LCM index (contentless FTS5), sessions, temporal model, relation
                                     topology, projection cursors + dead letters           (NORMAL, rebuildable)
    code/segments/                   content-addressed code-index segments, shared by worktrees
    worktrees/<worktree_id>/         code-index scope + sealed graph generations (immutable files)
    scratch/                         seal staging; deleted after each seal and at every open
  repos/_profile/                    same set for sessions/memory with no repository (no journal/code)
  remote/<node>/journal.db           remote replay spool (journal class)
  response-handles/                  TTL 24 h + byte cap, swept every tick
```

`repo_id = "repo_" + hex16(sha256(identity anchor))`. `worktree_id = hex16(sha256(canonical worktree root))`.

### 1. Engine

- **SQLite (rusqlite) for every mutable byte.** This honors the 2026-10-12 one-engine ruling and moves three mutable
  Grafeo containers into SQLite: the project memory graph (into `memory.db`) and the two session-relation graphs
  (`sessions.grafeo`, `user-sessions.grafeo`, into `derived.db` tables). Memory belongs in SQLite on durability grounds:
  Grafeo "can swallow WAL append errors" (`graph-db/location.rs:42-45`), and memory is the one FULL class. The
  holographic-memory note allows this ("bank storage placement flexible").
- **Grafeo stays only as the immutable sealed-generation file format.** Its measured win is mmap'd compact
  containers that open with zero heap (`sealed_store.rs`). Its measured costs all come from the long-lived mutable
  staging container: whole-container replay into heap (8.6 GB file → 20+ GB heap), WAL segments with no periodic
  checkpoint, and head-of-line staging release. In this design that container becomes per-seal scratch under
  `scratch/`. It is created for one seal and deleted when the seal publishes, so no Grafeo WAL outlives a seal.
- Rejected: replacing Grafeo with SQLite adjacency tables for the code graph. No measurement exists either way
  (plan 39 Task 12 is unchecked), and this redesign does not need that bet. Open question 3.

### 2. Unit of storage and sharing across worktrees

- **One file is one storage class of one repository.** One database is one file; nothing is ATTACHed.
- **Repository identity is independent of the git dir.** The anchor is the first-parent root commit of the remote
  default branch (`origin/HEAD`), else of `HEAD`. It is computed once with gix and cached as `repository_id` in the
  existing `<git_common_dir>/tracedecay-project.json` marker. Full clones, linked worktrees and moved checkouts all
  compute the same anchor, so the 14 clones and 32 worktrees on this host map to one store set. Fallbacks:
  - A repo with no commits keys on its path until its first commit; the path-keyed set then has no live checkout and
    is collected.
  - A non-git directory keys on its canonical path, as today.
  - `tracedecay project link --as <repo_id>` or `--split` writes an explicit override into the marker, for forks that
    should not share and unrelated repos that should.
- **Code indexes stay per worktree.** `worktrees/<worktree_id>/` holds the code-index scope and that worktree's sealed
  generations. Segments are content-addressed per repository, which plan 40 decided and never built.
- **Profile-wide data.** The registry, configuration and usage history live in `catalog.db`. Transcripts whose cwd
  matches no repository live in `repos/_profile/{evidence,derived}.db`, and profile memory in
  `repos/_profile/memory.db`. LCM payload files are gone: bodies are deflated blobs in `evidence.db`, stored once.

### 2a. Partitioning

Answer to Zack's question: yes, many databases. There are five classes per repository plus one catalog, and they all
share one executor.

| Data class | File | Write cadence | Retention | Scope | Kind |
|---|---|---|---|---|---|
| Repos, checkouts, aliases, config revisions | `catalog.db` | Low, per open/config | Rows of forgotten repos deleted; config forever | Profile | Durable (config) / rebuildable (registry) |
| Hook events, usage history, findings, loss receipts | `catalog.db` | Per hook / tick | 30 d events; 90 d usage; 256 MiB cap | Profile | Telemetry |
| Work, Workflow, handoff grants, git-index tx, native integration, graph heads, idempotency ledger | `journal.db` | Small transactions, per request | Terminal aggregates 90 d; idempotency 7 d; 512 MiB budget → prune oldest terminal | Repository | Authoritative |
| Facts, holographic banks, preferences, decisions, memory graph, memory anchors | `memory.db` | Low | Forever; never size-evicted; over 2 GiB is a finding only | Repository (+ `_profile`) | Durable |
| Observations, bodies, cursors, source presence, observation anchors, git evidence, LCM summaries | `evidence.db` | Hot append, every refresh pass | 180 d window + per-repo byte budget (8 GiB); summarized rows evicted first | Repository (+ `_profile`) | Evidence |
| LCM index, sessions, temporal model, relation topology, projection cursors, dead letters, read caches | `derived.db` | Bulk, behind evidence | Bounded by evidence (cascade) + 1.5× budget; drop and rebuild any time | Repository (+ `_profile`) | Rebuildable |
| Code-index scope, sealed generations | `worktrees/<id>/` files | Per publication | Active + 2 previous, 7 d; whole dir goes when the worktree root is gone | Worktree | Rebuildable |
| Segments | `code/segments/` | Per publication | Unreferenced sweep | Repository | Rebuildable |
| Seal staging | `scratch/` | Per seal | Deleted after seal and at open | Repository | Ephemeral |

Each split removes a recorded coupling:

- **Failure domain.**
  - `journal.db` leaves `sessions.db`, so a transcript fault (#4094, #3415) can no longer stall Work, git or grants.
  - `memory.db` leaves the graph journal, so a memory schema change never resets graph heads, and the reverse.
  - `derived.db` leaves `evidence.db`, so the class that the projection writes in bulk, and that held the corrupt TEMP
    cache, can be deleted and rebuilt without losing anything authoritative.
- **Write cadence.** Hot append (evidence) and bulk derivation (derived) get separate WALs and writers, which is the one
  concurrency lever the record measured (1.37× with four files, MEASUREMENTS.md). Small transactional journals no
  longer queue behind a 32-row projection window or a 6-hour temporal rebuild (the 2026-07-23 stall).
- **Retention.** Every file has exactly one retention rule, so the eviction kernel never mixes "forever" rows with
  "180 d" rows in one B-tree. Reclaiming derived space is a file unlink, not a vacuum.
- **Sharing scope.** Profile data lives in `catalog.db`, repository data in `repos/<id>/`, and worktree data in
  `worktrees/<id>/`. Deleting a worktree or a repository deletes a directory.
- **Merges, and why.**
  - Summaries join evidence: they can be LLM-generated (`LcmSummarizerMode::HermesAuxiliary`), so they are not
    rebuildable, and raw eviction depends on them.
  - Graph heads join the journal, because head CAS is a small transaction.
  - The external-source commit ledger (7 append-only tables, no deletes) is folded into `observations`. The
    observation row with its stable record id is the single commit record.
- **No cross-file transaction exists.** Every cross-file relation runs producer to consumer, and the consumer keeps
  its cursor in its own file:
  - derived keeps `projected_through(stream, seq)`;
  - journal keeps graph heads, and the sealed files are content-addressed.

  "Projection keeps its effects, rows and checkpoint in one transaction" (Preserve 7) holds inside `derived.db`.
- **Against ADR-003.** This keeps its catalog/activity/project split and its rejection of one giant database. It
  replaces the profile-wide `activity.db` with per-repository evidence/derived files:
  - a profile-wide file would be one 100 GB failure domain and one writer for all ingest;
  - deleting a repository would become row deletes instead of an unlink.

  ADR-003's argument that a session can touch several projects is met by attributing each session to the
  repository of its cwd, which is today's behavior.

### 3. Durability and WAL/checkpoint ownership

- **Durability per class, encoded in one table** (`StorageClass::policy()`):
  - FULL: catalog, journal, memory. The journal's receipt replay must survive power loss.
  - NORMAL: evidence, which is re-ingestable from host JSONL. Rows and cursors share one file, so a lost tail
    re-reads consistently.
  - NORMAL: derived, which is rebuildable. OFF was rejected because it can corrupt on OS crash and buys little in WAL
    mode.
- **The executor is the only checkpointer.** Every connection runs `wal_autocheckpoint=0` and `NO_CKPT_ON_CLOSE`.
  Checkpoints are always TRUNCATE, run under a **reader fence**. The fence works because of three facts that hold by
  construction:
  1. The daemon is the only process that opens these files. The 12 standalone registry sites are deleted, and
     offline commands only unlink files.
  2. Every read is an executor job: a closure with a statement deadline. No snapshot outlives a job, and no lease
     crosses an `.await`.
  3. The executor holds every connection's `InterruptHandle`.
- **Fence sequence.** When `wal_bytes ≥ class.soft` (8 MiB small classes, 32 MiB evidence/derived):
  1. New reads of that file queue; they do not fail.
  2. In-flight reads finish or are interrupted at their deadline (≤ 2 s) and retried after the fence.
  3. The checkpoint job runs `wal_checkpoint(TRUNCATE)` and releases the fence.

  So the WAL peaks at soft plus one batch. A "hard drain" state, the 100 ms retry loop, the PASSIVE/RESTART question,
  and "pending, blockers: []" all cease to exist; the #4097 ratchet has no state to live in.
- **Idle files are checkpointed and closed.** A file with no job for 60 s gets a TRUNCATE and its connections close,
  which also covers the idle 64.9 MB `global.db-wal`.
- **Settlement.** A write job, once admitted, runs to commit or rollback even if the caller drops the future (#3711).

### 4. Execution class

- **One class for every caller: async submit, oneshot reply.** A handle method validates, builds a `'static` closure
  over owned inputs, `try_send`s it to the file's lane (full means `Busy`), and awaits a oneshot. No storage API is
  sync, so nothing can park a native worker, a Tokio worker or a blocking-pool thread on SQLite.
  - `dispatch_read`, sync `ExactSqlHandle`, `recv_writer_reply`, `off_native_thread` (#4052) and both wait counters
    are deleted.
  - The git-index, native-integration and remote-replay actor threads are deleted with them.
- **Thread model: a fixed pool, `tracedecay-store-0..N` (default 6, clamp 2..16), shared by every file.**
  - A lane is per-file state: write queue (the existing deficit-round-robin `FairQueue`), read queue, up to two pooled
    read connections, one write connection, and fence state.
  - Any pool thread can run any lane. At most one thread holds a lane's write connection; rusqlite `Connection` is
    `Send`, and it is never used by two threads at once.
  - One thread is reserved for foreground classes (catalog, journal, memory) so bulk derivation cannot starve Work.
  - Sealed-graph queries run on the same pool, capped at N/2.
  - **Thread count is N regardless of file count**, against about 244 at rest today.
  - Open lanes are LRU-capped at 64 (about 600 fds). The cap evicts only idle lanes.
- **No interactive transactions.** A write is one closure, `FnOnce(&mut WriteTx) -> Result<R>`. It cannot `.await`, so
  it cannot hold the writer across a caller round trip; today an exact-SQL transaction can pin the writer for 120 s.
  Work logic that read, decided and wrote through 3 to 10 round trips becomes a pure decision function in contracts,
  run inside the closure (`transition`).
- **Bounded at the boundary.**
  - Per-lane queues hold 2048 ops / 16 MiB and refuse immediately when full.
  - Reads carry a class deadline and are interrupted when it passes.
  - Writes carry a 30 s statement deadline and roll back when it passes.
  - Opens and WAL recovery run on a pool thread; the caller awaits.

### 5. Corruption containment

| Fault | Detection | Blast radius | Recovery (automatic) |
|---|---|---|---|
| Bad row (projection) | Typed projector error | One stream (provider, session) for ≤ 3 attempts | Row moves to `dead_letters` in derived; stream cursor advances past it with a recorded gap; dead letters re-queue when `PROJECTOR_VERSION` changes; dropped with their evidence |
| Bad row (other writes) | Savepoint error | One request | Savepoint rollback, typed refusal |
| Process-lifetime connection or heap fault (#4094) | `SQLITE_CORRUPT`/`NOTADB` from rusqlite `ErrorCode` inside the engine, never from strings | One job | The lane drops **all** connections of that file (fresh heap and page cache) and retries the job once on a new connection. Only a second failure classifies the file. TEMP tables are banned from schema code, so no connection-local state exists to corrupt; the projection output-state cache is a `derived.db` table. |
| Corrupt file | Second failure on a fresh connection; `quick_check` on small classes | One class of one repository | Class policy: derived → unlink and rebuild from evidence. Evidence → refuse writes (capture pauses; host JSONL stays the source), confirm with `quick_check`, then unlink, reset cursors, re-ingest, and write a loss receipt. Journal → rename aside, recreate, fail in-flight aggregates with `JournalReset`, raise a finding; the debris is deleted after 7 d. Memory and catalog → salvage-and-swap (row-by-row copy through a fresh connection into a new file, then atomic rename), with a finding. The salvage is a repair, not a backup. |
| Corrupt sealed graph generation | Digest proof or latched read error | One generation of one worktree | Discard the generation; the worktree re-seals from its code-index generation |
| Store unlinked under the daemon | Each tick runs `fstat(pinned fd).st_nlink == 0` or a path-inode mismatch | That lane | Close the lane, which frees the space. If `profile-identity.json` itself is unlinked, the result is `ProfileLost`: close everything and exit, and the supervisor restarts into the new profile |

Other repositories and other classes never see any of these faults.

### 6. Retention and bounded growth

- **Ownership.** `Storage::maintain` owns every budget. One kernel per class owns that class's bytes: `EvidenceRetention`,
  `DerivedRetention`, `JournalRetention`, `CodeRetention`, `CatalogRetention`. A profile `BudgetPlanner` apportions a
  profile hard budget (default 64 GiB) across repositories by recency. Memory is excluded.
- **Measurement.** The executor already holds every file. Each tick records `page_count × page_size`, freelist and WAL
  bytes per file, plus directory bytes for `worktrees/`, `code/` and `scratch/`, into `catalog.db.storage_usage`.
  That history is persistent, so growth rate survives a restart. Doctor reads `usage()` and has no 64 MiB blind spot.
- **Cadence.** Every 10 minutes, time-sliced at 200 ms per slice. A slice that reports has-more runs again
  immediately at background priority. Retention covers every repository in the catalog, mounted or not; the executor
  opens an idle repository's file for the slice and closes it after.
- **At the budget.**
  - Evidence evicts oldest first: summarized rows, then all rows. The cascade deletes derived rows for the evicted
    sequences.
  - Derived first trims its FTS and temporal rows, then drops and rebuilds over a narrower window.
  - Journal prunes terminal aggregates.
  - Memory only raises a finding.
  - Every file is created with `auto_vacuum=INCREMENTAL`, and eviction batches run `incremental_vacuum`.

Each named growth source:

1. **Per-worktree stores never collect.** Worktree data lives only in `worktrees/<id>/`, and its liveness is the
   worktree root's existence (stat each tick), never the shared `.git`. The `live_git_common_dir_keeps_a_linked_worktree_store_live`
   rule is deleted. A repository with no live checkout for 30 days loses everything but `memory.db`, so memory never
   blocks collecting the rest. Any `repos/*` directory missing from the catalog is deleted at open and every tick,
   which is a daemon-owned collector for unregistered directories. `/fast/tmp` clones share the repository set, so
   they add worktree directories, not stores.
2. **SQLite hard-pressure drain.** Impossible by construction (§3): fenced TRUNCATE, no hard-drain state, idle
   close.
3. **Grafeo WALs.** No long-lived mutable Grafeo container exists. Sealed generations have no WAL, and scratch dies
   with its seal.
4. **LCM payloads.** No payload directory exists. A body is stored once, in `evidence.db`, and reference-counted by
   content hash. The orphan written before its transaction becomes impossible because blob and row commit together.
   Unsummarized rows are evicted at the hard budget with a loss receipt.
5. **Observation projection.** There is no queue table; the queue is evidence rows beyond the consumer cursor.
   Dispositions and topology are derived and cascade with evidence.
6. **Generation staging.** `scratch/` is deleted after each seal and at every repository open. Release has no head of
   line, because there is no staging container.
7. **`hook_analytics.jsonl`.** Replaced by `catalog.hook_events` (30 d plus row cap).
8. **`response-handles`.** Swept every tick (24 h TTL, 512 MiB cap), not once at MCP build.
9. **Other files.** The `maintenance/` inventory logs are deleted; the catalog holds that state. `host-components` keeps
   the two newest bundles. The idempotency ledger is age-pruned at 7 d in the journal, which ends the
   per-incarnation stranding.

### 7. Schema evolution

- Each file carries `user_version` plus a schema digest, and each class has one rule:
  - derived: mismatch → unlink and rebuild;
  - evidence: mismatch → unlink, then re-ingest whatever the host files still hold, with a loss receipt;
  - journal: mismatch → reset, with typed `JournalReset` for in-flight aggregates;
  - code: format mismatch → re-seal;
  - memory and catalog: the **only** forward-only, additive, in-crate migrations (`MIGRATIONS: &[Migration]`), because
    these classes are durable.
- **Beta upgrade.** The accepted reset (`tracedecay wipe --stale` + `tracedecay init`) is enough for everything except
  durable memory and configuration. The layout move from `projects/<id>/` to `repos/<id>/` would otherwise strand them
  in old `tracedecay.db`, `user-memory.db` and `global.db` files. So the design needs one more thing: a one-shot
  importer run by `init`. It copies `memory_v2_*` rows and configuration revisions into the new files, then deletes
  the old directory. It ships in cutover PR 6 and is deleted two releases later, so it is not a compatibility layer.

### 8. Public port surface

Public surface of `tracedecay-storage` (sketch.rs):

| Type | Public methods |
|---|---|
| `Storage` | `open`, `open_ephemeral` (tests), `repository`, `profile`, `configuration`, `usage`, `maintain`, `reset`, `forget`, `shutdown` (10) |
| `Repository` | `id`, `memory`, `sessions`, `work`, `code` (5) |
| `Configuration` | `get`, `set`, `history` (3) |
| `WorkJournal` | `get<A>`, `list<A>`, `transition<A>` (3, generic over 7 sealed aggregates) |
| `MemoryStore` | `commit`, `current`, `as_of`, `lineage`, `get`, `list`, `search`, `related`, `contradictions`, `history`, `feedback`, `status` (12) |
| `SessionStore` | `capture`, `project`, `summary_candidates`, `record_summary`, `search`, `load_session`, `cursor`, `dead_letters` (8) |
| `CodeGraph` | `head`, `begin_seal`, `publish`, `open_head` (4) |
| Value types | `RepositoryId`, `WorktreeId`, `Checkout`, `StorageError`, `StorageUsage`, `MaintenanceReport`, `StorageConfig`, `ResetScope` |

That is 45 methods on 8 handle types, plus 1 sealed trait (`JournalAggregate`, not implementable outside). Today's
census:

- 19 store traits with 143 methods;
- 24 sync Work ports with 77 methods;
- kernel ports: `PhysicalRuntimeAttachment` 11, resolver 2, publisher 1, `VerifiedGraphRuntimePortV1` 6, retained
  leases 7, probe 5 with 13 production implementations;
- the closed serde protocol (`RuntimeReadOperationV1`, 15-variant `RepositoryWritePayloadV1`, `deny_unknown_fields`
  wire enums);
- SQL DDL, WAL limits and corruption string matching exported from `tracedecay-store`.

Every one of those is deleted. Execution class, engine, files, WAL, batching, retention, identity and recovery are
hidden. What callers still see is the domain operation set, and the Work set shrinks most: 77 methods become 3
generic ones, because the decision logic moves into pure contracts functions (`work::request_cancellation(&state, &cmd)
-> Decision`). Those functions are domain logic, not port surface, and they are now testable without a database.

### 9. Lifecycle

- **init**: resolve the identity anchor, write or refresh the marker, upsert the checkout in the catalog, run the
  one-shot importer if the old layout exists, queue the worktree's index. Opens no store file eagerly.
- **wipe** (daemon request): `reset(repo, KeepMemory)`. Close the repository's lanes, unlink journal, evidence,
  derived, `worktrees/` and `scratch/`, and keep the identity and `memory.db`. Recreating them on the next open is
  intended, because they rebuild. **`wipe --all`** stops the daemon under the lifecycle lease and removes the profile,
  memory included.
- **projects forget**: `forget(repo)`. Close the lanes, unlink `repos/<id>/` including memory, delete the catalog
  rows, remove the marker's `repository_id`. The identity is now retired; a later open mints it fresh.
- **doctor**: reads `usage()`, findings, dead letters and loss receipts through the daemon. It never opens a store.
  Without a daemon, it reports file sizes by stat only.
- **maintenance**: `maintain(slice)` every 10 minutes: checkpoints of idle files, budgets, eviction cascade, worktree
  and repository GC, unregistered-directory GC, usage history, unlink probe.
- **Profile deleted while running (2026-10-16).** The unlink probe sees `st_nlink == 0` on the pinned
  `profile-identity.json` within one tick, closes every lane (releasing the 112 GB), and exits with `ProfileLost`.
- **The `code_projects` row that survives wipe.** `reset` keeps the identity on purpose, and `forget` removes it. The
  two commands now mean two different things, and both run through the daemon that holds the files.

### 10. Cutover plan

Each PR deletes what it replaces in the same wave. Gates for every PR: clippy `-Dwarnings` and `rustfmt_check_2024` on
touched crates, unit tests of touched crates, `//crates/tracedecay:unit_test`, `:daemon_suite`, `:mcp_suite`,
`:session_suite`.

1. **Executor under the existing attachment.** This is the riskiest unknown: pooled connections, the reader fence and
   idle close on today's files and schema. Replace `PersistentWriter` + `ReaderPool` threads with lanes on the fixed
   pool, keeping `FairQueue`, batching and the ledger. Add the fenced TRUNCATE, idle close, unlink probe and the
   per-file usage sample to telemetry. Delete the hard-drain state, PASSIVE path and attachment-local blocker
   inventory. **Proves**: a new test opens 64 stores and asserts the thread count is N; #4097's
   `hard_wal_pressure...` test is rewritten to assert the WAL stays ≤ soft + batch under continuous readers; an
   unlink test asserts the fds are released; all suites pass. #4097 is closed as superseded.
2. **Async read boundary.** `dispatch_read` and the sync `ExactSqlHandle` reads become async oneshots from the lane.
   Delete the sync read API and the `retained_fact_runtime` dual path. **Proves**: the native reader-wait counter
   reads 0 across the four suites (#4085 acceptance), and the counter is then deleted.
3. **Async journal.** Add `WorkJournal` with `transition`/`get`/`list` and seven aggregates. Move Work/Workflow
   decisions into pure contracts functions. Delete the 24 sync ports, `WorkSqliteStorage` interactive transactions,
   `off_native_thread` for Work (#4052), and the git-index, native-integration and remote-replay actor threads.
   **Proves**: the writer-wait counter reads 0 and is then deleted; contracts unit tests cover the decision functions
   with no database.
4. **Graph narrowing.** Move the memory graph and session-relation graphs to SQLite tables, turn the staging
   container into `scratch/`, and make layered generations depend only on sealed bases. Delete `GraphDbRegistry`
   owners and leases, WAL collapse, staging release and `corrupt_store` for mutable containers. **Proves**: daemon
   suite graph journeys pass; no `*.grafeo.wal` exists after a seal in a new test; heap after open is O(1) in
   generation size.
5. **Partition sessions.db.** Split it into evidence and derived, move the journal tables out, add the per-stream
   projection cursor and dead letters, ban TEMP, fold the external-source ledger in, and inline payload blobs. This is
   a reset of session stores only, which `wipe --stale` already covers. Delete `projection_queue`, `lcm-payloads/`
   and the string corruption classifier. **Proves**: in the session suite, a poisoned row dead-letters while later rows
   of other streams and of the same stream project; and an injected connection fault heals by reconnect with no file
   reset.
6. **Repository identity and layout.** Add the anchor-based `repo_id`, the `repos/<id>/` layout and `catalog.db`
   (replacing the `global.db` registry), worktree directories, the new orphan rules, and the one-shot importer.
   Delete `projects/<id>/` resolution, the `project_aliases` git-common-dir alias, the store locator resolver,
   standalone registries and `sqlite_read_snapshot`. **Proves**: a test makes two full clones and one linked
   worktree, asserts one `repo_id` with three worktree directories, and checks that a deleted worktree is collected
   in one tick; an import test preserves facts.
7. **Budgets and eviction.** Add class kernels, the profile planner, `catalog.storage_usage` history,
   `hook_events`, response-handle sweeps and doctor findings. Delete `store_soft_budgets_bytes`, in-memory
   watermarks and the 64 MiB doctor cap. **Proves**: #4099 item 3, where an ingest journey over budget trips a finding
   and the evidence size stays within budget.

## Module map

After the cutover there are about 45 crates, down from 51.

- **New: `tracedecay-storage`.** The executor and lanes, layout and identity, catalog, and modules `journal`,
  `memory`, `sessions` (evidence plus derived, projection, retention), `code` (scratch, publish, sealed open, code
  retention), `retention` (planner, kernels, usage), `fault`. It depends on domain, contracts, graph-db and rusqlite.
- **Deleted, with their contents absorbed into storage or deleted outright:**
  - `tracedecay-store`: wire enums, probes, SQL constants; its domain DTOs move to contracts.
  - `tracedecay-store-runtime`: session registry, resolver, writer gates, remote actors.
  - `tracedecay-rusqlite-runtime`: the writer and reader threads and exact SQL are deleted; `FairQueue`, connection
    policy and Work SQL move into storage; `native_runtime` moves to runtime-core.
  - `tracedecay-global-db`: the registered union schema and the observation adapter.
  - `tracedecay-session-temporal-store`: persistence moves into storage; temporal extraction becomes pure functions
    in session-runtime.
- **Merged:**
  - `tracedecay-lcm`: schema, payload and retention go to storage; summarization goes to session-runtime.
  - `tracedecay-code-index-retention` goes to `storage::code`.
  - `tracedecay-maintenance`: retention goes to storage, and the tick policy goes to the daemon.
- **Narrowed:**
  - `tracedecay-graph-db` keeps only seal, open and prove for the sealed format.
  - `tracedecay-runtime-core` loses `db/`, `shard_runtime/`, `storage/` and `sqlite_read_snapshot*`.
  - `tracedecay-contracts` loses every storage port trait and gains pure decision functions.
- **Composition.** `tracedecay-daemon-service`, `tracedecay` and `tracedecay-mcp` hold `Storage`/`Repository`
  handles. There is one composition root: `Storage::open` at daemon start.

## Synthesis decision

Filled in by arena.

## Tradeoffs accepted

- We accept one large `tracedecay-storage` crate in exchange for one owner of every byte on disk. Bazel incremental
  builds of that crate get slower.
- We accept losing cross-page snapshot consistency, because every read is a short job. In exchange, the checkpoint
  fence is total. Pagination uses keyset cursors.
- We accept that forks sharing a root commit share a store until someone runs `project link --split`. In exchange,
  clones converge with no configuration.
- We accept losing evidence older than the host's own transcript retention on an evidence reset. In exchange, there
  is no evidence migration code.
- We accept that derived search is unavailable or partial while a derived rebuild runs. In exchange, corruption and
  schema changes heal without an operator.
- We accept a 30 s statement ceiling per write closure. In exchange, no write can hold a lane longer. Bulk work must
  page.
- Grafeo stays as a file format. This might be mistaken for a two-engine design: it is an immutable format with no
  WAL and no mutation, like the code-index generation files.

## Alternatives considered

- **One store set per worktree** (today's failure mode taken further). Every worktree would carry its own copy of
  transcripts, memory and journals. The interface stays shallow because callers must know which worktree's store
  holds a fact, and GC has to tell 46 sets apart. Rejected by the incident itself.
- **One profile-wide database per class** (ADR-003 literal: `activity.db` for all evidence). It has the same small
  surface, but one writer serializes all ingest. A corrupt file loses every repository's evidence, and reclaiming a
  repository becomes row deletes plus vacuum on a 100 GB file. Its interface depth is equal; it loses on containment
  and reclamation.
- **Per repository, one file** (today's `sessions.db` plus `tracedecay.db`, made async). This is the smallest change.
  It hides as much behind the API, but keeps the coupling that #4094/#4097 exposed: one TEMP fault or one WAL stalls
  Work, git and ingest together, and one retention rule mixes forever and 180 d rows. ADR-003 stands.
- **Keep thread-per-store and add eviction of idle projects.** The thread count still scales with active
  repositories × files; per-file splitting multiplies it, which forces the false choice between partitioning and
  thread bounds.

## Implementation reconciliation

None yet.

## Open questions and risks

1. Can every Work/Workflow flow become decide-in-closure? Any flow that awaits git or the network between its read and
   its write must become optimistic: read, decide outside, then `transition` with a version check. PR 3 will find the
   flows that do this.
2. Is the first-parent root of `origin/HEAD` stable enough? Orphan-branch repositories and a later `--allow-unrelated-histories`
   merge could change it. The marker cache pins the first answer, but a fresh clone computes it again. Should a remote
   URL disagreement split the identity automatically?
3. Do dependency-bearing graph generations need staging rows? That is the PR 4 unknown. If they do, the fallback is
   one scratch container per worktree that lives until the next cold seal, collected with the worktree.
4. What else reads the external-source ledger (remote replay? privacy remediation?) that folding it into
   `observations` would break?
5. Does a pool of 6 threads match the p99 of thread-per-file under `mcp_suite` load? PR 1 must measure it, not assume
   it.
6. Is 64 GiB the right default profile budget, and 8 GiB per repository for evidence?

## Next implementation step

Write the `Lane` and `Executor` types from sketch.rs inside `tracedecay-rusqlite-runtime` behind
`RepositoryRuntimePhysicalAttachment`, and add the 64-store thread-count test plus the fenced-TRUNCATE WAL test
(cutover PR 1).
