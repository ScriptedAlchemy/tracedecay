# Candidate 4: same files, one governor

Tree: origin/master e5a8ede6ea. Companion: `sketch.rs`. Direction argued at full strength: keep every
file the profile has today and fix every 2026-10-16 incident inside that layout with the fewest
structural changes. I concede nothing on the file layout. I adapt three things that the evidence
forces: durability becomes per request (not per file), the sync Work/Workflow ports become async, and
`wipe --stale` stays a whole-file reset until the first GA schema change (named in 7).

## Problem

Every incident of 2026-10-16 traces to a missing store-level mechanism, not to the number of files:
nothing bounds bytes; idle stores never checkpoint; every open SQLite file costs 4 to 10 threads
(30 projects ~ 244 threads at rest); reads and sync ports park native workers (2,683 waits per
mcp_suite run, #4085); one fault freezes a whole file (a queue head, a WAL reader slot, a Faulted
writer); and nothing re-checks the path after attach (112 GB held by unlinked files).
Constraints I honor: SQLite only (2026-10-12); daemon is the only mutable authority (#480); settlement
outlives the request (#3711); durable memory is forever at full durability, never size-evicted; no
backups; identity is the repository; the projection drain gets its dead-letter path; durability is
decided per class; the writer ledger, runtime-core async `Connection`, sealed generations and spool are
right and stay.

## Usage (caller's view)

Callers see the ports they see today. The only change is that every method is `async`, and nothing
else can block.

```rust
// daemon-service: was sync, wrapped in off_native_thread (#4052). Now just awaited.
let attempt = work_service.start_attempt(req).await?;           // WorkAttemptStoragePort, async

// FactStore / ObservationStore: was async-shaped but called sync dispatch_read inside the poll.
let head = observations.next_queued_observation().await?;        // no native wait, counter stays 0

// A storage implementer (the only code that sees the executor):
store.write(WriteKind::Journal, move |tx| {                       // whole txn runs on a pool worker
    tx.execute(&stmt)?; Ok(row)
}).await?;

// An offline CLI thread (no runtime): the one sync door.
let id = executor.block_on(lifecycle.init(root))?;               // WrongThread error on a pool/native thread

// Operator: one place to look.
tracedecay storage report   // StorageLedger::health(): sizes by class incl. Grafeo dirs, WAL phase,
                            // dead letters, parked streams, detached stores, findings
```

## Shape

### 1. Engine

SQLite via rusqlite, unchanged. Grafeo stays as the code-graph and session-topology engine. Evidence
for keeping Grafeo: (a) every Grafeo byte is rebuildable (graph rows from sealed generations and
code-index files; session topology from the relational rows), so it never holds the only copy of
anything and its failure domain is already separate; (b) replacing the graph with SQLite tables is a
multi-month rewrite of the riskiest subsystem, and no comparison exists (plan 39 task 12 is unchecked),
so the evidence does not support starting it; (c) Zack's "one engine" ruling was about Turso vs SQLite.
Its real costs are heap residency (8.6 GB container ~ 20 GB heap) and an unbounded WAL directory; both
are fixable inside it (section 6, items G1/G2). Reopen the choice only if PR 9 measurements show heap
above 2x container size after hibernate is wired. That is a trigger, not a hedge.

### 2. Unit of storage and worktree sharing

Unchanged files. One project id per **repository**. One store per project. 30+ worktrees and ~14
clones map to one `projects/<id>/`; only code indexes (`code-index-v1/<sha256(worktree root)>/`) and
`Code{Worktree}` graph namespaces are per worktree. Profile-wide data (registry, observation log for
unscoped sessions, LCM payloads, profile memory) lives in the four profile files of the table in 2a.

**Clone convergence.** Evidence from this host: of the full clones under `/fast/tmp`, 7 tracedecay
clones have `origin = /fast/projects/tracedecay` (a local path), 5 have the https URL, 2 have the ssh
URL, and 7 more repos have no remote at all. The raw URL string converges 5 of those 14; normalising
ssh/https gets 7; the 7 path origins need the origin repo's own key; a root-commit key converges all 14
and the no-remote ones too. So:
`RepositoryKey = Marker | RootCommit | RemoteUrl`, strongest first (sketch section 5). On first open of a
clone with no marker: compute keys, look them up in `project_aliases` (new alias kind `repo-key:`),
adopt the existing `proj_*` id, write the marker into that clone's `.git`. The mint function is
unchanged (`default_profile_project_id`), so no existing id moves and no live store is orphaned.
Root commit is cached in the marker (one `rev-list --max-parents=0` per clone, ever). Shallow clones
fall back to the normalised remote. Two forks sharing history share a project: acceptable and arguably
right (same facts, same repository). An `explicit identity file` is already the marker; it always wins.

### 2a. Partitioning (required): the case against ADR-003 and for "one sessions.db per project"

| Data class | File | Cadence | Retention | Scope | Rebuildable / authoritative |
|---|---|---|---|---|---|
| Registry, aliases, config revisions, analytics rollups | `global.db` | low; per hook rollup | analytics 30 d (was 180), observability 30/395 d, registry rows forever | profile | authoritative, re-initable from manifests + git markers |
| Profile memory facts | `user-memory.db` (+ `.grafeo` index) | per fact write | forever, never size-evicted | profile | authoritative; graph index rebuildable |
| Profile-scope sessions (evidence, LCM, temporal) | `user-sessions.db` | hot append | budget 8/16 GiB, age rules (section 6) | profile | evidence + derived |
| Project memory facts + graph-publication journal | `tracedecay.db` | small txn | forever (memory) / by liveness (journal) | repository | authoritative |
| Project sessions: observations, projection queue, LCM, temporal, dead letters | `sessions.db` tables | hot append | budget 8/16 GiB, age rules | repository | evidence + derived |
| Work, Workflow, handoff grants, git-index tx, native-integration journals | `sessions.db` tables, `WriteKind::Journal` | small txn per request | terminal runs 180 d | repository | authoritative |
| Code graph, memory graph index, session topology | `*.grafeo`, `tracedecay.sealed/`, `graph-replay/` | per publication | superseded gens dropped; cold project rebuilds | repository (namespaces per worktree) | rebuildable |
| Code-index generations | `code-index-v1/<sha256(root)>/` | per publication | 32 gens / 8 GiB / 7 d per scope; project cap | worktree | rebuildable |
| Payload files, spool, handles, logs | sibling dirs / JSONL | per message / hook | bounded or rotated (section 6) | repository / profile | evidence copy / transient |

The ADR-003 argument, with the incidents that motivated "many databases" taken one at a time (does a
split close it, and what closes it instead):

| Incident | Mechanism | Would a split close it? | What closes it here |
|---|---|---|---|
| 07-23 6 h rebuild stalled the API | reads funnelled through the writer path | no longer applies: S11 reader/writer split shipped; remaining write-vs-write contention is a turn-length problem | `TurnBudgets` cap every write turn (2 s bulk), one maintenance turn daemon-wide |
| #3415 / #4094 drain frozen 6 days | strict global queue order + fail-closed + string-matched "corrupt" | no: the head is still the head in any file that holds both log and projections | per-stream dead letter + typed fault supervisor (3, 5) |
| #3984 18 GB git-attribution retries | no write bound | no: three files, same unbounded rows | `ByteClass` budgets + `Saturated{Budget}` (6) |
| #4097 229 MB WAL, 245k wakes | PASSIVE under overlapping readers | partly (blast radius smaller) but the WAL still ratchets in whichever file is hot | checkpoint ladder (3) |
| #4100 24 h grant freeze | `handoff_open_grants_v1` behind the same writer and the hard drain | partly | no hard drain exists; no interactive txn occupies a writer (3, 4) |
| 153 GB | growth, not co-location | no | the ledger (6) |

Costs a split would add, from source: capture writes `observations + queue + cursor + ledger` in one
txn and projection writes `sessions + lcm + dispositions + checkpoint + queue delete` in one txn; the
cut lines Zack's question implies (log vs projections) run through exactly those txns, so a split
needs the outbox/watermark protocol the July plan specified (its own listed risk 1). It also adds a
WAL, a checkpoint lane, a schema installer, a GC rule and a doctor row per file per project. The 10-09
measurement gave 1.37x throughput for four shard files with a worse p99 (38.7 to 76.3 ms vs 24.6 to
29.4 ms). ADR-003's backup harm is moot (no backups); its privacy harm has no owner today.

What I concede and what it costs. The strongest evidence for a split is narrow and real: Work/Workflow
SQL references no observation, LCM or queue table (grep of rusqlite-runtime work*, workflow*, handoff
finds none), so a `journal.db` split would cost zero cross-file atomicity, and under a shared executor
a file costs zero threads. Two things remain true of today's co-location after every fix below:
(i) a `wipe --stale` of `ProjectSessions` loses Work/Workflow rows, and (ii) a physically corrupt
`sessions.db` takes the journals with it. Neither has fired on disk: the only on-disk corruption on
record (#480) was multi-process writers, now excluded by daemon ownership; #4094 was in-heap. I
therefore do **not** split now. The decision rule is written down: split `journal.db` out in a later
mechanical PR if any of (a) a `Journal` write p99 queue wait above 250 ms over a week, (b) a second
session-schema reset that costs live Work state, (c) one on-disk corruption of a Sessions file.
Everything needed (the `FileClass`/`WriteKind` seam) is built regardless.

### 3. Durability and WAL/checkpoint ownership

Durability by class, per **request** (fixed input 5): `FileClass::durability(WriteKind)`. Registry,
Memory, RemoteSpool: FULL always. Sessions: `Journal` and `Control` FULL, `Evidence` and `Maintenance`
NORMAL (evidence is re-read from host cursors; the cursor advances in the same txn). A turn never mixes
durabilities; the writer sets `PRAGMA synchronous` between turns (it is a connection setting, legal
outside a txn). This is why one Sessions file can honestly hold both classes.

Owner: the store's own writer turn, and nothing else. Ladder (`next_wal_phase`, pure and unit-testable):
PASSIVE at 32 MiB after a turn; at 128 MiB (or three incomplete PASSIVEs) close the **read gate** and
attempt non-blocking RESTART/TRUNCATE every 250 ms; idle store: TRUNCATE after 5 s without work (the
missing idle timer); TRUNCATE at eviction and shutdown. No `hard_drain_required`, no `BlockGeneral`, no
100 ms retry forever.

Why the ratchet is impossible by construction. Two invariants: **R**: no read snapshot outlives
`TurnBudgets::read` (5 s default, 30 s ceiling). `StoreHandle::read` takes a closure, so a snapshot
cannot be leased across an await or a round trip, and the progress handler (already used for
`should_cancel`, connection/mod.rs:1131) interrupts at the deadline. **W**: all connections to a file
are created by one `StoreCore`, so the reader set is known. Given R, a closed gate empties the reader
set within `gate_window` (35 s); then RESTART succeeds and the next commit restarts the log. PR #4097's
trace (health reader admitted at the newest frame, PASSIVE "complete", drain lifts, re-arms) cannot
recur: PASSIVE is no longer the only mode, and the health reader is deleted (health is fstat plus
in-memory counters, no SQL). If a foreign reader (another process, the sqlite3 CLI) still holds a slot
after the window, the phase is `Stuck`: the gate **opens**, writes were never blocked, a finding names
the file, one retry per 60 s. Bounded and visible, not a drain. WAL bytes also feed the file budget (6).
The test that proves it: PR 2 reproduces #4097 (overlapping readers + steady writes) and asserts WAL
< 1 MiB within `gate_window`.

### 4. Execution class

Model: **actor per file on one shared, bounded FIFO pool.** Each `StoreCore` has a bounded mailbox and a
`scheduled` flag; a ready queue feeds W writer workers; reads run on R reader workers against a
per-store connection set (<= 2 warm, closed after 30 s idle). Threads total = W + R + 1 governor
(<= 13 at default sizing) regardless of file count, versus 4 to 10 per store today. Connections are
leased to workers per turn (rusqlite `Connection` is `Send`; SQLite is serialized-mode in the bundled
build).

Boundary: every port method is `async`; the future is a oneshot receiver, so it needs no ambient
asupersync `Cx` (today `dispatch_read` is sync while `dispatch_submit` demands a `Cx`). Every closure
runs entirely on a pool thread; a native worker only ever awaits a oneshot. The interactive
`begin_immediate` pattern (writer thread waiting `recv_timeout(25 ms)` between caller round trips,
<= 120 s) is deleted: the closure is the transaction. Bounded: per-store ops/bytes and a global byte
semaphore are enforced (today `global_queue_max_bytes` is validated then only reported); overflow is
`Saturated`, never a block. `StoreExecutor::block_on` is the single sync door and returns `WrongThread`
on pool or native threads in every build profile. Sync ports flip mechanically (`WorkAttemptStoragePort`
et al. keep names and method sets), `off_native_thread` wrappers (#4052) and `recv_writer_reply`
parking disappear, and the three std actor threads per project (git-index, native-integration,
remote-replay) become plain async stores (1.8 kLOC of `block_on` actors deleted).

### 5. Corruption containment (no operator action unless the file is authoritative)

| Fault | Blast radius | Detection | Recovery |
|---|---|---|---|
| Bad row / page | the caller that touched it; for a queue head, that **stream** only | corrupt-like result code reproduced on a fresh connection (`ConnectionLedger`), not message text | caller gets `DataCorrupt{Page}`; drain parks the stream after 3 attempts over >= 60 s |
| Process-lifetime connection/heap fault (#4094: corrupt in-heap TEMP cache) | one store, one op, no durable change | first corrupt-like code on connection g with no reproduction | drop the connection (TEMP tables, page cache, mmap die with it), return `ConnectionReset`, next turn opens g+1; a success clears the mark. Never counted as a drain failure |
| Corrupt file (canary `sqlite_master` + page 1 fails, or 3 distinct ops fail in 10 min) | that file only | supervisor canary | Sessions/derived: close, delete family (no copy kept), recreate, reset source cursors so host JSONL re-reads. Registry: freeze, rebuild from manifests + markers. Memory/RemoteSpool: freeze that file, finding; the one operator-visible case |
| Corrupt graph generation | that generation / projection | existing digest proof and corruption lock | existing quarantine + replay from sealed / code-index; the terminal `Faulted` registry slot gains a cooldown (60 s doubling to 1 h) and retries via the next open, so it is no longer daemon-lifetime |
| Writer fault (panic, checkpoint driver error) | that store | existing `WriterState::Faulted` | poison + reopen with cooldown; 3 in 10 min escalates to the file row above. Today nothing ever restarts it |

Dead letter (fixed input 4, sketch section 3). Ordering is per **stream** (provider, session), which is
what the July rule actually needs: "no checkpoint advances over a gap" holds inside a stream, and a
parked stream's checkpoint does not move. After `max_attempts = 3` over `min_span = 60 s` with a
non-`Contract` fault, one txn parks the stream (`projection_dead_letter`: head, class, attempts,
`parked_rows`, `redrive_after`) and the pass continues with the next stream. Redrive: daily and on
projector-version bump, reusing the version-rebuild replay restricted to one stream. Worst-case ingest
freeze for a poison message: one session for about a minute, not a file for six days. The queue-head
conditions in `projection_drain.rs:46-83` (predecessor step, external-source head) park their own stream
the same way instead of aborting the drain.

### 6. Retention and bounded growth

Owner of **measurement and budget declaration**: `StorageLedger` (tracedecay-maintenance, existing
crate). Owner of **shrinking**: the existing retention kernels, now driven by `BudgetFinding`s (data,
not a new trait). Measurement: every 60 s, `fstat` on each pinned fd (db + WAL, any size; the 64 MiB
doctor cap disappears) plus cached directory walks every 5 min for Grafeo (container **and** WAL dir
summed, fixing the `metadata().len()` on a directory), sealed, replay, code-index, payloads, logs.
Samples persist in `storage_samples` (30 d) so restarts do not re-baseline. Defaults (placeholders to
be recalibrated from the first week of samples, which is exactly what #4099 lacked): project
`sessions.db` and `user-sessions.db` 8 soft / 16 GiB hard; `global.db` 0.5 / 2; `tracedecay.db` 1 / 4
(Durable: findings only); graph family per project 10 / 20; code-index per project 16 / 32; payloads 4 / 8
per sessions DB; profile total 64 GiB. At hard: Evidence prunes then `Saturated{Budget}` on capture
(host JSONL is untouched, so refusal loses nothing); Rebuildable drops the coldest project's graph and
code-index first; Durable only reports.

Growth sources from `growth-sources-4099.md`, each answered:

1. **Per-worktree project stores never collect.** Cause removed upstream: clone/worktree convergence (2)
   stops minting them. Liveness redefined (`classify_and_plan`): Live iff a **registered worktree root**
   exists and resolves to this repository; `git_common_dir` is no longer an identity root (the test
   `live_git_common_dir_keeps_a_linked_worktree_store_live` is inverted for stores whose roots are all
   gone and whose RepositoryKey is held by another store: `Superseded`). Unregistered `proj_*` dirs get a
   collector in the daemon tick (7 d without writes, page 64 per tick, hourly). `memory_*` rows keep
   **only** `tracedecay.db`; sessions, graph, code-index, payloads are removed. `/fast/tmp` needs no
   special case: liveness is path existence. Hooks stop creating `projects/<id>/` (they resolve through
   the registry, not the marker alone). `projects forget` stays the only memory delete.
2. **Hard-pressure drain pends forever.** Deleted (3). Reader revocation is invariant R; escalation is
   the ladder; there is no state that retries PASSIVE every 100 ms.
3. **Grafeo WALs without checkpoint.** `collapse_replayed_wal` (recovery.rs:215-234) already calls
   `database.wal_checkpoint()` on a `&GrafeoDB`. The ledger timer calls it on the serving engine when the
   WAL dir exceeds 512 MiB or 6 h since the last one, then runs the existing covered-segment removal.
   Transient cost: the container swings to ~2x during a checkpoint, so the graph budget counts 2x.
   Also: request-scoped graph leases (not mount-scoped pins) so an idle engine hibernates and its heap
   is released. PR 1 proves live checkpoint is safe before anything else is built.
4. **LCM payloads.** GC runs for every registered store (cold-open through the executor; "mounted only"
   disappears); orphan files older than 24 h removed; raw rows drop at 180 d only if summarized, or
   at 180 d regardless once the file is over soft budget.
5. **Observation projection.** Payload release widens from "superseded/deleted + 30 d" to "projected and
   older than 60 d". `external_source_*`, temporal and sanitization tables get `RetentionRule`s
   (90 d, bounded chunks, `HasMore` re-run) where today they have no DELETE. Ledger rows of prior
   incarnations pruned at 48 h (the hook retry window), replacing "never".
6. **Generation staging.** Release cursor skips a failing projection into a quarantine row and moves on
   (the same poison-unit pattern as the drain), instead of head-of-line.
7. **`hook_analytics.jsonl`**: rotate at 8 MiB, keep 2. 8. **response-handles**: sweep each tick, not once
   at MCP build. 9. Maintenance inventory logs: delete older than 14 d.

Why retention was starving: 500 rows/day/kernel on mounted stores with no feedback. Rules now run in
chunks under `TurnBudgets::bulk` and return `HasMore`, so the 60 s retry cadence applies.

### 7. Schema evolution

Per-component table (sketch section 7): `Reset` components (LCM, temporal, observation, projection)
keep today's rule: mismatch means `wipe --stale` then `init`; `Migrate` components (memory_v2,
registry, Work/Workflow/handoff journals) get ordered forward migrations in one txn at open starting at
v40, because "memory survives indefinitely" is incompatible with reset-only. For a beta install the
accepted reset (`tracedecay wipe --stale` + `tracedecay init`) is sufficient; no data migration is
needed to adopt this design because no file is added, moved or renamed. The one new table family
(`projection_dead_letter`, `storage_samples`, `repo-key:` aliases) installs on existing files via the
existing component installer; they are `Reset`-class and append-only-safe to create.

### 8. Public port surface

Hide the engine: SQL text, WAL limits, DDL and the corruption string matcher leave
`tracedecay-store` (PR 10). Today: 19 store traits / 143 methods (3 sync: GitIndex 13, NativeIntegration
17, GraphPublicationStoreV1 12) + 24 sync Work/Workflow ports / 77 methods + the kernel
`PhysicalRuntimeAttachment` (11 methods, defaults "no port") + `ExactSqlHandle` sync API + serde wire
enums `RuntimeSubmitRequestV1` (15 variants) / `RuntimeReadRequestV1`. After:

| Public type | Methods | Note |
|---|---|---|
| 19 store traits | 143 + 3 dead-letter | all async |
| 24 Work/Workflow ports | 77 | all async, unchanged names |
| `StoreExecutor` | 6 | start, attach, footprints, block_on, govern_once, shutdown |
| `StoreHandle` | 3 | write, read, footprint |
| `StorageLedger` | 3 | sample, evaluate, health |
| `ProjectLifecycle` | 2 | init, retire |

Removed: `PhysicalRuntimeAttachment`, `ExactSqlHandle`, `RetainedExactSqlCapability`, sync
`dispatch_read`, `ReaderBudgetV1`, `AdmissionConfigV1` as a public knob. Honest depth verdict: the port
count is flat (220 methods before, 223 after) because I do not consolidate callers' ports. The new
surface is 14 methods; what disappears is ~6 kLOC of channel, lane, lease and actor machinery under it.
The typed wire enums remain internal fast paths in this wave (a deep shape deletes them; mine does not
need to for any incident).

### 9. Lifecycle

- **init**: resolve repository identity (marker, then aliases, then mint), register the worktree root,
  write the marker; creates no store file (first open does, on a pool worker).
- **wipe** (local): daemon-mediated `retire(Derived)`: one registry txn deletes `code_projects`,
  aliases, `graph_scopes`, then the daemon evicts stores, removes `projects/<id>/` except `tracedecay.db`
  when it holds memory rows, and deletes the markers it knows, so the next open does not silently
  recreate it. `wipe --all` stops the daemon first.
- **projects forget**: `retire(All)`; the only path that deletes memory.
- **doctor**: read-only `StorageLedger::health()` (every class, any size); `--apply` runs
  `govern_once` and the findings' actions through the daemon.
- **maintenance**: governor thread: 5 s settle/evict, 30 s fstat, 60 s sample; maintenance tick: hourly
  retention over **all** registered stores, daily orphan/superseded collection.
- **Profile deleted while the daemon runs** (the 112 GB case): every sample compares the pinned fd
  (`st_nlink == 0` or `(dev, ino)` differing from `stat(path)`) and re-reads `profile-identity.json`.
  `Unlinked` on a store: close its connections (the kernel frees the space) and reopen by path
  (recreating, as `wipe` intends). Identity file missing or `brain_id`/`profile_id` changed: the daemon
  answers `Unavailable{ProfileReplaced}`, closes everything and exits so its supervisor restarts it
  against the new profile. Detected within 30 s instead of never.

### 10. Cutover plan (riskiest unknown first; each PR deletes what it replaces)

Targets everywhere: clippy and rustfmt on touched crates, touched crates' unit tests, then the suites
named. `:unit_test` is run on every PR.

1. **Grafeo live checkpoint spike + footprint classifier.** Unknown: is `wal_checkpoint` on a serving
   `&GrafeoDB` safe under concurrent readers/writers, and what is the 2x swing? Adds the timer call and
   fixes the "other" family and the WAL-dir stat. Proves: stress test (writes + reads + checkpoint loop,
   assert WAL dir bounded, no wrong result). If it fails, stop: the "keep Grafeo" thesis is dead.
   Targets: graph-db unit tests, `:daemon_suite`.
2. **Checkpoint ladder + read deadlines.** `next_wal_phase`, read gate, idle TRUNCATE, `Stuck`; delete
   `hard_drain_required`, `BlockGeneral`, the 100 ms retry, supersedes #4097. Proves: reproduction of
   #4097 ends with WAL < 1 MiB in `gate_window`; `rusqlite-runtime` unit tests; `:session_suite`.
3. **Closure API + async `dispatch_read`.** `StoreHandle::{write,read}` on the existing threads; migrate
   the typed read callers (FactStore, ObservationStore, external-source, remote). Proves #4085:
   native reader-wait counter (from #4083) is 0 in `:mcp_suite` (was 2,683). Targets `:mcp_suite`,
   `:daemon_suite`, `:unit_test`.
4. **Flip Work/Workflow/git/native ports to async.** Mechanical; delete `off_native_thread` wrappers,
   `recv_writer_reply` parking, the sync `ExactSqlHandle` API and the two project actor threads. Proves:
   native writer-wait counter 0; `:daemon_suite`, `:mcp_suite`.
5. **Shared pool.** Turn-based writer, pooled readers, `scheduled` mailbox; delete per-store threads,
   health reader, deferred-return threads, lanes. Proves: thread count flat (test opens 64 stores,
   asserts <= W + R + 1 rusqlite threads); throughput not worse on the existing 4-shard benchmark.
   Riskiest rewrite, so it comes after its callers are closure-shaped.
6. **Fault supervisor + dead letter.** `ConnectionLedger`, canary, quarantine policy, per-stream parking,
   delete string matching in `tracedecay-store/projection.rs`. Proves: injected corrupt-like code on one
   connection resets without a dead letter; reproduced code parks one stream and the rest of the file
   projects; `:session_suite`.
7. **Governor.** Idle settle/evict, fstat inode check, profile-replaced exit, sample persistence.
   Proves: test unlinks a store dir under a running executor and observes `Detached` within one tick;
   64 idle stores reach 0 warm connections.
8. **Identity convergence + liveness + collectors.** Alias kind, root-commit key, marker write,
   `Superseded`, unregistered collector, retire-through-daemon, hooks no longer mkdir. Proves: fixture
   with three clones (path, https, ssh origins) resolves to one id; deleted-worktree store is collected;
   memory rows keep only `tracedecay.db`. Targets `:daemon_suite`, `:unit_test`.
9. **Ledger + retention.** `StorageLedger`, budgets, `RetentionRule`s, `HasMore`, unmounted-store
   retention, jsonl rotation, handle sweep. Proves: synthetic 3x-budget sessions file is pruned under
   budget in bounded ticks; capture refuses at hard with `Saturated`.
10. **Durability classes + migrations + leak removal.** `FileClass::durability`, `Migrate` components,
    move DDL/WAL constants out of `tracedecay-store`. Proves: crash-harness commit survives for
    `Journal`/`Control`; migration chain test v39 to v40.

### Module map (zero crates added, zero deleted)

Edited: `tracedecay-rusqlite-runtime` (executor, supervisor, WAL ladder in; `exact_sql` sync API ~3.4 kLOC,
`reader/` lanes and leases ~3.1 kLOC, health reader out; estimated +2 kLOC), `tracedecay-runtime-core`
(async `Connection` routes to `StoreHandle`; dead `project_code_open_runtime_budget` path out; layout
alias lookup), `tracedecay-store` (SQL/WAL/DDL/string matcher out; traits async), `tracedecay-contracts`
(24 ports async), `tracedecay-host-admission` (drain), `tracedecay-global-db` (dead-letter rows, retention
rules, component classes), `tracedecay-maintenance` (`StorageLedger`, orphan classifier),
`tracedecay-store-runtime` (lifecycle, registry cooldown), `tracedecay-graph-db` (periodic checkpoint),
`tracedecay-code-index-runtime` and `tracedecay-agent-hosts` (actor threads out, ~1.8 kLOC).
Merges were considered and rejected: crate shuffling closes no incident (see Alternatives).

## Synthesis decision

Left for the orchestrator.

## Tradeoffs accepted

- We accept Work/Workflow journals sharing a file with evidence in exchange for zero new files, no
  cross-file atomicity protocol and no new schema scope. The residual (journal loss on a physically
  corrupt `sessions.db`, or on a session-schema reset) has explicit triggers.
- We accept `Saturated{Budget}` on capture at the hard budget in exchange for a hard ceiling; host JSONL
  is not touched, so nothing is lost unless the host rotates the file first.
- We accept that a corrupt Sessions file is deleted and re-ingested (evidence older than the host's
  JSONL retention is lost) in exchange for no operator action and no backups.
- We accept that read closures need owned inputs and have a 30 s ceiling; long scans must page.
- We accept a 2x transient container size during Grafeo checkpoint, accounted in the graph budget.
- We accept that the port count does not shrink; depth is gained underneath, not at the ports.
- We accept placeholder budget numbers; the mechanism, not the digits, is the deliverable.
- Per-stream ordering, not per-file: a session behind a poison message waits for redrive.

## Alternatives considered

- **Class-per-file split (profile-wide or per-repository ADR-003 layout).** Judged on depth: it moves
  complexity to callers (outbox/watermark protocol across the log/projection cut, N installers, N
  collectors) to buy a failure domain whose only on-disk instance has not occurred. It does not fix the
  ratchet, the drain or growth. Rejected now; trigger rule in 2a keeps the door open.
- **Event log plus read models.** Depth is good at the read side, but it rewrites capture, projection,
  LCM and temporal in one move, and still needs this design's executor, checkpoint and budgets. A larger
  version of the same cutover.
- **Cap open stores (LRU) and keep thread-per-store.** Cheapest threads fix, but reads still park native
  workers (#4085), interactive txns still occupy a writer for up to 120 s, and evicting a hot 63 GB store
  pays WAL recovery on the next open. Rejected: it bounds threads but not the incident classes.
- **Replace Grafeo with SQLite graph tables.** Would remove heap residency and a checkpoint policy and
  match "one engine", but it is a rewrite of the code graph with no baseline measurement. Rejected for
  this wave; PR 9 measurements are the trigger.
- **Make Work/Workflow ports closure-taking services (`&mut Tx`) instead of async ports.** Deeper
  (one call per service method), but it rewrites 8 services and 70 construction sites. Flipping `async`
  closes #4085 with the same callers.

## Implementation reconciliation

None yet.

## Open questions and risks

- Is `GrafeoDB::wal_checkpoint` safe on a serving engine in the pinned fork? (PR 1; the whole Grafeo
  thesis rests on it. The fork source was not read.)
- Do any existing bulk writers legitimately exceed 2 s per txn (rebuild staging copies, vacuum,
  `rebuild_projection`)? If yes they move to `Maintenance` chunks before PR 5; what are the real numbers?
- Does a per-stream checkpoint fit `rebuild.rs`'s global-frontier assumptions? (Drain reads show a single
  `MIN(observation_sequence)` head and `checkpoint+1` rule; redrive per stream reuses the version-rebuild
  path. Not traced in depth.)
- Which long-lived readers exist outside a closure (doctor scans, `begin_read_snapshot` users)? They must
  page; the count decides PR 3's size.
- Should the journal split (2a) simply be taken now? It is cheap and I argue it is not yet earned. Your
  call; the executor makes either choice cost zero threads.
- Can graph owners drop their mount-scoped pin? I did not trace who holds it (open question in the
  grounding too), and `project_server_lifecycle.rs` was not read, so the #4100 grant-freeze closure is
  inferred from adjacency, not from its code.
- Is a shared root commit a safe identity for all forks and template-derived repos on this host?
- Biggest risk: PR 5, the shared-pool rewrite of a 3.3 kLOC writer loop with fairness, batching and
  ledger semantics. Mitigation: it lands last among the runtime PRs, behind closure-shaped callers, with
  the existing writer tests as its spec.

## Next implementation step

Write the Grafeo live-checkpoint stress test (PR 1) against the pinned fork, because its result decides
whether the rest of this shape stands.
